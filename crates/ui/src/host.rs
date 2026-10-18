//! The state the windows share with the daemon: the config, the model library and what the
//! daemon reports about itself, plus the way a window asks the daemon to act.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Global};
use tokativ_config::Config;
use tokativ_engine::ModelId;
use tokativ_engine::models::{self, Installed, RemoteFile, Repository};

/// What a window asks the daemon to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Toggle,
    Confirm,
    Cancel,
    Settings,
    Updates,
    Quit,
    /// A window is recording a new hotkey, so the current one has to let its presses through.
    Capturing(bool),
}

type Dispatch = Rc<dyn Fn(Action, &mut App)>;

/// The handles every window reaches through `Host::global(cx)`.
#[derive(Clone)]
pub struct Host {
    pub store: Entity<Store>,
    pub library: Entity<Library>,
    pub status: Entity<Status>,
    dispatch: Dispatch,
}

impl Global for Host {}

impl Host {
    pub fn install(
        store: Entity<Store>,
        library: Entity<Library>,
        status: Entity<Status>,
        dispatch: impl Fn(Action, &mut App) + 'static,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            store,
            library,
            status,
            dispatch: Rc::new(dispatch),
        });
    }

    pub fn global(cx: &App) -> Self {
        cx.global::<Self>().clone()
    }

    pub fn dispatch(&self, action: Action, cx: &mut App) {
        (self.dispatch)(action, cx);
    }
}

/// The config as the app currently runs it. Changes made here are written to disk at once, and
/// the daemon observes this entity to apply them.
pub struct Store {
    config: Config,
}

impl Store {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Edits the config and saves it. A failed save is logged and the change still applies for
    /// this run.
    pub fn update(&mut self, edit: impl FnOnce(&mut Config), cx: &mut Context<Self>) {
        let mut config = self.config.clone();
        edit(&mut config);
        if config == self.config {
            return;
        }
        if let Err(error) = config.save() {
            log::error!("config: cannot save: {error:#}");
        }
        self.config = config;
        cx.notify();
    }

    /// Takes a config read back from disk after an outside edit, without saving it again.
    pub fn replace(&mut self, config: Config, cx: &mut Context<Self>) {
        if config != self.config {
            self.config = config;
            cx.notify();
        }
    }
}

/// What the daemon reports for the windows to show.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The insertion method in use, by name.
    pub method: String,
    /// Why the method is not the one asked for, when it is not.
    pub note: Option<String>,
    pub recording: bool,
    /// The device the loaded model runs on, once one has loaded.
    pub device: Option<tokativ_engine::Device>,
    /// What the loaded model takes besides the audio, once one has loaded.
    pub abilities: Option<tokativ_engine::Abilities>,
}

/// Where one model stands on disk.
#[derive(Clone, Debug, PartialEq)]
pub enum ModelState {
    Missing,
    Downloading { done: u64, total: u64 },
    Installed { bytes: u64 },
    Failed(String),
}

/// The files of one repository, as the search panel lists them.
#[derive(Clone, Debug, PartialEq)]
pub enum Files {
    Loading,
    Loaded(Vec<RemoteFile>),
    Failed(String),
}

/// A search of Hugging Face and what came back.
#[derive(Default)]
pub struct Search {
    pub query: String,
    pub results: Vec<Repository>,
    pub loading: bool,
    pub error: Option<String>,
    /// The files of repositories the user opened, by repository.
    pub files: HashMap<String, Files>,
    /// Counts searches, so a slow answer to an old query is dropped.
    generation: u64,
}

/// The installed models, the downloads in flight, and searches of the hub. Downloads and
/// requests run off the main thread and report back here.
pub struct Library {
    installed: Vec<Installed>,
    downloads: HashMap<ModelId, ModelState>,
    cancels: HashMap<ModelId, Arc<AtomicBool>>,
    pub search: Search,
}

const PROGRESS_TICK: Duration = Duration::from_millis(120);

impl Library {
    pub fn new() -> Self {
        Self {
            installed: models::installed(),
            downloads: HashMap::new(),
            cancels: HashMap::new(),
            search: Search::default(),
        }
    }

    pub fn installed_models(&self) -> &[Installed] {
        &self.installed
    }

    pub fn state(&self, id: &ModelId) -> ModelState {
        if let Some(state) = self.downloads.get(id) {
            return state.clone();
        }
        match self.installed.iter().find(|model| &model.id == id) {
            Some(model) => ModelState::Installed { bytes: model.bytes },
            None => ModelState::Missing,
        }
    }

    /// The models being downloaded, and those whose download failed, so the list can show them
    /// before they are installed.
    pub fn fetching(&self) -> Vec<ModelId> {
        let mut ids: Vec<ModelId> = self
            .downloads
            .iter()
            .filter(|(_, state)| {
                matches!(
                    state,
                    ModelState::Downloading { .. } | ModelState::Failed(_)
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort_by_key(|id| id.name().to_lowercase());
        ids
    }

    pub fn installed(&self, id: &ModelId) -> bool {
        self.installed.iter().any(|model| &model.id == id)
    }

    pub fn any_installed(&self) -> bool {
        !self.installed.is_empty()
    }

    fn rescan(&mut self) {
        self.installed = models::installed();
    }

    pub fn download(&mut self, id: ModelId, cx: &mut Context<Self>) {
        if matches!(self.state(&id), ModelState::Downloading { .. }) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.insert(id.clone(), cancel.clone());
        self.downloads
            .insert(id.clone(), ModelState::Downloading { done: 0, total: 0 });
        cx.notify();

        let progress = Arc::new(Mutex::new((0u64, 0u64)));
        let reported = progress.clone();
        let (sender, receiver) = futures::channel::oneshot::channel();
        let fetched = id.clone();
        std::thread::Builder::new()
            .name("download".into())
            .spawn(move || {
                let result = models::download(
                    &fetched,
                    &|done, total| *reported.lock().expect("progress lock") = (done, total),
                    &cancel,
                );
                sender.send(result).ok();
            })
            .expect("spawning a download thread");

        cx.spawn(async move |this, cx| {
            let mut receiver = receiver;
            let result = loop {
                let tick = cx.background_executor().timer(PROGRESS_TICK);
                match futures::future::select(&mut receiver, tick).await {
                    futures::future::Either::Left((result, _)) => break result,
                    futures::future::Either::Right(_) => {
                        let (done, total) = *progress.lock().expect("progress lock");
                        let id = id.clone();
                        let updated = this.update(cx, |this, cx| {
                            this.downloads
                                .insert(id, ModelState::Downloading { done, total });
                            cx.notify();
                        });
                        if updated.is_err() {
                            return;
                        }
                    }
                }
            };
            this.update(cx, |this, cx| {
                this.cancels.remove(&id);
                this.downloads.remove(&id);
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) if error.is::<models::Cancelled>() => {}
                    Ok(Err(error)) => {
                        log::error!("models: {} download failed: {error:#}", id.key());
                        this.downloads
                            .insert(id, ModelState::Failed(format!("{error:#}")));
                    }
                    Err(_) => {
                        this.downloads
                            .insert(id, ModelState::Failed("the download thread died".into()));
                    }
                }
                this.rescan();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn cancel(&mut self, id: &ModelId) {
        if let Some(cancel) = self.cancels.get(id) {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn delete(&mut self, id: &ModelId, cx: &mut Context<Self>) {
        if let Err(error) = models::delete(id) {
            log::error!("models: cannot delete {}: {error:#}", id.key());
        }
        self.downloads.remove(id);
        self.rescan();
        cx.notify();
    }

    /// Searches the hub for `query`. An empty query clears the results.
    pub fn search(&mut self, query: String, cx: &mut Context<Self>) {
        self.search.generation += 1;
        let generation = self.search.generation;
        self.search.query = query.clone();
        self.search.error = None;
        if query.trim().is_empty() {
            self.search.results.clear();
            self.search.loading = false;
            cx.notify();
            return;
        }
        self.search.loading = true;
        cx.notify();
        let request = cx.background_spawn(async move { models::search(query.trim()) });
        cx.spawn(async move |this, cx| {
            let found = request.await;
            this.update(cx, |this, cx| {
                if this.search.generation != generation {
                    return;
                }
                this.search.loading = false;
                match found {
                    Ok(results) => this.search.results = results,
                    Err(error) => {
                        log::warn!("models: search failed: {error:#}");
                        this.search.results.clear();
                        this.search.error = Some(format!("{error:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Lists a repository's GGUF files, once, for choosing a quantization.
    pub fn open_repository(&mut self, repo: String, cx: &mut Context<Self>) {
        if matches!(
            self.search.files.get(&repo),
            Some(Files::Loading | Files::Loaded(_))
        ) {
            return;
        }
        self.search.files.insert(repo.clone(), Files::Loading);
        cx.notify();
        let listed = repo.clone();
        let request = cx.background_spawn(async move { models::repository_files(&listed) });
        cx.spawn(async move |this, cx| {
            let files = request.await;
            this.update(cx, |this, cx| {
                let files = match files {
                    Ok(files) => Files::Loaded(files),
                    Err(error) => Files::Failed(format!("{error:#}")),
                };
                this.search.files.insert(repo, files);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Default for Library {
    fn default() -> Self {
        Self::new()
    }
}

/// Creates the shared entities with the config loaded from disk.
pub fn entities(config: Config, cx: &mut App) -> (Entity<Store>, Entity<Library>, Entity<Status>) {
    (
        cx.new(|_| Store::new(config)),
        cx.new(|_| Library::new()),
        cx.new(|_| Status::default()),
    )
}
