//! The settings window, laid out the way Sonora lays out its settings: a search field and a bar
//! of categories float over one column of rows, which scrolls beneath them through a blur. Every
//! control writes the config through [`Store`], and the daemon applies the change from there.

use gpui::Focusable as _;
use gpui::prelude::*;
use std::ops::Range;

use gpui::{
    AnyElement, App, Context, Entity, Global, Hsla, Pixels, ScrollHandle, SharedString, Size, Task,
    UniformListScrollHandle, Window, WindowHandle, div, px, svg, uniform_list,
};
use tokativ_config::{Backend, Capitals, Config, Periods, PillEdge, Processor};
use tokativ_engine::{Hardware, ModelId};
use ui::{
    ActiveTheme as _, Button, Entrance, Input, Menu, MenuItem, Perch, Picker, Popovers, Radio,
    Scrollbar, Scroller, Scrubber, ScrubberState, Separator, Switch, TabBar, Text, Tipped as _,
    Vacancy, eyebrow, veiled,
};

use crate::chrome::{self, veil};
use crate::host::{Files, Host, Library, ModelState, Status, Store};
use crate::models;
use crate::setup::Setup;
use crate::themes;
use crate::usage::{Sampler, Usage};

const WIDTH: f32 = 760.;
const HEIGHT: f32 = 720.;
/// How wide the column of rows, the search field and the category bar grow at most.
const COLUMN: Pixels = px(640.);
/// How far the rows are blurred where they pass under the header.
const HEADER_BLUR: Pixels = px(1.);
/// How far past the header the rows keep dissolving, so sharp rows never meet the haze at a
/// hard edge.
const HEADER_FADE_TAIL: Pixels = px(48.);
/// How far a clickable row's hover shade reaches past the column on either side.
const HOVER_REACH: Pixels = px(10.);
/// How much a query found in a setting's title counts over one found in its detail.
const TITLE_WEIGHT: u32 = 2;
/// The ranges the sliders cover.
const SILENCE_MS: (f32, f32) = (200., 2000.);
const THRESHOLD: (f32, f32) = (0.2, 0.9);
const MARGIN: (f32, f32) = (0., 240.);
const IDLE_MINUTES: [u32; 5] = [0, 5, 15, 30, 60];
/// How often the resources rows read the daemon's footprint.
const SAMPLE_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// The tallest the microphone menu grows before it scrolls, about seven rows.
const DEVICE_MENU_HEIGHT: Pixels = px(300.);
/// How long typing must pause before a model search goes out.
const SEARCH_PAUSE: std::time::Duration = std::time::Duration::from_millis(350);

/// Names the page the settings window opens on, for working on one page. `find` opens the
/// model search.
pub const PAGE_VARIABLE: &str = "TOKATIV_SETTINGS_PAGE";

struct Open(WindowHandle<Settings>);

impl Global for Open {}

/// Opens the settings window, or raises it when it is already open.
pub fn open(cx: &mut App) {
    // An accessory app on macOS stays behind the app in front unless it asks to come forward.
    #[cfg(target_os = "macos")]
    cx.activate(true);
    if let Some(handle) = cx.try_global::<Open>().map(|open| open.0)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = chrome::options("Tokativ", Size::new(px(WIDTH), px(HEIGHT)), cx);
    match cx.open_window(options, |window, cx| {
        window.set_rem_size(cx.theme().font_size);
        cx.new(|cx| {
            let mut settings = Settings::new(window, cx);
            match std::env::var(PAGE_VARIABLE).ok().as_deref() {
                Some("find") => settings.explore(true, window, cx),
                Some(id) => {
                    if let Some(page) = Page::from_id(id) {
                        settings.select(page, cx);
                    }
                }
                None => {}
            }
            settings.entrance = None;
            settings
        })
    }) {
        Ok(handle) => cx.set_global(Open(handle)),
        Err(error) => log::error!("settings: cannot open the window: {error:#}"),
    }
}

/// Opens the settings window on the models, for a session that found its model missing. A setup
/// that is not finished still shows first.
pub fn open_models(cx: &mut App) {
    open(cx);
    if let Some(handle) = cx.try_global::<Open>().map(|open| open.0) {
        handle
            .update(cx, |settings, _, cx| settings.select(Page::Model, cx))
            .ok();
    }
}

/// A category of settings, one per entry in the bar under the search field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Model,
    Dictation,
    Style,
    General,
}

impl Page {
    const ALL: [Self; 4] = [Self::General, Self::Model, Self::Dictation, Self::Style];

    /// The name `TOKATIV_SETTINGS_PAGE` takes.
    fn id(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Dictation => "dictation",
            Self::Style => "style",
            Self::General => "general",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "typing" => Some(Self::Dictation),
            "pill" | "system" => Some(Self::General),
            _ => Self::ALL.into_iter().find(|page| page.id() == id),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Model => "Model",
            Self::Dictation => "Dictation",
            Self::Style => "Writing style",
            Self::General => "General",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Model => "icons/library-big.svg",
            Self::Dictation => "icons/mic-vocal.svg",
            Self::Style => "icons/pencil.svg",
            Self::General => "icons/settings.svg",
        }
    }
}

/// One entry of the column, described without building it. A search scores slots by their
/// title and detail and only builds the ones that match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// A group label over the rows that follow it.
    Title(&'static str),
    /// The label over the rows that edit the profile in use, which carries its name.
    ProfileTitle,
    /// The hairline between two rows.
    Sep,
    Models,
    Explore,
    Runtime,
    Vocabulary,
    LiveText,
    Hotkey,
    Microphone,
    Pause,
    Threshold,
    Method,
    Profiles,
    Capitals,
    Periods,
    SpokenEmoji,
    Laughter,
    Replacements,
    Theme,
    CustomThemes,
    Position,
    Distance,
    Caption,
    CancelButton,
    ConfirmButton,
    Login,
    Tray,
    Unload,
    AutoUpdates,
    Updates,
    /// What the app and its model take, as a row of readings.
    Resources,
    Version,
}

/// A slider whose value is shown while it is dragged and saved once it is let go.
struct Slider {
    state: ScrubberState,
    dragging: Option<f32>,
}

impl Slider {
    fn new(id: &str) -> Self {
        Self {
            state: ScrubberState::new(id.to_owned()),
            dragging: None,
        }
    }
}

pub struct Settings {
    /// The first run, shown in place of the settings until it is finished.
    setup: Option<Entity<Setup>>,
    store: Entity<Store>,
    library: Entity<Library>,
    status: Entity<Status>,
    popovers: Popovers,
    silence: Slider,
    threshold: Slider,
    margin: Slider,
    devices: Vec<tokativ_audio::InputDevice>,
    scrollbar: Entity<Scrollbar>,
    device_scrollbar: Entity<Scrollbar>,
    usage: Usage,
    /// The graphics cards and the processor with their memory, once listed, so the readings
    /// can show how much of it is taken.
    hardware: Option<Vec<Hardware>>,
    _sampling: Task<()>,
    _listing: Task<()>,
    /// The field over the column that searches the settings themselves.
    search: Entity<Input>,
    /// The settings search's text, trimmed. Empty means the column shows one category.
    query: String,
    /// The field that searches Hugging Face for models.
    model_search: Entity<Input>,
    #[cfg(not(target_os = "linux"))]
    hotkey: Entity<crate::hotkey::Recorder>,
    replace_from: Entity<Input>,
    replace_to: Entity<Input>,
    term: Entity<Input>,
    profile_name: Entity<Input>,
    /// The model query last sent, so a notify that did not change the text does not search
    /// again.
    searched: String,
    /// The model search is showing in place of the model category.
    exploring: bool,
    /// The search result whose files are listed.
    opened: Option<String>,
    page: Page,
    /// How tall the header floating over the column measured last, so the rows start beneath it.
    header_height: Pixels,
    /// Whether the header has measured itself. Until then the column stays hidden rather than
    /// flashing unpadded for a frame.
    header_measured: bool,
    /// The entrance the column plays after a change of category.
    entrance: Option<Entrance>,
    results_scroll: UniformListScrollHandle,
    results_scrollbar: Entity<Scrollbar>,
    files_scroll: UniformListScrollHandle,
    files_scrollbar: Entity<Scrollbar>,
    _debounce: Option<Task<()>>,
}

impl Settings {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = cx.entity_id();
        let host = Host::global(cx);
        cx.observe(&host.store, |this: &mut Self, store, cx| {
            // Finishing the setup hands the window over to the settings.
            if this.setup.is_some() && store.read(cx).config().setup_done {
                this.setup = None;
                this.page = Page::General;
                this.reveal(cx);
            }
            cx.notify();
        })
        .detach();
        let setup = (!host.store.read(cx).config().setup_done)
            .then(|| cx.new(|cx| Setup::begin(window, cx)));
        cx.observe(&host.library, |_, _, cx| cx.notify()).detach();
        cx.observe(&host.status, |_, _, cx| cx.notify()).detach();
        let search = cx.new(|cx| {
            Input::new("Search settings", cx)
                .icon("icons/search.svg")
                .clearable()
                .blurred()
        });
        cx.observe(&search, |this: &mut Self, input, cx| {
            let query = input.read(cx).text().trim().to_owned();
            // A search lists rows from every category, so wherever the column was scrolled to
            // answers for nothing once one starts.
            if this.query.is_empty() && !query.is_empty() {
                this.scroll_to_top(cx);
            }
            this.query = query;
            cx.notify();
        })
        .detach();
        let model_search =
            cx.new(|cx| Input::new("Search Hugging Face", cx).icon("icons/search.svg"));
        cx.observe(&model_search, |this: &mut Self, _, cx| {
            this.model_query_changed(cx)
        })
        .detach();
        let results_scroll = UniformListScrollHandle::new();
        let files_scroll = UniformListScrollHandle::new();
        cx.observe_window_appearance(window, |_, _, cx| {
            crate::refresh_theme(cx);
            cx.notify();
        })
        .detach();
        Self {
            setup,
            store: host.store,
            library: host.library,
            status: host.status,
            popovers: Popovers::default(),
            silence: Slider::new("silence"),
            threshold: Slider::new("threshold"),
            margin: Slider::new("margin"),
            devices: tokativ_audio::input_devices(),
            scrollbar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(view)),
            device_scrollbar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(view)),
            usage: Usage::default(),
            hardware: None,
            search,
            query: String::new(),
            model_search,
            #[cfg(not(target_os = "linux"))]
            hotkey: cx.new(crate::hotkey::Recorder::new),
            replace_from: cx.new(|cx| Input::new("Heard as", cx).compact()),
            replace_to: cx.new(|cx| Input::new("Write as", cx).compact()),
            term: cx.new(|cx| Input::new("Add a name or term", cx).compact()),
            profile_name: cx.new(|cx| Input::new("Profile name", cx).compact()),
            searched: String::new(),
            exploring: false,
            opened: None,
            page: Page::General,
            header_height: Pixels::ZERO,
            header_measured: false,
            entrance: None,
            results_scrollbar: cx.new(|_| {
                Scrollbar::new(results_scroll.0.borrow().base_handle.clone()).watching(view)
            }),
            results_scroll,
            files_scrollbar: cx.new(|_| {
                Scrollbar::new(files_scroll.0.borrow().base_handle.clone()).watching(view)
            }),
            files_scroll,
            _debounce: None,
            _listing: {
                let listing = cx.background_spawn(async { tokativ_engine::hardware() });
                cx.spawn(async move |this, cx| {
                    let hardware = listing.await;
                    this.update(cx, |this, cx| {
                        this.hardware = Some(hardware);
                        cx.notify();
                    })
                    .ok();
                })
            },
            _sampling: cx.spawn(async move |this, cx| {
                let mut sampler = Sampler::default();
                loop {
                    let usage = sampler.sample();
                    let open = this.update(cx, |this, cx| {
                        this.usage = usage;
                        cx.notify();
                    });
                    if open.is_err() {
                        return;
                    }
                    cx.background_executor().timer(SAMPLE_EVERY).await;
                }
            }),
        }
    }

    fn edit(&self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Config)) {
        self.store.update(cx, |store, cx| store.update(edit, cx));
    }

    fn searching(&self) -> bool {
        !self.query.is_empty()
    }

    /// Shows one category and ends any search or model search, so a click on the bar always
    /// lands on a plain page.
    fn select(&mut self, page: Page, cx: &mut Context<Self>) {
        let moved = self.page != page || self.exploring || self.searching();
        self.page = page;
        self.exploring = false;
        self.popovers.close();
        if !self.search.read(cx).text().is_empty() {
            self.search.update(cx, |input, cx| input.set_text("", cx));
        }
        if moved {
            self.scroll_to_top(cx);
            self.reveal(cx);
        }
        cx.notify();
    }

    /// Swaps the model category for the model search, or back.
    fn explore(&mut self, exploring: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Model;
        self.exploring = exploring;
        self.opened = None;
        if !self.search.read(cx).text().is_empty() {
            self.search.update(cx, |input, cx| input.set_text("", cx));
        }
        self.scroll_to_top(cx);
        self.reveal(cx);
        if exploring {
            let focus = self.model_search.focus_handle(cx);
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn scroll_to_top(&self, cx: &mut Context<Self>) {
        self.scrollbar.update(cx, |bar, _| bar.place(Pixels::ZERO));
    }

    /// Plays the entrance over the column, the way Sonora brings in a page.
    fn reveal(&mut self, cx: &mut Context<Self>) {
        self.entrance = (!cx.reduce_motion()).then(Entrance::start);
    }

    /// How much of the column the entrance still hides, from 1 as it starts to 0.
    fn hidden(&mut self, window: &mut Window, cx: &App) -> f32 {
        let Some(entrance) = self.entrance.filter(|_| !cx.reduce_motion()) else {
            self.entrance = None;
            return 0.;
        };
        if !entrance.running() {
            self.entrance = None;
            return 0.;
        }
        window.request_animation_frame();
        entrance.hidden()
    }

    /// Takes the header's measured height. The rows pad themselves by it and scroll beneath
    /// the header, and the scrollbar starts below it so the bar never slides under the blur.
    fn set_header_height(&mut self, height: Pixels, cx: &mut Context<Self>) {
        if self.header_height == height && self.header_measured {
            return;
        }
        self.header_height = height;
        self.header_measured = true;
        self.scrollbar.update(cx, |bar, cx| {
            bar.set_track_top(height, cx);
        });
        cx.notify();
    }

    /// The slots of one category, group labels included.
    fn page_slots(&self, page: Page, _cx: &App) -> Vec<Slot> {
        match page {
            Page::Model => vec![
                Slot::Models,
                Slot::Explore,
                Slot::Runtime,
                Slot::Title("Vocabulary"),
                Slot::Vocabulary,
            ],
            Page::Dictation => [
                match cfg!(target_os = "linux") {
                    true => Slot::LiveText,
                    false => Slot::Hotkey,
                },
                Slot::Microphone,
                Slot::Title("Voice detection"),
                Slot::Pause,
                Slot::Threshold,
                Slot::Title("Typing"),
                Slot::Method,
            ]
            .to_vec(),
            Page::Style => vec![
                Slot::Profiles,
                Slot::ProfileTitle,
                Slot::Capitals,
                Slot::Periods,
                Slot::SpokenEmoji,
                Slot::Laughter,
                Slot::Replacements,
            ],
            Page::General => [
                Slot::Title("Appearance"),
                Slot::Theme,
                Slot::CustomThemes,
                Slot::Title("Pill"),
                Slot::Position,
                Slot::Distance,
                Slot::Caption,
                Slot::CancelButton,
                Slot::ConfirmButton,
                Slot::Title("System"),
                Slot::Login,
                Slot::Tray,
                Slot::Unload,
            ]
            .into_iter()
            .chain(
                [Slot::AutoUpdates, Slot::Updates]
                    .into_iter()
                    .filter(|_| cfg!(not(target_os = "linux"))),
            )
            .chain([
                Slot::Title("Resources"),
                Slot::Resources,
                Slot::Title("About"),
                Slot::Version,
            ])
            .collect(),
        }
    }

    /// Every setting of every category that answers the query, best match first. Ties keep the
    /// order of the categories.
    fn found(&self, cx: &App) -> Vec<Slot> {
        let query = self.query.to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut hits: Vec<(u32, Slot)> = Page::ALL
            .into_iter()
            .flat_map(|page| self.page_slots(page, cx))
            .filter_map(|slot| {
                let (title, detail) = self.meta(slot, cx)?;
                let title = title.to_lowercase();
                let both = format!("{title} {}", detail.unwrap_or_default().to_lowercase());
                words.iter().all(|word| both.contains(word)).then(|| {
                    let titled = words.iter().filter(|word| title.contains(*word)).count();
                    (1 + titled as u32 * TITLE_WEIGHT, slot)
                })
            })
            .collect();
        hits.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        hits.into_iter().map(|(_, slot)| slot).collect()
    }

    /// The title and detail a slot shows and a search matches. Labels and hairlines have none,
    /// so a search never lists them.
    fn meta(&self, slot: Slot, cx: &App) -> Option<(SharedString, Option<SharedString>)> {
        let status = self.status.read(cx);
        let pair = |title: &'static str, detail: Option<String>| {
            Some((SharedString::from(title), detail.map(SharedString::from)))
        };
        let plain = |title: &'static str, detail: &'static str| pair(title, Some(detail.into()));
        match slot {
            Slot::Title(_) | Slot::ProfileTitle | Slot::Sep => None,
            Slot::Models => plain(
                "Models",
                "The installed models. Clicking one makes it the one sessions dictate with.",
            ),
            Slot::Explore => plain(
                "Explore more models",
                "Search Hugging Face for Whisper, Canary, Voxtral and other models that run here.",
            ),
            Slot::Runtime => pair(
                "Runtime",
                Some(match &status.device {
                    Some(device) => format!("Running on {}.", device.name),
                    None => "Where the model runs.".to_owned(),
                }),
            ),
            Slot::Vocabulary => plain(
                "Names and terms",
                "Spelled the way you write them here, most important first.",
            ),
            Slot::LiveText => plain(
                "Live text",
                "A draft at the cursor while you speak. Needs the input method.",
            ),
            Slot::Hotkey => plain("Hotkey", "Starts a session and finishes it."),
            Slot::Microphone => plain("Microphone", "The input sessions listen to."),
            Slot::Pause => plain("Pause length", "How long a silence ends a phrase."),
            Slot::Threshold => plain(
                "Speech threshold",
                "Higher ignores more noise but may clip quiet words.",
            ),
            Slot::Method => pair(
                "Method",
                Some(match &status.note {
                    Some(note) => {
                        format!("Using {}, because {note}.", status.method.to_lowercase())
                    }
                    None => "How text reaches the focused app.".to_owned(),
                }),
            ),
            Slot::Profiles => plain(
                "Profiles",
                "Switch between them here or from the tray. Add copies the one in use.",
            ),
            Slot::Capitals => plain("Capitals", "How sentences start."),
            Slot::Periods => plain("Periods", "Question and exclamation marks always stay."),
            Slot::SpokenEmoji => plain(
                "Spoken emoji",
                "“Thumbs up emoji” → 👍. Names in 140+ languages.",
            ),
            Slot::Laughter => plain("Laughter as one word", "“Ha-ha-ha” becomes “hahaha”."),
            Slot::Replacements => plain(
                "Replacements",
                "Words to write differently every time they are heard.",
            ),
            Slot::Theme => plain(
                "Theme",
                "System follows the light or dark mode of the desktop.",
            ),
            Slot::CustomThemes => plain(
                "Custom themes",
                "JSON files in the themes folder, in the format Sonora reads. A saved file shows up right away.",
            ),
            Slot::Position => plain("Pill position", "The edge of the screen the pill sits on."),
            Slot::Distance => plain("Distance from the edge", "The gap under or over the pill."),
            Slot::Caption => plain(
                "Live caption",
                "A floating text window below the top pill or above the bottom pill.",
            ),
            Slot::CancelButton => plain(
                "Cancel button",
                "The cross on the left. Escape cancels without it.",
            ),
            Slot::ConfirmButton => plain(
                "Confirm button",
                "The check mark on the right. Enter or the hotkey finishes without it.",
            ),
            Slot::Login => pair("Launch at login", None),
            Slot::Tray => plain("Tray icon", "Switch models and profiles from the panel."),
            Slot::Unload => plain(
                "Unload models",
                "Frees memory between sessions. The next one starts a moment later.",
            ),
            Slot::AutoUpdates => plain(
                "Automatic update checks",
                "Looks for a new release now and then.",
            ),
            Slot::Updates => plain("Updates", "Look for a new release now."),
            Slot::Resources => plain(
                "Resources",
                "The memory and processor time the app takes, and the memory its model takes.",
            ),
            Slot::Version => pair(
                "Tokativ",
                Some(format!(
                    "Version {}. {} models installed.",
                    env!("CARGO_PKG_VERSION"),
                    self.library.read(cx).installed_models().len()
                )),
            ),
        }
    }

    /// The column's slots, with a hairline between rows that are not under a fresh label. The
    /// explore row reads as part of the model list above it, so no hairline touches it.
    fn slots(&self, cx: &App) -> Vec<Slot> {
        let bare = match self.searching() {
            false => self.page_slots(self.page, cx),
            true => self.found(cx),
        };
        let mut slots = Vec::with_capacity(bare.len() * 2);
        let mut parted = false;
        let mut previous = None;
        for slot in bare {
            let titled = matches!(slot, Slot::Title(_) | Slot::ProfileTitle);
            let explore = slot == Slot::Explore || previous == Some(Slot::Explore);
            previous = Some(slot);
            if parted && !titled && !explore {
                slots.push(Slot::Sep);
            }
            parted = !titled;
            slots.push(slot);
        }
        slots
    }

    /// A plain row for a slot: its title and detail on the left, `action` on the right.
    fn row(&self, slot: Slot, action: impl IntoElement, cx: &App) -> AnyElement {
        let (title, detail) = self.meta(slot, cx).unwrap_or_default();
        row(title, detail, action.into_any_element(), cx)
    }

    fn slot_element(&mut self, slot: Slot, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        match slot {
            Slot::Title(label) => title(label, cx),
            Slot::ProfileTitle => title(&config.active_profile().name.clone(), cx),
            Slot::Sep => Separator::horizontal().w_full().into_any_element(),
            Slot::Models => self.models(config, cx),
            Slot::Explore => self.explore_row(cx),
            Slot::Runtime => self.runtime(config, cx),
            Slot::Vocabulary => self.vocabulary(config, cx),
            Slot::LiveText => {
                let live = config.live_preview;
                let switch = Switch::new("live-preview", live).on_click(cx.listener(
                    move |this, _, _, cx| this.edit(cx, |config| config.live_preview = !live),
                ));
                self.row(slot, switch, cx)
            }
            #[cfg(not(target_os = "linux"))]
            Slot::Hotkey => self.row(slot, self.hotkey.clone(), cx),
            #[cfg(target_os = "linux")]
            Slot::Hotkey => div().into_any_element(),
            Slot::Microphone => self.microphone(config, cx),
            Slot::Pause => {
                let silence = self
                    .silence
                    .dragging
                    .unwrap_or_else(|| fraction(config.vad.silence_ms as f32, SILENCE_MS));
                let ms = value(silence, SILENCE_MS).round() as u32;
                let slider = self.slider(
                    &self.silence,
                    silence,
                    format!("{ms} ms"),
                    |this| &mut this.silence,
                    |config, fraction| {
                        config.vad.silence_ms = value(fraction, SILENCE_MS).round() as u32
                    },
                    cx,
                );
                self.row(slot, slider, cx)
            }
            Slot::Threshold => {
                let threshold = self
                    .threshold
                    .dragging
                    .unwrap_or_else(|| fraction(config.vad.threshold, THRESHOLD));
                let shown = value(threshold, THRESHOLD);
                let slider = self.slider(
                    &self.threshold,
                    threshold,
                    format!("{shown:.2}"),
                    |this| &mut this.threshold,
                    |config, fraction| {
                        config.vad.threshold = (value(fraction, THRESHOLD) * 100.).round() / 100.
                    },
                    cx,
                );
                self.row(slot, slider, cx)
            }
            Slot::Method => self.method(config, cx),
            Slot::Profiles => self.profiles(config, cx),
            Slot::Capitals => {
                let capitals = config.active_profile().capitals;
                let picker = Picker::new("capitals", &self.popovers, capitals.name())
                    .width(Picker::REGULAR)
                    .items(Capitals::ALL.into_iter().map(|choice| {
                        MenuItem::new(
                            SharedString::from(format!("capitals-{choice:?}")),
                            choice.name(),
                        )
                        .selected(choice == capitals)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.active_profile_mut().capitals = choice);
                        }))
                    }));
                self.row(slot, picker, cx)
            }
            Slot::Periods => {
                let periods = config.active_profile().periods;
                let picker = Picker::new("periods", &self.popovers, periods.name())
                    .width(Picker::REGULAR)
                    .items(Periods::ALL.into_iter().map(|choice| {
                        MenuItem::new(
                            SharedString::from(format!("periods-{choice:?}")),
                            choice.name(),
                        )
                        .selected(choice == periods)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.active_profile_mut().periods = choice);
                        }))
                    }));
                self.row(slot, picker, cx)
            }
            Slot::SpokenEmoji => {
                let on = config.active_profile().spoken_emoji;
                let switch =
                    Switch::new("spoken-emoji", on).on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| config.active_profile_mut().spoken_emoji = !on)
                    }));
                self.row(slot, switch, cx)
            }
            Slot::Laughter => {
                let on = config.active_profile().join_laughter;
                let switch = Switch::new("join-laughter", on).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.edit(cx, |config| config.active_profile_mut().join_laughter = !on)
                    },
                ));
                self.row(slot, switch, cx)
            }
            Slot::Replacements => self.replacements(config, cx),
            Slot::Theme => self.theme(config, cx),
            Slot::CustomThemes => {
                let folder = Button::new("open-themes")
                    .label("Open folder")
                    .small()
                    .outline()
                    .on_click(|_, _, cx| {
                        let folder = themes::folder();
                        if let Err(error) = std::fs::create_dir_all(&folder) {
                            log::error!("themes: cannot create {}: {error}", folder.display());
                        }
                        cx.open_with_system(&folder);
                    });
                self.row(slot, folder, cx)
            }
            Slot::Position => {
                let edge = config.pill.edge;
                let picker = Picker::new("edge", &self.popovers, edge.name())
                    .width(Picker::NARROW)
                    .items(PillEdge::ALL.into_iter().map(|choice| {
                        MenuItem::new(
                            SharedString::from(format!("edge-{choice:?}")),
                            choice.name(),
                        )
                        .selected(choice == edge)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.pill.edge = choice);
                        }))
                    }));
                self.row(slot, picker, cx)
            }
            Slot::Distance => {
                let margin = self
                    .margin
                    .dragging
                    .unwrap_or_else(|| fraction(config.pill.margin, MARGIN));
                let pixels = value(margin, MARGIN).round();
                let slider = self.slider(
                    &self.margin,
                    margin,
                    format!("{pixels} px"),
                    |this| &mut this.margin,
                    |config, fraction| config.pill.margin = value(fraction, MARGIN).round(),
                    cx,
                );
                self.row(slot, slider, cx)
            }
            Slot::Caption => self.switch(slot, "caption", config.pill.caption, cx, |config| {
                &mut config.pill.caption
            }),
            Slot::CancelButton => self.switch(
                slot,
                "cancel-button",
                config.pill.cancel_button,
                cx,
                |config| &mut config.pill.cancel_button,
            ),
            Slot::ConfirmButton => self.switch(
                slot,
                "confirm-button",
                config.pill.confirm_button,
                cx,
                |config| &mut config.pill.confirm_button,
            ),
            Slot::Login => self.switch(slot, "login", config.launch_at_login, cx, |config| {
                &mut config.launch_at_login
            }),
            Slot::Tray => self.switch(slot, "tray", config.tray, cx, |config| &mut config.tray),
            Slot::Unload => self.unload(config, cx),
            Slot::AutoUpdates => self.switch(
                slot,
                "automatic-updates",
                config.check_for_updates,
                cx,
                |config| &mut config.check_for_updates,
            ),
            Slot::Updates => {
                let button = Button::new("check-updates")
                    .label("Check for updates")
                    .small()
                    .outline()
                    .on_click(|_, _, cx| Host::global(cx).dispatch(crate::Action::Updates, cx));
                self.row(slot, button, cx)
            }
            Slot::Resources => self.resources(config, cx),
            Slot::Version => self.row(slot, div(), cx),
        }
    }

    /// A row with a switch that flips one boolean in the config.
    fn switch(
        &self,
        slot: Slot,
        id: &'static str,
        on: bool,
        cx: &mut Context<Self>,
        field: fn(&mut Config) -> &mut bool,
    ) -> AnyElement {
        let switch = Switch::new(id, on).on_click(cx.listener(move |this, _, _, cx| {
            this.edit(cx, |config| *field(config) = !on);
        }));
        self.row(slot, switch, cx)
    }

    fn microphone(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let device = config.input_device.clone();
        let device_name = device
            .as_ref()
            .and_then(|id| self.devices.iter().find(|known| &known.id == id))
            .map(|known| known.name.clone())
            .unwrap_or_else(|| "System default".into());
        let default_item = MenuItem::new("device-default", "System default")
            .selected(device.is_none())
            .on_click(cx.listener(|this, _, _, cx| {
                this.edit(cx, |config| config.input_device = None);
            }));
        let device_items = self.devices.iter().map(|known| {
            let id = known.id.clone();
            MenuItem::new(
                SharedString::from(format!("device-{}", known.id)),
                known.name.clone(),
            )
            .selected(device.as_deref() == Some(&known.id))
            .on_click(cx.listener(move |this, _, _, cx| {
                let id = id.clone();
                this.edit(cx, |config| config.input_device = Some(id));
            }))
        });
        let picker = Picker::new("device", &self.popovers, device_name)
            .menu(device_menu("device-menu", &self.device_scrollbar))
            .item(default_item)
            .items(device_items.collect::<Vec<_>>());
        self.row(Slot::Microphone, picker, cx)
    }

    /// A slider row's control: the scrubber and the value beside it.
    #[allow(clippy::too_many_arguments)]
    fn slider(
        &self,
        slider: &Slider,
        fraction: f32,
        label: String,
        field: fn(&mut Self) -> &mut Slider,
        save: fn(&mut Config, f32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div().w(theme.metrics.cover).child(
                    Scrubber::new(&slider.state, fraction)
                        .colors(theme.progress_bar, theme.muted, theme.foreground)
                        .on_move(cx.listener(move |this, fraction: &f32, _, cx| {
                            field(this).dragging = Some(*fraction);
                            cx.notify();
                        }))
                        .on_release(cx.listener(move |this, _, _, cx| {
                            if let Some(fraction) = field(this).dragging.take() {
                                this.edit(cx, |config| save(config, fraction));
                            }
                        })),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.metrics.control * 1.8)
                    .whitespace_nowrap()
                    .text_right()
                    .text_size(theme.text(Text::Small))
                    .child(label),
            )
            .into_any_element()
    }

    fn method(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let backend = config.insertion.backend;
        let picker = Picker::new("backend", &self.popovers, backend.name())
            .width(Picker::REGULAR)
            .items(
                Backend::ALL
                    .into_iter()
                    .filter(|choice| {
                        // The input method is a Wayland protocol.
                        cfg!(target_os = "linux") || *choice != Backend::InputMethod
                    })
                    .map(|choice| {
                        MenuItem::new(
                            SharedString::from(format!("backend-{choice:?}")),
                            choice.name(),
                        )
                        .selected(choice == backend)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.insertion.backend = choice);
                        }))
                    }),
            );
        self.row(Slot::Method, picker, cx)
    }

    fn theme(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let current = config.theme.clone();
        let known = themes::name(&current, cx);
        let pick = |id: String, label: String, cx: &mut Context<Self>| {
            MenuItem::new(SharedString::from(format!("theme-{id}")), label)
                .selected(id == current)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = id.clone();
                    this.edit(cx, |config| config.theme = id);
                }))
        };
        let mut items: Vec<MenuItem> = themes::BUILT_IN
            .into_iter()
            .map(|kind| pick(kind.id().to_owned(), kind.label().to_string(), cx))
            .collect();
        let custom = themes::custom(cx).to_vec();
        if !custom.is_empty() {
            items.push(MenuItem::separator("themes-custom"));
            items.extend(
                custom
                    .into_iter()
                    .map(|theme| pick(theme.id, theme.name, cx)),
            );
        }
        // A theme file that was removed or broke stays selected until another is picked.
        let shown = known.unwrap_or_else(|| {
            let missing = format!("{current} (unavailable)");
            items.push(
                MenuItem::new("theme-unavailable", missing.clone())
                    .selected(true)
                    .disabled(),
            );
            missing
        });
        let picker = Picker::new("theme", &self.popovers, shown)
            .width(Picker::REGULAR)
            .items(items);
        self.row(Slot::Theme, picker, cx)
    }

    /// The app's memory and processor time and the model's memory, side by side. The app's own
    /// memory leaves out the weights when they sit in it, so the columns add up.
    fn resources(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let gpu = self
            .status
            .read(cx)
            .device
            .as_ref()
            .is_some_and(|device| device.gpu);
        let id = ModelId::parse(&config.model);
        let weights = id
            .as_ref()
            .and_then(|id| match self.library.read(cx).state(id) {
                ModelState::Installed { bytes } => Some(bytes),
                _ => None,
            });
        let model = id
            .map(|id| models::Described::of(id).name)
            .unwrap_or_default();
        let hardware = self.hardware.as_deref().unwrap_or_default();
        let total = |on_gpu: bool| {
            hardware
                .iter()
                .find(|hardware| hardware.gpu == on_gpu)
                .map(|hardware| hardware.memory)
                .filter(|memory| *memory > 0)
        };
        let ram = total(false);
        let model_total = total(gpu);
        let share = |part: u64, whole: Option<u64>| whole.map(|whole| part as f32 / whole as f32);

        // On the processor the weights count in the app's resident memory while loaded, which
        // is when it holds more than they take.
        let resident = self.usage.memory;
        let app = match (gpu, weights) {
            (false, Some(weights)) if resident > weights => resident - weights,
            _ => resident,
        };
        let of_memory = |whole: Option<u64>, what: &str| match whole {
            Some(whole) => format!("of {} {what}", chrome::memory(whole)),
            None => what.to_owned(),
        };

        let app_memory = Reading {
            id: "reading-app-memory",
            place: Place::First,
            label: "App RAM",
            value: chrome::size(app),
            share: share(app, ram),
            of: of_memory(ram, "memory"),
            tip: "Memory Tokativ itself takes: the windows, the audio and the engine, without \
                  the model's weights."
                .to_owned(),
        };
        let cpu = self.usage.cpu;
        let app_cpu = Reading {
            id: "reading-app-cpu",
            place: Place::Middle,
            label: "App CPU",
            value: cpu
                .map(|cpu| format!("{cpu:.0}%"))
                .unwrap_or_else(|| "…".to_owned()),
            share: cpu.map(|cpu| cpu / 100.),
            of: "of one core".to_owned(),
            tip: "How busy Tokativ keeps the processor, as a share of one core. It rises while \
                  you speak and the model listens."
                .to_owned(),
        };
        let model_memory = Reading {
            id: "reading-model-memory",
            place: Place::Last,
            label: match gpu {
                true => "Model VRAM",
                false => "Model RAM",
            },
            value: weights
                .map(chrome::size)
                .unwrap_or_else(|| "None".to_owned()),
            share: weights.and_then(|weights| share(weights, model_total)),
            of: match gpu {
                true => of_memory(model_total, "video memory"),
                false => of_memory(model_total, "memory"),
            },
            tip: match (weights, gpu) {
                (None, _) => "No model is downloaded yet.".to_owned(),
                (Some(_), true) => format!(
                    "The weights of {model}, held on the graphics card while the model is \
                     loaded. About the size of the download."
                ),
                (Some(_), false) => format!(
                    "The weights of {model}, held in memory while the model is loaded. About \
                     the size of the download."
                ),
            },
        };

        let divider = || div().w(px(1.)).flex_none().bg(theme.border.opacity(0.6));
        div()
            .py_2()
            .child(
                div()
                    .flex()
                    .rounded(chrome::GROUP_RADIUS)
                    .squircle()
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border.opacity(0.6))
                    .bg(theme.secondary.opacity(0.25))
                    .child(reading(app_memory, cx))
                    .child(divider())
                    .child(reading(app_cpu, cx))
                    .child(divider())
                    .child(reading(model_memory, cx)),
            )
            .into_any_element()
    }

    fn unload(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let idle = config.models.unload_after_minutes;
        let idle_name = |minutes: u32| match minutes {
            0 => "Never".to_string(),
            minutes => format!("After {minutes} min"),
        };
        let picker = Picker::new("idle", &self.popovers, idle_name(idle))
            .width(Picker::REGULAR)
            .items(IDLE_MINUTES.into_iter().map(|minutes| {
                MenuItem::new(
                    SharedString::from(format!("idle-{minutes}")),
                    idle_name(minutes),
                )
                .selected(minutes == idle)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |config| config.models.unload_after_minutes = minutes);
                }))
            }));
        self.row(Slot::Unload, picker, cx)
    }

    /// The models to choose from. Clicking an installed model makes it the one sessions use.
    fn models(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let chosen = ModelId::parse(&config.model);
        let listed = models::listed(self.library.read(cx), &[]);
        let rows = listed.into_iter().map(|model| {
            let id = model.id.clone();
            let radio = Radio::new(
                SharedString::from(format!("use-{}", id.key())),
                Some(&id) == chosen.as_ref(),
            )
            .into_any_element();
            let key = id.key();
            let choose = cx.listener(move |this, _, _, cx| {
                let key = key.clone();
                this.edit(cx, |config| config.model = key);
            });
            div()
                .mx(-HOVER_REACH)
                .child(models::model_row(
                    model,
                    &self.library,
                    Some(radio),
                    Some(Box::new(choose)),
                    HOVER_REACH,
                    cx.theme().radius,
                    cx,
                ))
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .py_2()
            .children(rows)
            .into_any_element()
    }

    /// The row that opens the model search, marked with a magnifier.
    fn explore_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (title, _) = self.meta(Slot::Explore, cx).unwrap_or_default();
        let icon = |path: &str, size: f32| {
            svg()
                .path(icons::path(path))
                .size(px(size))
                .flex_none()
                .text_color(theme.muted_foreground)
        };
        let entry = hovered(
            "explore-models",
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(icon("icons/search.svg", 16.))
                .child(labels(title, None, cx))
                .child(icon("icons/chevron-right.svg", 16.)),
            cx,
        )
        .on_click(cx.listener(|this, _, window, cx| this.explore(true, window, cx)));
        div()
            .mx(-HOVER_REACH)
            .mb_4()
            .child(entry)
            .into_any_element()
    }

    fn runtime(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let processor = config.processor;
        let picker = Picker::new("processor", &self.popovers, processor.name())
            .width(Picker::REGULAR)
            .items(Processor::ALL.into_iter().map(|choice| {
                MenuItem::new(
                    SharedString::from(format!("processor-{choice:?}")),
                    choice.name(),
                )
                .selected(choice == processor)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |config| config.processor = choice);
                }))
            }));
        self.row(Slot::Runtime, picker, cx)
    }

    /// The names and terms the model is asked to spell as written, and whether the model in use
    /// takes them at all.
    fn vocabulary(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let ignored = self
            .status
            .read(cx)
            .abilities
            .is_some_and(|abilities| !abilities.vocabulary);
        let items: Vec<AnyElement> = config
            .vocabulary
            .iter()
            .enumerate()
            .map(|(index, term)| {
                let remove = cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |config| {
                        if index < config.vocabulary.len() {
                            config.vocabulary.remove(index);
                        }
                    });
                });
                item(
                    SharedString::from(format!("remove-term-{index}")),
                    div().child(term.clone()),
                    remove,
                    cx,
                )
            })
            .collect();
        let add = Button::new("add-term")
            .label("Add")
            .small()
            .outline()
            .on_click(cx.listener(|this, _, window, cx| this.add_term(window, cx)));
        let note = ignored.then(|| {
            let name = ModelId::parse(&config.model)
                .map(|id| models::Described::of(id).name)
                .unwrap_or_else(|| "This model".to_owned());
            div()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{name} does not use the vocabulary. Whisper and Qwen3-ASR do, and they also follow the text before each phrase."
                ))
        });
        self.listing(
            Slot::Vocabulary,
            items,
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(self.term.clone()))
                .child(div().flex_none().child(add)),
            note,
            cx,
        )
    }

    /// Adds the term typed into the field and clears it.
    fn add_term(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let term = self.term.read(cx).text().trim().to_owned();
        if term.is_empty() {
            return;
        }
        self.edit(cx, |config| {
            if !config.vocabulary.contains(&term) {
                config.vocabulary.push(term)
            }
        });
        self.term.update(cx, |input, cx| input.set_text("", cx));
        let focus = self.term.focus_handle(cx);
        focus.focus(window, cx);
    }

    /// Every profile with the one in use marked, and the field that names a new profile or
    /// renames the one in use.
    fn profiles(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let active = config.active_profile().name.clone();
        let removable = config.profiles.len() > 1;
        let items: Vec<AnyElement> = config
            .profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let name = profile.name.clone();
                let choose = {
                    let name = name.clone();
                    cx.listener(move |this, _, _, cx| {
                        let name = name.clone();
                        this.edit(cx, |config| config.profile = name);
                    })
                };
                let remove = Button::new(SharedString::from(format!("remove-profile-{index}")))
                    .icon("icons/x.svg")
                    .small()
                    .ghost()
                    .tint(cx.theme().muted_foreground)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| {
                            if index < config.profiles.len() && config.profiles.len() > 1 {
                                config.profiles.remove(index);
                            }
                        });
                    }));
                div()
                    .mx(-HOVER_REACH)
                    .child(
                        hovered(
                            SharedString::from(format!("profile-{index}")),
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(Radio::new(
                                    SharedString::from(format!("use-profile-{index}")),
                                    name == active,
                                ))
                                .child(div().flex_1().min_w_0().child(name))
                                .when(removable, |this| this.child(remove)),
                            cx,
                        )
                        .py_1()
                        .on_click(choose),
                    )
                    .into_any_element()
            })
            .collect();
        let rename = Button::new("rename-profile")
            .label("Rename")
            .small()
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.name_profile(false, cx)));
        let add = Button::new("add-profile")
            .label("Add")
            .small()
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.name_profile(true, cx)));
        self.listing(
            Slot::Profiles,
            items,
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(self.profile_name.clone()))
                .child(div().flex_none().child(rename))
                .child(div().flex_none().child(add)),
            None::<gpui::Div>,
            cx,
        )
    }

    /// Adds a copy of the profile in use under the name typed in the field and switches to it,
    /// or with `copy` off renames the profile in use. A name already taken does nothing.
    fn name_profile(&mut self, copy: bool, cx: &mut Context<Self>) {
        let name = self.profile_name.read(cx).text().trim().to_owned();
        let taken = self
            .store
            .read(cx)
            .config()
            .profiles
            .iter()
            .any(|profile| profile.name == name);
        if name.is_empty() || taken {
            return;
        }
        self.edit(cx, |config| {
            let profile = config.active_profile_mut();
            match copy {
                true => {
                    let mut copied = profile.clone();
                    copied.name = name.clone();
                    config.profiles.push(copied);
                }
                false => profile.name = name.clone(),
            }
            config.profile = name;
        });
        self.profile_name
            .update(cx, |input, cx| input.set_text("", cx));
    }

    /// The fixed rewrites of the profile in use, and the two fields that add one.
    fn replacements(&self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let arrow = || {
            svg()
                .path(icons::path("icons/chevron-right.svg"))
                .size(px(14.))
                .flex_none()
                .text_color(theme.muted_foreground)
        };
        let items: Vec<AnyElement> = config
            .active_profile()
            .replacements
            .iter()
            .enumerate()
            .map(|(index, replacement)| {
                let remove = cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |config| {
                        let replacements = &mut config.active_profile_mut().replacements;
                        if index < replacements.len() {
                            replacements.remove(index);
                        }
                    });
                });
                item(
                    SharedString::from(format!("remove-replacement-{index}")),
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(replacement.from.clone()))
                        .child(arrow())
                        .child(div().flex_1().min_w_0().child(replacement.to.clone())),
                    remove,
                    cx,
                )
            })
            .collect();
        let add = Button::new("add-replacement")
            .label("Add")
            .small()
            .outline()
            .on_click(cx.listener(|this, _, window, cx| this.add_replacement(window, cx)));
        self.listing(
            Slot::Replacements,
            items,
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(self.replace_from.clone()))
                .child(div().flex_1().min_w_0().child(self.replace_to.clone()))
                .child(div().flex_none().child(add)),
            None::<gpui::Div>,
            cx,
        )
    }

    /// Adds the replacement typed into the two fields and clears them.
    fn add_replacement(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.replace_from.read(cx).text().trim().to_owned();
        let to = self.replace_to.read(cx).text().to_owned();
        if from.is_empty() {
            return;
        }
        self.edit(cx, |config| {
            config
                .active_profile_mut()
                .replacements
                .push(tokativ_config::Replacement { from, to })
        });
        for field in [&self.replace_from, &self.replace_to] {
            field.update(cx, |input, cx| input.set_text("", cx));
        }
        let focus = self.replace_from.focus_handle(cx);
        focus.focus(window, cx);
    }

    /// A row that holds a list: the slot's title and detail, the items under them, then the
    /// fields that add one and a note when there is one.
    fn listing(
        &self,
        slot: Slot,
        items: Vec<AnyElement>,
        adder: impl IntoElement,
        note: Option<impl IntoElement>,
        cx: &App,
    ) -> AnyElement {
        let (title, detail) = self.meta(slot, cx).unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .py_3()
            .child(labels(title, detail, cx))
            .when(!items.is_empty(), |this| {
                this.child(div().flex().flex_col().children(items))
            })
            .child(adder)
            .children(note)
            .into_any_element()
    }

    /// Search over Hugging Face for any model the engine can run, in place of the model
    /// category. Results and a repository's files are virtualized lists, so only the rows in
    /// view are drawn.
    fn explorer(&self, cx: &mut Context<Self>) -> AnyElement {
        let back = |label: &'static str, cx: &mut Context<Self>| {
            Button::new("explore-back")
                .label(label)
                .icon("icons/chevron-left.svg")
                .small()
                .ghost()
                .on_click(
                    cx.listener(|this, _, window, cx| match this.opened.is_some() {
                        true => {
                            this.opened = None;
                            cx.notify();
                        }
                        false => this.explore(false, window, cx),
                    }),
                )
        };

        let theme = *cx.theme();
        let search = &self.library.read(cx).search;
        let results = search.results.clone();
        let note = if search.query.trim().is_empty() {
            Some("Whisper, Parakeet, Canary, GigaAM, Moonshine, Qwen3-ASR, Voxtral and more. Only models in GGUF form that run here are listed.".to_string())
        } else if search.loading && search.results.is_empty() {
            Some("Searching".to_string())
        } else if let Some(error) = &search.error {
            Some(format!("Search failed: {error}"))
        } else if search.results.is_empty() {
            Some("No models that run here match that.".to_string())
        } else {
            None
        };
        let files = self
            .opened
            .as_ref()
            .map(|repo| match search.files.get(repo) {
                Some(Files::Loaded(files)) if !files.is_empty() => Ok(files.clone()),
                None | Some(Files::Loading) => Err("Listing files".to_string()),
                Some(Files::Failed(error)) => Err(format!("Cannot list files: {error}")),
                Some(Files::Loaded(_)) => Err("No GGUF files in this repository".to_string()),
            });

        let (top, list) = match (self.opened.clone(), files) {
            (Some(repo), Some(files)) => {
                let found = results
                    .iter()
                    .find(|repository| repository.id == repo)
                    .cloned();
                let top = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().flex().child(back("Results", cx)))
                    .child(
                        div()
                            .text_size(theme.text(Text::Large))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(repo.clone()),
                    )
                    .when_some(found.as_ref(), |this, repository| {
                        this.child(models::facts(repository, cx))
                    })
                    .into_any_element();
                let list = match files {
                    Ok(files) => {
                        let count = files.len();
                        let rows = uniform_list(
                            "repository-files",
                            count,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|index| {
                                        models::file_row(&repo, &files[index], &this.library, cx)
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.files_scroll)
                        .size_full();
                        listed(
                            Scroller::listing("repository-files-scroll", &self.files_scrollbar)
                                .child(rows)
                                .into_any_element(),
                            cx,
                        )
                    }
                    Err(waiting) => hint(&waiting, cx),
                };
                (top, list)
            }
            _ => {
                let list = match note {
                    Some(note) => hint(&note, cx),
                    None => {
                        let count = results.len();
                        let rows = uniform_list(
                            "search-results",
                            count,
                            cx.processor(move |_, range: Range<usize>, _, cx| {
                                range
                                    .map(|index| {
                                        let repository = &results[index];
                                        let repo = repository.id.clone();
                                        let open = cx.listener(move |this, _, _, cx| {
                                            this.library.update(cx, |library, cx| {
                                                library.open_repository(repo.clone(), cx)
                                            });
                                            this.opened = Some(repo.clone());
                                            cx.notify();
                                        });
                                        models::result_row(repository, Box::new(open), cx)
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.results_scroll)
                        .size_full();
                        listed(
                            Scroller::listing("search-results-scroll", &self.results_scrollbar)
                                .child(rows)
                                .into_any_element(),
                            cx,
                        )
                    }
                };
                let top = div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(back("Models", cx))
                            .child(div().flex().gap_2().children(GUIDES.map(
                                |(id, label, url)| {
                                    Button::new(id)
                                        .label(label)
                                        .icon("icons/external-link.svg")
                                        .small()
                                        .outline()
                                        .on_click(move |_, _, cx| cx.open_url(url))
                                },
                            ))),
                    )
                    .child(self.model_search.clone())
                    .into_any_element();
                (top, list)
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(self.header_height)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .max_w(COLUMN)
                    .px_6()
                    .pb_6()
                    .gap_4()
                    .child(top)
                    .child(list),
            )
            .into_any_element()
    }

    /// Searches once typing pauses, so each keystroke does not reach the network.
    fn model_query_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.model_search.read(cx).text().to_owned();
        if query == self.searched {
            return;
        }
        self.searched = query.clone();
        self._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_PAUSE).await;
            this.update(cx, |this, cx| {
                this.opened = None;
                this.library
                    .update(cx, |library, cx| library.search(query, cx));
            })
            .ok();
        }));
    }

    /// The column of rows for the category shown, or for the search.
    fn column(&mut self, config: &Config, cx: &mut Context<Self>) -> AnyElement {
        let slots = self.slots(cx);
        let body = match slots.is_empty() {
            true => Vacancy::new("No settings match")
                .icon("icons/search.svg")
                .py_6()
                .into_any_element(),
            false => div()
                .flex()
                .flex_col()
                .children(
                    slots
                        .into_iter()
                        .map(|slot| self.slot_element(slot, config, cx)),
                )
                .into_any_element(),
        };
        Scroller::new("settings", &self.scrollbar)
            .flex()
            .flex_col()
            .items_center()
            // The rows dissolve over the header's height as they scroll up beneath it, so no
            // sharp text sits over the blur. The tail reaches past the header so the handoff has
            // no hard edge.
            .fade_edges(self.header_height + HEADER_FADE_TAIL, px(0.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(COLUMN)
                    .px_6()
                    .pb_6()
                    .pt(self.header_height)
                    .child(body),
            )
            .into_any_element()
    }

    /// The search field and the category bar, floated over the column with a blur under them.
    fn header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let searching = self.searching();
        let chosen = self.page;
        let view = cx.entity().downgrade();
        // A search lights no category, since its rows come from all of them, and picking one
        // ends the search.
        let categories = TabBar::new("settings-categories")
            .max_w_full()
            .blurred()
            .items(Page::ALL.map(|page| {
                Button::new(SharedString::from(format!("page-{}", page.id())))
                    .label(page.title())
                    .icon(page.icon())
                    .small()
                    .ghost()
                    .selected(!searching && page == chosen)
                    .on_click(cx.listener(move |this, _, _, cx| this.select(page, cx)))
            }));
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .px_6()
            // The veil is absolute and takes the header's own size, so the last child, the
            // column, is the one to measure.
            .on_children_prepainted(move |bounds, _, cx| {
                let Some(height) = bounds.last().map(|bounds| bounds.size.height) else {
                    return;
                };
                view.update(cx, |view, cx| view.set_header_height(height, cx))
                    .ok();
            })
            .child(veil(self.header_height, HEADER_BLUR, window))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(COLUMN)
                    .gap_2()
                    .pt_4()
                    .pb_6()
                    .child(self.search.clone())
                    .child(div().flex().justify_center().child(categories)),
            )
            .into_any_element()
    }
}

/// Pages that compare speech models, offered beside the search for picking one to look for.
const GUIDES: [(&str, &str, &str); 2] = [
    (
        "guide-leaderboard",
        "ASR leaderboard",
        "https://huggingface.co/spaces/hf-audio/open_asr_leaderboard",
    ),
    (
        "guide-handy",
        "Handy models",
        "https://models.handy.computer/",
    ),
];

/// A group label over the rows that follow it, in Sonora's eyebrow style.
fn title(label: &str, cx: &App) -> AnyElement {
    div()
        .h(cx.theme().metrics.row)
        .flex()
        .flex_col()
        .justify_end()
        .pb_1()
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(eyebrow(label.to_owned(), cx)),
        )
        .into_any_element()
}

/// A title with its muted detail under it, filling the room left of a control.
fn labels(title: SharedString, detail: Option<SharedString>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(title),
        )
        .when_some(detail, |this, detail| {
            this.child(
                div()
                    .min_w_0()
                    .text_color(theme.muted_foreground)
                    .text_size(theme.text(Text::Small))
                    .child(detail),
            )
        })
}

/// A settings row: the title and detail on the left, the control on the right.
fn row(
    title: SharedString,
    detail: Option<SharedString>,
    action: AnyElement,
    cx: &App,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .py_3()
        .child(labels(title, detail, cx))
        .child(div().flex_none().child(action))
        .into_any_element()
}

/// A row that shades under the pointer. The shade reaches past the column by
/// [`HOVER_REACH`], which the caller takes back with a negative margin, so the row's text
/// stays in line with the rows around it.
fn hovered(
    id: impl Into<gpui::ElementId>,
    content: impl IntoElement,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px(HOVER_REACH)
        .py_2()
        .rounded(theme.radius)
        .squircle()
        .cursor_pointer()
        .hover(|style| style.bg(theme.secondary.opacity(0.6)))
        .child(content)
}

/// One entry of a list row, with the cross that removes it.
fn item(
    id: SharedString,
    content: impl IntoElement,
    remove: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_2()
        .py_0p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(content),
        )
        .child(
            Button::new(id)
                .icon("icons/x.svg")
                .small()
                .ghost()
                .tint(theme.muted_foreground)
                .on_click(remove),
        )
        .into_any_element()
}

/// A measured value shown where a control would go.
/// One reading of the resources row: a label, the value large, a bar for how much of the whole
/// it takes when the whole is known, and what the whole is. `tip` explains it on hover.
struct Reading {
    id: &'static str,
    /// Where the reading sits in the row, so its hover shade takes the frame's outer corners.
    place: Place,
    label: &'static str,
    value: String,
    share: Option<f32>,
    of: String,
    tip: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    First,
    Middle,
    Last,
}

fn reading(reading: Reading, cx: &App) -> AnyElement {
    let theme = cx.theme();
    // The frame does not clip to its rounded corners, so the readings at its ends round their
    // own outer corners to match, inside its one pixel border.
    let corner = crate::nested_radius(chrome::GROUP_RADIUS, px(1.));
    let shaped = div().id(reading.id);
    let shaped = match reading.place {
        Place::First => shaped.rounded_l(corner).squircle(),
        Place::Last => shaped.rounded_r(corner).squircle(),
        Place::Middle => shaped,
    };
    shaped
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_1()
        .px(chrome::GROUP_PADDING)
        .py_3()
        .hover(|style| style.bg(theme.secondary.opacity(0.45)))
        .tip_label(reading.tip, Perch::Above)
        .child(eyebrow(reading.label, cx).text_size(theme.text(Text::Tiny)))
        .child(
            div()
                .text_size(theme.text(Text::Large))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .whitespace_nowrap()
                .child(reading.value),
        )
        .child(
            div()
                .pt_1()
                .child(chrome::meter(reading.share.unwrap_or(0.), px(6.), cx)),
        )
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(reading.of),
        )
        .into_any_element()
}

/// A virtualized list on a raised surface, filling the space left on the page.
fn listed(list: AnyElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .p(chrome::LISTED_INSET)
        .rounded(chrome::GROUP_RADIUS)
        .squircle()
        .bg(theme.secondary.opacity(0.45))
        .border_1()
        .border_color(theme.border.opacity(0.6))
        .child(list)
        .into_any_element()
}

/// A muted line standing in for a list that has nothing to show yet.
fn hint(text: &str, cx: &App) -> AnyElement {
    div()
        .text_size(cx.theme().text(Text::Small))
        .text_color(cx.theme().muted_foreground)
        .child(text.to_owned())
        .into_any_element()
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(setup) = self.setup.clone() {
            return chrome::frame("Set up Tokativ", setup.into_any_element(), window, cx);
        }
        let config = self.store.read(cx).config().clone();
        let hidden = self.hidden(window, cx);
        let backdrop: Hsla = cx.theme().background;
        let page = match self.exploring && !self.searching() {
            true => self.explorer(cx),
            false => self.column(&config, cx),
        };
        let header = self.header(window, cx);
        let content = div()
            .relative()
            .size_full()
            .overflow_hidden()
            // The padding, the fade and the scrollbar all hang off the header's measured
            // height, so the column sits out the first frame rather than snapping into place.
            .when(!self.header_measured, |this| this.invisible())
            .child(veiled(div().absolute().inset_0().child(page), hidden))
            // The scrim is the page's own colour, so fading it out fades the column in.
            .when(hidden > 0., |this| {
                this.child(div().absolute().inset_0().bg(backdrop).opacity(hidden))
            })
            .child(header)
            .into_any_element();
        chrome::frame("Tokativ", content, window, cx)
    }
}

/// The menu a microphone picker opens: capped in height and scrolled with Sonora's scrollbar,
/// since a machine can list many inputs.
pub(crate) fn device_menu(id: &'static str, scrollbar: &Entity<Scrollbar>) -> Menu {
    Menu::new(id)
        .w(Picker::WIDE)
        .max_h(DEVICE_MENU_HEIGHT)
        .scrollbar(scrollbar.clone())
}

/// Where `value` sits in `range`, as a fraction from 0 to 1.
fn fraction(value: f32, (low, high): (f32, f32)) -> f32 {
    ((value - low) / (high - low)).clamp(0., 1.)
}

fn value(fraction: f32, (low, high): (f32, f32)) -> f32 {
    low + (high - low) * fraction.clamp(0., 1.)
}
