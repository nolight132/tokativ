//! The background process: it owns the GPUI app, answers the control socket and the tray, and
//! runs one dictation session at a time.

use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, KeyBinding, QuitMode, WindowHandle};
use notify::{RecursiveMode, Watcher as _};
use tokativ_audio::{Levels, Options, Recorder, Silero, SpeechDetector, Timing};
use tokativ_config::{Capitals, Config, config_dir, config_path, socket_path};
use tokativ_engine::{Joiner, ModelId, Transcriber, restyle};
use tokativ_input::{Inserter, Key, Method};
use tokativ_ui::{Action, Caption, Host, Library, Phase, Pill, Status, Store};

use crate::assets::Assets;
use crate::autostart;
#[cfg(not(target_os = "linux"))]
use crate::hotkey::{Hotkeys, Pressed};
use crate::ipc::{self, Command};
use crate::tray::{Shown, Tray, TrayEvent};

/// What sits at the cursor while a segment is transcribed and no draft of it is ready.
const PLACEHOLDER: &str = "…";
/// How much new speech triggers another draft transcript.
const DRAFT_EVERY: Duration = Duration::from_millis(600);
/// How often the daemon looks whether a model finished loading on some device.
const DEVICE_POLL: Duration = Duration::from_secs(2);

/// Everything that reaches the daemon from another thread.
enum Event {
    Command(Command),
    Tray(TrayEvent),
    Recorder {
        session: u64,
        event: tokativ_audio::Event,
    },
    Transcribed {
        session: u64,
        result: Result<String>,
    },
    Drafted {
        session: u64,
        segment: usize,
        result: Result<String>,
    },
    Key(Key),
    ConfigFile,
    /// A file in the themes folder changed.
    ThemesFolder,
}

/// The pill window and where it was put, which decides whether the next session can reuse it.
struct Placed {
    handle: WindowHandle<Pill>,
    output: Option<String>,
    pill: tokativ_config::Pill,
}

impl Placed {
    fn fits(&self, output: &Option<String>, config: &Config) -> bool {
        &self.output == output && self.pill == config.pill
    }
}

/// One press of the hotkey to the pill closing.
struct Session {
    id: u64,
    engine: ModelId,
    /// The microphone, once it has opened.
    recorder: Option<Recorder>,
    /// Created at the first insertion, once the input method may have reported the text
    /// already left of the cursor.
    joiner: Option<Joiner>,
    /// Segments handed to the transcriber whose text has not come back yet.
    pending: usize,
    /// How many segments the recorder has finished, which is also the index of the one being
    /// spoken now.
    spoken: usize,
    /// How many segments have come back and been inserted.
    inserted: usize,
    /// The draft shown at the cursor and the segment it belongs to.
    draft: Option<(usize, String)>,
    /// The recorder may still deliver segments.
    capturing: bool,
    /// Confirm was pressed, so the session ends once the last segment is inserted.
    stopping: bool,
    /// The hotkey was pressed again while finishing, so a new session follows in the same pill.
    restart: bool,
    /// Speech was heard, so confirming has something to wait for.
    heard: bool,
    /// What the field held left of the cursor, read when the first segment went to the model.
    /// The input method reports it only once the field has taken it, so it is read once.
    surrounding: Option<String>,
    /// The text this session has inserted so far.
    written: String,
    /// Drafts show at the cursor as well as in the caption.
    at_cursor: bool,
}

impl Session {
    /// What the model is told with the next segment: the vocabulary, and the text that segment
    /// follows. Segments still being transcribed are not in it yet.
    fn context(
        &mut self,
        inserter: Option<&Inserter>,
        vocabulary: &[String],
    ) -> tokativ_engine::Context {
        let surrounding = self.surrounding.get_or_insert_with(|| {
            inserter
                .and_then(Inserter::before_cursor)
                .unwrap_or_default()
        });
        tokativ_engine::Context {
            vocabulary: vocabulary.to_vec(),
            before: format!("{surrounding}{}", self.written),
        }
    }
}

pub struct Daemon {
    store: Entity<Store>,
    library: Entity<Library>,
    status: Entity<Status>,
    applied: Config,
    events: UnboundedSender<Event>,
    transcriber: Transcriber,
    inserter: Option<Inserter>,
    session: Option<Session>,
    next_session: u64,
    /// The pill window, kept while it is on screen so a new session can unfold it again
    /// instead of opening another.
    pill: Option<Placed>,
    /// The caption beside the pill, while the session has words to show in it.
    caption: Option<WindowHandle<Caption>>,
    tray: Option<Tray>,
    tray_events: UnboundedSender<TrayEvent>,
    /// The toggle hotkey and, during a session, Escape and Enter. Linux leaves the toggle to the
    /// compositor and takes Escape and Enter through the input method instead.
    #[cfg(not(target_os = "linux"))]
    hotkeys: Option<Hotkeys>,
    _watcher: Option<notify::RecommendedWatcher>,
}

/// Starts the daemon and blocks until it quits.
pub fn run(config: Config) -> Result<()> {
    // The entry names the binary that wrote it, so an upgrade would leave it starting the old one.
    if config.launch_at_login
        && let Err(error) = autostart::apply(true)
    {
        log::error!("autostart: {error:#}");
    }
    // A second start is not a failure, so a service manager does not keep retrying it.
    if ipc::send(&socket_path(), &Command::Ping).is_ok() {
        log::info!("tokativ is already running");
        return Ok(());
    }
    let (events, receiver) = unbounded();
    let commands = events.clone();
    ipc::serve(&socket_path(), move |command| {
        commands.unbounded_send(Event::Command(command)).ok();
    })?;

    let application = gpui_platform::application().with_assets(Assets);
    application.run(move |cx: &mut App| {
        #[cfg(target_os = "macos")]
        keep_out_of_dock();
        cx.set_quit_mode(QuitMode::Explicit);
        if let Err(error) = Assets::load_fonts(cx) {
            log::error!("cannot load the UI fonts: {error:#}");
        }
        tokativ_ui::themes::init(&config.theme, cx);
        cx.bind_keys(bindings());

        let daemon = match Daemon::new(config, events, cx) {
            Ok(daemon) => daemon,
            Err(error) => {
                log::error!("{error:#}");
                cx.quit();
                return;
            }
        };
        listen(daemon, receiver, cx);
    });
    std::fs::remove_file(socket_path()).ok();
    Ok(())
}

/// Runs the daemon as an accessory app: no Dock icon and no menu bar of its own, while its
/// windows still open and take focus. GPUI makes every app a regular one at launch, so this has
/// to come after.
#[cfg(target_os = "macos")]
fn keep_out_of_dock() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if let Some(main_thread) = MainThreadMarker::new() {
        NSApplication::sharedApplication(main_thread)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

fn listen(daemon: Entity<Daemon>, mut receiver: UnboundedReceiver<Event>, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(event) = receiver.next().await {
            daemon.update(cx, |daemon, cx| daemon.handle(event, cx));
        }
    })
    .detach();
}

impl Daemon {
    fn new(config: Config, events: UnboundedSender<Event>, cx: &mut App) -> Result<Entity<Self>> {
        let (store, library, status) = tokativ_ui::entities(config.clone(), cx);
        let transcriber = Transcriber::start(idle_limit(&config), config.processor)?;
        let inserter = start_inserter(&config, &events);

        let (tray_events, mut tray_receiver) = unbounded();
        let forwarded = events.clone();
        cx.background_spawn(async move {
            while let Some(event) = tray_receiver.next().await {
                forwarded.unbounded_send(Event::Tray(event)).ok();
            }
        })
        .detach();

        let watched = events.clone();
        let watcher = watch_config(move |event| {
            watched.unbounded_send(event).ok();
        })
        .map_err(|error| log::warn!("config: not watching for edits: {error:#}"))
        .ok();

        #[cfg(not(target_os = "linux"))]
        let hotkeys = start_hotkeys(&config, &events);

        let daemon = cx.new(|cx| {
            let mut daemon = Self {
                store: store.clone(),
                library: library.clone(),
                status,
                applied: config.clone(),
                events,
                transcriber,
                inserter,
                session: None,
                next_session: 0,
                pill: None,
                caption: None,
                tray: None,
                tray_events,
                #[cfg(not(target_os = "linux"))]
                hotkeys,
                _watcher: watcher,
            };
            cx.observe(
                &store,
                |this: &mut Self, store: Entity<Store>, cx: &mut Context<Self>| {
                    let config = store.read(cx).config().clone();
                    this.apply(config, cx);
                },
            )
            .detach();
            cx.observe(&library, |this: &mut Self, _, cx| this.publish(cx))
                .detach();
            daemon.place_tray(config.tray, cx);
            // Models load on the worker thread, so the device they landed on is checked for
            // rather than announced.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(DEVICE_POLL).await;
                    let alive = this.update(cx, |this, cx| {
                        let status = this.status.read(cx);
                        if status.device != this.transcriber.device()
                            || status.abilities != this.transcriber.abilities()
                        {
                            this.publish(cx);
                        }
                    });
                    if alive.is_err() {
                        return;
                    }
                }
            })
            .detach();
            daemon.publish(cx);
            daemon.preload(cx);
            daemon
        });

        let weak = daemon.downgrade();
        Host::install(
            store,
            library.clone(),
            daemon.read(cx).status.clone(),
            move |action, cx| {
                weak.update(cx, |daemon, cx| daemon.act(action, cx)).ok();
            },
            cx,
        );
        tokativ_ui::updates::init(cx);

        // The pill waits folded from the start, so even the first session shows it at once. Other
        // systems cannot pass clicks through a waiting window, so they open the pill per session.
        #[cfg(target_os = "linux")]
        daemon.update(cx, |daemon, cx| {
            let config = daemon.store.read(cx).config().clone();
            let output = tokativ_input::focused_output();
            daemon.pill = daemon.open_pill(Levels::default(), &config, output, true, cx);
        });

        #[cfg(target_os = "macos")]
        crate::accessibility::recover(config.setup_done);
        let first_run = !config.setup_done || !library.read(cx).any_installed();
        if first_run {
            daemon.update(cx, |daemon, cx| daemon.open_setup(cx));
        }
        log::info!("tokativ is listening on {}", socket_path().display());
        Ok(daemon)
    }

    fn handle(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Command(command) => self.command(command, cx),
            Event::Tray(TrayEvent::Toggle) => self.toggle(cx),
            Event::Tray(TrayEvent::Model(model)) => self.set_model(model, cx),
            Event::Tray(TrayEvent::Profile(name)) => self.set_profile(name, cx),
            Event::Tray(TrayEvent::Settings) => self.open_settings(cx),
            #[cfg(not(target_os = "linux"))]
            Event::Tray(TrayEvent::Updates) => tokativ_ui::updates::open(cx),
            Event::Tray(TrayEvent::Quit) => self.quit(cx),
            Event::Recorder { session, event } => self.recorded(session, event, cx),
            Event::Transcribed { session, result } => self.transcribed(session, result, cx),
            Event::Drafted {
                session,
                segment,
                result,
            } => self.drafted(session, segment, result, cx),
            Event::Key(Key::Escape) => self.cancel(cx),
            Event::Key(Key::Enter) => self.confirm(cx),
            Event::ConfigFile => match Config::load() {
                Ok(config) => self.store.update(cx, |store, cx| store.replace(config, cx)),
                Err(error) => log::warn!("config: keeping the running settings: {error:#}"),
            },
            Event::ThemesFolder => tokativ_ui::themes::reload(cx),
        }
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        log::debug!("command: {command:?}");
        match command {
            Command::Toggle => self.toggle(cx),
            Command::Start => self.start(cx),
            Command::Stop | Command::Confirm => self.confirm(cx),
            Command::Cancel => self.cancel(cx),
            Command::Model(model) => self.set_model(model, cx),
            Command::Profile(name) => self.set_profile(name, cx),
            Command::Settings => self.open_settings(cx),
            Command::Quit => self.quit(cx),
            Command::Ping => {}
        }
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Toggle => self.toggle(cx),
            Action::Confirm => self.confirm(cx),
            Action::Cancel => self.cancel(cx),
            Action::Settings => self.open_settings(cx),
            Action::Updates => tokativ_ui::updates::open(cx),
            Action::Quit => self.quit(cx),
            #[cfg(not(target_os = "linux"))]
            Action::Capturing(capturing) => {
                if let Some(hotkeys) = &mut self.hotkeys {
                    hotkeys.pause(capturing);
                }
            }
            // The compositor owns the hotkey on Linux, and no window records one there.
            #[cfg(target_os = "linux")]
            Action::Capturing(_) => {}
        }
    }

    /// Starts a session, confirms the one running, or, while one is finishing, asks for a new
    /// one to follow it so the pill goes straight back to listening.
    fn toggle(&mut self, cx: &mut Context<Self>) {
        match &mut self.session {
            None => self.start(cx),
            Some(session) if !session.stopping => self.confirm(cx),
            Some(session) => {
                session.restart = !session.restart;
                let phase = match session.restart {
                    true => Phase::Listening,
                    false => Phase::Processing,
                };
                if let Some(pill) = self.pill.as_ref().map(|placed| placed.handle) {
                    set_phase(pill, phase, cx);
                }
            }
        }
    }

    /// Shows the pill at once and opens the microphone in the background, since a device can
    /// take a few hundred milliseconds to open. Speech before it opens is not heard.
    fn start(&mut self, cx: &mut Context<Self>) {
        if self.session.is_some() || tokativ_ui::updates::installing(cx) {
            return;
        }
        let config = self.store.read(cx).config().clone();
        let Some(engine) =
            ModelId::parse(&config.model).filter(|engine| self.library.read(cx).installed(engine))
        else {
            log::warn!("session: {} is not downloaded", config.model);
            self.open_setup(cx);
            return;
        };

        self.next_session += 1;
        let id = self.next_session;
        let levels = Levels::default();
        self.show_pill(levels.clone(), &config, cx);
        self.transcriber.preload(engine.clone());
        if let Some(inserter) = &self.inserter {
            inserter.begin();
        }
        #[cfg(not(target_os = "linux"))]
        if let Some(hotkeys) = &mut self.hotkeys {
            hotkeys.hold_session_keys(true);
        }
        log::info!("session {id}: listening with {}", engine.name());
        self.session = Some(Session {
            id,
            engine,
            recorder: None,
            joiner: None,
            pending: 0,
            spoken: 0,
            inserted: 0,
            draft: None,
            capturing: true,
            stopping: false,
            restart: false,
            heard: false,
            surrounding: None,
            written: String::new(),
            at_cursor: false,
        });
        self.publish(cx);

        // Drafts show as preedit text, which only the input method can put at the cursor, and in
        // the caption.
        let at_cursor = config.live_preview
            && self
                .inserter
                .as_ref()
                .is_some_and(|inserter| inserter.status().method == Method::InputMethod);
        if let Some(session) = &mut self.session {
            session.at_cursor = at_cursor;
        }
        let drafts = at_cursor || config.pill.caption;
        let options = Options {
            device: config.input_device.clone(),
            timing: Timing {
                threshold: config.vad.threshold,
                silence_ms: config.vad.silence_ms,
                max_segment_s: config.vad.max_segment_s,
                preroll_ms: config.vad.preroll_ms,
            },
            progress: drafts.then_some(DRAFT_EVERY),
        };
        let events = self.events.clone();
        let opening = cx.background_spawn(async move {
            let silero = Silero::new()?;
            Recorder::start(
                options,
                Some(Box::new(silero) as Box<dyn SpeechDetector>),
                levels,
                move |event| {
                    events
                        .unbounded_send(Event::Recorder { session: id, event })
                        .ok();
                },
            )
        });
        cx.spawn(async move |this, cx| {
            let opened = opening.await;
            this.update(cx, |daemon, cx| daemon.opened(id, opened, cx))
                .ok();
        })
        .detach();
    }

    /// Takes the microphone a session asked for. A session that was confirmed meanwhile stops it
    /// at once, and one that was cancelled drops it.
    fn opened(&mut self, id: u64, opened: Result<Recorder>, cx: &mut Context<Self>) {
        let session = self.session.as_mut().filter(|session| session.id == id);
        match (session, opened) {
            (Some(session), Ok(recorder)) => match session.stopping {
                true => recorder.stop(),
                false => session.recorder = Some(recorder),
            },
            (Some(_), Err(error)) => {
                log::error!("session {id}: cannot record: {error:#}");
                if let Some(pill) = self.pill.as_ref().map(|placed| placed.handle) {
                    set_phase(pill, Phase::Error, cx);
                }
                let session = self.session.take().expect("session checked above");
                self.close(session, false, cx);
            }
            (None, Ok(recorder)) => recorder.cancel(),
            (None, Err(_)) => {}
        }
    }

    /// Unfolds the waiting pill, or opens a new one when there is none or it sits on another
    /// output or at another place than the config now asks for.
    fn show_pill(&mut self, levels: Levels, config: &Config, cx: &mut Context<Self>) {
        // The pill outlives sessions, so it picks up a light or dark switch here. The portal also
        // answers late at login, after the waiting pill was first drawn.
        tokativ_ui::refresh_theme(cx);
        let output = tokativ_input::focused_output();
        if let Some(placed) = &self.pill
            && placed.fits(&output, config)
        {
            let reopened = placed.handle.update(cx, |pill, window, cx| {
                pill.reopen(levels.clone(), window, cx)
            });
            if reopened.is_ok() {
                return;
            }
        }
        self.pill = self.open_pill(levels, config, output, false, cx);
    }

    /// Opens a pill window, replacing any there was. `resting` opens it folded away.
    fn open_pill(
        &mut self,
        levels: Levels,
        config: &Config,
        output: Option<String>,
        resting: bool,
        cx: &mut Context<Self>,
    ) -> Option<Placed> {
        if let Some(old) = self.pill.take() {
            cx.defer(move |cx| {
                old.handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
        }
        tokativ_ui::refresh_theme(cx);
        let confirm = cx.entity().downgrade();
        let cancel = confirm.clone();
        let opened = tokativ_ui::pill::open(
            levels,
            &config.pill,
            move |cx| {
                confirm.update(cx, |daemon, cx| daemon.confirm(cx)).ok();
            },
            move |cx| {
                cancel.update(cx, |daemon, cx| daemon.cancel(cx)).ok();
            },
            resting,
            cx,
        );
        match opened {
            Ok(handle) => Some(Placed {
                handle,
                output,
                pill: config.pill.clone(),
            }),
            Err(error) => {
                log::error!("session: cannot show the pill: {error:#}");
                None
            }
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.stopping {
            return;
        }
        // Nothing was said, so there is nothing to wait for: close at once rather than waiting
        // for the microphone to open or flush an empty buffer.
        if !session.heard && session.pending == 0 {
            let mut session = self.session.take().expect("session checked above");
            if let Some(recorder) = session.recorder.take() {
                recorder.cancel();
            }
            log::info!("session {}: confirmed with nothing said", session.id);
            self.close(session, true, cx);
            return;
        }
        session.stopping = true;
        if let Some(recorder) = session.recorder.take() {
            recorder.stop();
        }
        if let Some(pill) = self.pill.as_ref().map(|placed| placed.handle) {
            set_phase(pill, Phase::Processing, cx);
        }
        log::info!("session {}: confirmed", session.id);
        self.finish_if_done(cx);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(mut session) = self.session.take() else {
            return;
        };
        if let Some(recorder) = session.recorder.take() {
            recorder.cancel();
        }
        log::info!("session {}: cancelled", session.id);
        self.close(session, true, cx);
    }

    fn recorded(&mut self, id: u64, event: tokativ_audio::Event, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut().filter(|session| session.id == id) else {
            return;
        };
        match event {
            tokativ_audio::Event::Started => {
                session.heard = true;
                log::debug!("session {id}: speech");
            }
            tokativ_audio::Event::Segment(segment) => {
                session.pending += 1;
                let index = session.spoken;
                session.spoken += 1;
                log::debug!(
                    "session {id}: segment of {:.1} s{}",
                    segment.samples.len() as f32 / tokativ_audio::RATE as f32,
                    if segment.forced {
                        ", cut at the limit"
                    } else {
                        ""
                    }
                );
                // The last draft of this segment stays up until its final text replaces it.
                let drafted = matches!(&session.draft, Some((segment, _)) if *segment == index);
                if !drafted {
                    session.draft = Some((index, PLACEHOLDER.into()));
                }
                if (!drafted || !session.at_cursor)
                    && let Some(inserter) = &self.inserter
                {
                    inserter.preedit(PLACEHOLDER.into());
                }
                let events = self.events.clone();
                let context = session.context(self.inserter.as_ref(), &self.applied.vocabulary);
                self.transcriber.transcribe(
                    session.engine.clone(),
                    segment.samples,
                    context,
                    move |result| {
                        events
                            .unbounded_send(Event::Transcribed {
                                session: id,
                                result,
                            })
                            .ok();
                    },
                );
                self.show_caption(cx);
            }
            tokativ_audio::Event::Progress(samples) => {
                let segment = session.spoken;
                let events = self.events.clone();
                let context = session.context(self.inserter.as_ref(), &self.applied.vocabulary);
                self.transcriber
                    .draft(session.engine.clone(), samples, context, move |result| {
                        events
                            .unbounded_send(Event::Drafted {
                                session: id,
                                segment,
                                result,
                            })
                            .ok();
                    });
            }
            tokativ_audio::Event::Failed(error) => {
                log::error!("session {id}: capture failed: {error}");
                if let Some(pill) = self.pill.as_ref().map(|placed| placed.handle) {
                    set_phase(pill, Phase::Error, cx);
                }
                let session = self.session.take().expect("session checked above");
                self.close(session, false, cx);
            }
            tokativ_audio::Event::Finished => {
                session.capturing = false;
                self.finish_if_done(cx);
            }
        }
    }

    fn transcribed(&mut self, id: u64, result: Result<String>, cx: &mut Context<Self>) {
        let style = self.applied.active_profile();
        let capitalize = style.capitals == Capitals::AsSpoken;
        let Some(session) = self.session.as_mut().filter(|session| session.id == id) else {
            return;
        };
        session.pending -= 1;
        let segment = session.inserted;
        session.inserted += 1;
        // A draft of a later segment stays up, and a placeholder while earlier ones are pending.
        let after = match &session.draft {
            Some((drafted, text)) if *drafted > segment => text.clone(),
            _ => {
                session.draft =
                    (session.pending > 0).then(|| (segment + 1, PLACEHOLDER.to_string()));
                session
                    .draft
                    .as_ref()
                    .map(|(_, text)| text.clone())
                    .unwrap_or_default()
            }
        };
        let text = match result {
            Ok(text) => text,
            Err(error) => {
                log::error!("session {id}: transcription failed: {error:#}");
                String::new()
            }
        };
        log::debug!("session {id}: {text:?}");
        // Without drafts at the cursor it only ever shows the placeholder.
        let at_cursor = match session.at_cursor {
            true => after,
            false if session.pending > 0 => PLACEHOLDER.to_string(),
            false => String::new(),
        };
        let before = self.inserter.as_ref().and_then(Inserter::before_cursor);
        let joiner = session
            .joiner
            .get_or_insert_with(|| Joiner::new(before.as_deref(), capitalize));
        let joined = joiner.join(&restyle(&text, style));
        if let Some(text) = &joined {
            session.written.push_str(text);
        }
        if let Some(inserter) = &self.inserter {
            match joined {
                Some(text) => inserter.insert(text, at_cursor),
                None => inserter.preedit(at_cursor),
            }
        }
        self.show_caption(cx);
        self.finish_if_done(cx);
    }

    /// Shows a draft of the segment being spoken, unless that segment has finished since.
    fn drafted(&mut self, id: u64, segment: usize, result: Result<String>, cx: &mut Context<Self>) {
        let style = self.applied.active_profile();
        let capitalize = style.capitals == Capitals::AsSpoken;
        let Some(session) = self.session.as_mut().filter(|session| session.id == id) else {
            return;
        };
        if segment != session.spoken || session.stopping {
            return;
        }
        let Ok(text) = result else {
            return;
        };
        let before = self.inserter.as_ref().and_then(Inserter::before_cursor);
        let joiner = session
            .joiner
            .get_or_insert_with(|| Joiner::new(before.as_deref(), capitalize));
        let Some(preview) = joiner.preview(&restyle(&text, style)) else {
            return;
        };
        log::debug!("session {id}: draft {preview:?}");
        session.draft = Some((segment, preview.clone()));
        if session.at_cursor
            && let Some(inserter) = &self.inserter
        {
            inserter.preedit(preview);
        }
        self.show_caption(cx);
    }

    /// Shows the session's text and the draft after it in the caption, opening the caption
    /// with the first words. Does nothing while the caption is turned off.
    fn show_caption(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let pill = self
            .pill
            .as_ref()
            .map(|placed| &placed.pill)
            .unwrap_or(&self.applied.pill);
        if !pill.caption {
            return;
        }
        let written = session.written.clone();
        let draft = session
            .draft
            .as_ref()
            .map(|(_, draft)| draft.clone())
            .unwrap_or_default();
        if self.caption.is_none() {
            if written.trim().is_empty() && draft.trim().is_empty() {
                return;
            }
            // A draft may arrive after the user has moved to another output. Keep its window
            // with the controls that opened for this session.
            let display = self.pill.as_ref().and_then(|placed| {
                placed
                    .handle
                    .update(cx, |_, window, cx| {
                        window.display(cx).map(|display| display.id())
                    })
                    .ok()
                    .flatten()
            });
            self.caption = tokativ_ui::caption::open(pill.edge, pill.margin, display, cx)
                .map_err(|error| log::error!("caption: cannot open: {error:#}"))
                .ok();
        }
        if let Some(caption) = self.caption {
            cx.defer(move |cx| {
                caption
                    .update(cx, |caption, _, cx| caption.show(&written, &draft, cx))
                    .ok();
            });
        }
    }

    fn finish_if_done(&mut self, cx: &mut Context<Self>) {
        let done = self
            .session
            .as_ref()
            .is_some_and(|session| session.stopping && !session.capturing && session.pending == 0);
        if !done {
            return;
        }
        let session = self.session.take().expect("session checked above");
        log::info!("session {}: done", session.id);
        let restart = session.restart;
        self.close(session, !restart, cx);
        if restart {
            self.start(cx);
        }
    }

    /// Ends a session's text insertion and, with `dismiss`, folds the pill away. A pill that
    /// shows an error or goes on to the next session stays.
    fn close(&mut self, session: Session, dismiss: bool, cx: &mut Context<Self>) {
        drop(session);
        if let Some(inserter) = &self.inserter {
            inserter.end();
        }
        #[cfg(not(target_os = "linux"))]
        if let Some(hotkeys) = &mut self.hotkeys {
            hotkeys.hold_session_keys(false);
        }
        if dismiss && let Some(pill) = self.pill.as_ref().map(|placed| placed.handle) {
            cx.defer(move |cx| {
                pill.update(cx, |pill, _, cx| pill.dismiss(cx)).ok();
            });
        }
        // The next session starts its caption empty.
        if let Some(caption) = self.caption.take() {
            cx.defer(move |cx| {
                caption
                    .update(cx, |caption, _, cx| caption.dismiss(cx))
                    .ok();
            });
        }
        self.publish(cx);
    }

    /// Switches the model sessions dictate with, keeping the choice in the config. A model that
    /// is not downloaded opens the models in the settings window instead.
    fn set_model(&mut self, model: String, cx: &mut Context<Self>) {
        let installed =
            ModelId::parse(&model).is_some_and(|id| self.library.read(cx).installed(&id));
        if !installed {
            log::warn!("model: {model} is not downloaded");
            self.open_setup(cx);
            return;
        }
        self.store.update(cx, |store, cx| {
            store.update(|config| config.model = model, cx)
        });
    }

    /// Writes text in the profile with this name from the next phrase on.
    fn set_profile(&mut self, name: String, cx: &mut Context<Self>) {
        let known = self
            .store
            .read(cx)
            .config()
            .profiles
            .iter()
            .any(|profile| profile.name == name);
        if !known {
            log::warn!("profile: there is no profile called {name:?}");
            return;
        }
        self.store.update(cx, |store, cx| {
            store.update(|config| config.profile = name, cx)
        });
    }

    /// Applies a changed config. Session settings take effect from the next session.
    fn apply(&mut self, config: Config, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.applied, config.clone());
        if old.processor != config.processor {
            self.transcriber.set_processor(config.processor);
        }
        if old.model != config.model || old.processor != config.processor {
            log::info!("model: {} on {:?}", config.model, config.processor);
            self.preload(cx);
        }
        if old.insertion.backend != config.insertion.backend {
            self.inserter = start_inserter(&config, &self.events);
        } else if old.insertion.terminals != config.insertion.terminals
            && let Some(inserter) = &self.inserter
        {
            inserter.set_terminals(config.insertion.terminals.clone());
        }
        if old.theme != config.theme {
            tokativ_ui::themes::select(&config.theme, cx);
        }
        if old.tray != config.tray {
            self.place_tray(config.tray, cx);
        }
        #[cfg(not(target_os = "linux"))]
        if old.hotkey != config.hotkey
            && let Some(hotkeys) = &mut self.hotkeys
            && let Err(error) = hotkeys.set_toggle(&config.hotkey)
        {
            log::error!("hotkey: {error:#}");
        }
        if old.launch_at_login != config.launch_at_login
            && let Err(error) = autostart::apply(config.launch_at_login)
        {
            log::error!("autostart: {error:#}");
        }
        if old.models != config.models {
            self.transcriber.set_idle_limit(idle_limit(&config));
        }
        self.publish(cx);
    }

    fn preload(&self, cx: &App) {
        let config = self.store.read(cx).config();
        if config.models.unload_after_minutes != 0 {
            return;
        }
        if let Some(id) = ModelId::parse(&config.model)
            && self.library.read(cx).installed(&id)
        {
            self.transcriber.preload(id);
        }
    }

    /// Shows or hides the tray item. A sandboxed daemon never shows one, so the panel keeps a
    /// single item for the real install.
    fn place_tray(&mut self, placed: bool, cx: &App) {
        self.tray = match placed && tokativ_config::sandbox().is_none() {
            false => None,
            true => Tray::spawn(
                self.tray_events.clone(),
                self.session.is_some(),
                self.shown(cx),
            )
            .map_err(|error| log::warn!("tray: {error:#}"))
            .ok(),
        };
    }

    /// The models and profiles as the tray lists them.
    fn shown(&self, cx: &App) -> Shown {
        let library = self.library.read(cx);
        let config = self.store.read(cx).config();
        Shown {
            model: config.model.clone(),
            profile: config.active_profile().name.clone(),
            profiles: config
                .profiles
                .iter()
                .map(|profile| profile.name.clone())
                .collect(),
            installed: library
                .installed_models()
                .iter()
                .map(|model| (model.id.key(), model.id.name()))
                .collect(),
        }
    }

    /// Pushes the daemon's state to the windows and the tray.
    fn publish(&mut self, cx: &mut Context<Self>) {
        let recording = self.session.is_some();
        let shown = self.shown(cx);
        let device = self.transcriber.device();
        let abilities = self.transcriber.abilities();
        let (method, note) = match &self.inserter {
            Some(inserter) => {
                let status = inserter.status();
                (status.method.name().to_string(), status.note)
            }
            None => (
                "Unavailable".to_string(),
                Some("text cannot be sent to other apps".to_string()),
            ),
        };
        self.status.update(cx, |status, cx| {
            *status = Status {
                method,
                note,
                recording,
                device,
                abilities,
            };
            cx.notify();
        });
        if let Some(tray) = &mut self.tray {
            tray.show(recording, shown);
        }
    }

    fn open_settings(&mut self, cx: &mut Context<Self>) {
        tokativ_ui::refresh_theme(cx);
        tokativ_ui::settings::open(cx);
    }

    fn open_setup(&mut self, cx: &mut Context<Self>) {
        tokativ_ui::refresh_theme(cx);
        tokativ_ui::settings::open_models(cx);
    }

    fn quit(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
        self.tray = None;
        cx.quit();
    }
}

/// Changes the pill's phase once the current update is done, since the change may come from a
/// click inside the pill itself and GPUI cannot update a window that is mid-update.
fn set_phase(pill: WindowHandle<Pill>, phase: Phase, cx: &mut App) {
    cx.defer(move |cx| {
        pill.update(cx, |pill, window, cx| pill.set_phase(phase, window, cx))
            .ok();
    });
}

/// Registers the toggle hotkey, with every press fed back as an event. Without it the tray and
/// the command line still start sessions.
#[cfg(not(target_os = "linux"))]
fn start_hotkeys(config: &Config, events: &UnboundedSender<Event>) -> Option<Hotkeys> {
    let events = events.clone();
    Hotkeys::new(&config.hotkey, move |pressed| {
        let event = match pressed {
            Pressed::Toggle => Event::Command(Command::Toggle),
            Pressed::Key(key) => Event::Key(key),
        };
        events.unbounded_send(event).ok();
    })
    .map_err(|error| log::error!("hotkey: {error:#}"))
    .ok()
}

/// Connects the text insertion, with Escape and Enter fed back as events.
fn start_inserter(config: &Config, events: &UnboundedSender<Event>) -> Option<Inserter> {
    let keys = events.clone();
    Inserter::start(
        config.insertion.backend,
        config.insertion.terminals.clone(),
        move |key| {
            keys.unbounded_send(Event::Key(key)).ok();
        },
    )
    .map_err(|error| log::error!("text insertion is unavailable: {error:#}"))
    .ok()
}

fn idle_limit(config: &Config) -> Option<Duration> {
    match config.models.unload_after_minutes {
        0 => None,
        minutes => Some(Duration::from_secs(minutes as u64 * 60)),
    }
}

/// Watches the config folder and the themes folder inside it, and calls `changed` with
/// [`Event::ConfigFile`] whenever the config file is written and with [`Event::ThemesFolder`]
/// whenever a theme file is added, written or removed. The folders are watched rather than the
/// files, since saving replaces a file.
fn watch_config(changed: impl Fn(Event) + Send + 'static) -> Result<notify::RecommendedWatcher> {
    let directory = config_dir();
    let themes = tokativ_ui::themes::folder();
    std::fs::create_dir_all(&themes).with_context(|| format!("creating {}", themes.display()))?;
    let path = config_path();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if event.kind.is_access() {
            return;
        }
        let config = event.paths.iter().any(|touched| touched == &path);
        if config && (event.kind.is_create() || event.kind.is_modify()) {
            changed(Event::ConfigFile);
        }
        let theme = event.paths.iter().any(|touched| {
            touched.parent() == Some(themes.as_path())
                && touched
                    .extension()
                    .is_some_and(|extension| extension == "json")
        });
        if theme {
            changed(Event::ThemesFolder);
        }
    })
    .context("starting the config watcher")?;
    watcher
        .watch(&directory, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", directory.display()))?;
    Ok(watcher)
}

/// The editing keys for the text fields in the setup and settings windows.
fn bindings() -> Vec<KeyBinding> {
    use ui::*;
    let editing = Some(INPUT_CONTEXT);
    vec![
        KeyBinding::new("backspace", Backspace, editing),
        KeyBinding::new("ctrl-backspace", BackspaceWord, editing),
        KeyBinding::new("delete", Delete, editing),
        KeyBinding::new("ctrl-delete", DeleteWord, editing),
        KeyBinding::new("left", Left, editing),
        KeyBinding::new("right", Right, editing),
        KeyBinding::new("ctrl-left", WordLeft, editing),
        KeyBinding::new("ctrl-right", WordRight, editing),
        KeyBinding::new("shift-left", SelectLeft, editing),
        KeyBinding::new("shift-right", SelectRight, editing),
        KeyBinding::new("shift-ctrl-left", SelectWordLeft, editing),
        KeyBinding::new("shift-ctrl-right", SelectWordRight, editing),
        KeyBinding::new("home", Home, editing),
        KeyBinding::new("end", End, editing),
        KeyBinding::new("shift-home", SelectHome, editing),
        KeyBinding::new("shift-end", SelectEnd, editing),
        KeyBinding::new("ctrl-a", SelectAll, editing),
        KeyBinding::new("ctrl-v", Paste, editing),
        KeyBinding::new("ctrl-c", Copy, editing),
        KeyBinding::new("ctrl-x", Cut, editing),
        KeyBinding::new("space", Space, editing),
        KeyBinding::new("escape", Dismiss, editing),
    ]
}
