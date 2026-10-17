//! The settings window. Every control writes the config through [`Store`], and the daemon
//! applies the change from there.

use gpui::Focusable as _;
use gpui::prelude::*;
use std::ops::Range;

use gpui::{
    AnyElement, App, Context, Entity, Global, ScrollHandle, SharedString, Size, Task,
    UniformListScrollHandle, Window, WindowHandle, div, px, svg, uniform_list,
};
use tokativ_config::{Backend, Capitals, Config, Periods, PillEdge, Processor};
use tokativ_engine::ModelId;
use ui::{
    ActiveTheme as _, Button, Input, Menu, MenuItem, Picker, Popovers, Radio, Scrollbar, Scroller,
    Scrubber, ScrubberState, Switch, Text,
};

use crate::chrome::{self, group, row, setting};
use crate::host::{Files, Host, Library, ModelState, Status, Store};
use crate::models;
use crate::themes;
use crate::usage::{Sampler, Usage};

const WIDTH: f32 = 920.;
const HEIGHT: f32 = 720.;
const SIDEBAR: gpui::Pixels = px(220.);
/// The ranges the sliders cover.
const SILENCE_MS: (f32, f32) = (200., 2000.);
const THRESHOLD: (f32, f32) = (0.2, 0.9);
const MARGIN: (f32, f32) = (0., 240.);
const IDLE_MINUTES: [u32; 5] = [0, 5, 15, 30, 60];
/// How often the resources group reads the daemon's footprint.
const SAMPLE_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// The tallest the microphone menu grows before it scrolls, about seven rows.
const DEVICE_MENU_HEIGHT: gpui::Pixels = px(300.);
/// How long typing must pause before the search goes out.
const SEARCH_PAUSE: std::time::Duration = std::time::Duration::from_millis(350);

/// Names the page the settings window opens on, for working on one page.
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
    let options = chrome::options("Tokativ settings", Size::new(px(WIDTH), px(HEIGHT)), cx);
    match cx.open_window(options, |window, cx| {
        window.set_rem_size(cx.theme().font_size);
        cx.new(|cx| {
            let mut settings = Settings::new(window, cx);
            let first = std::env::var(PAGE_VARIABLE)
                .ok()
                .and_then(|id| Page::from_id(&id));
            if let Some(page) = first {
                settings.go(page, window, cx);
            }
            settings
        })
    }) {
        Ok(handle) => cx.set_global(Open(handle)),
        Err(error) => log::error!("settings: cannot open the window: {error:#}"),
    }
}

/// A page of the settings window, one per entry in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Model,
    Find,
    Dictation,
    Style,
    General,
}

impl Page {
    /// The name `TOKATIV_SETTINGS_PAGE` takes.
    fn id(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Find => "find",
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

    const ALL: [Self; 5] = [
        Self::Model,
        Self::Find,
        Self::Dictation,
        Self::Style,
        Self::General,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Model => "Model",
            Self::Find => "Find models",
            Self::Dictation => "Dictation",
            Self::Style => "Writing style",
            Self::General => "General",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Model => "icons/library-big.svg",
            Self::Find => "icons/search.svg",
            Self::Dictation => "icons/mic-vocal.svg",
            Self::Style => "icons/pencil.svg",
            Self::General => "icons/settings.svg",
        }
    }
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
    _sampling: Task<()>,
    search_input: Entity<Input>,
    #[cfg(not(target_os = "linux"))]
    hotkey: Entity<crate::hotkey::Recorder>,
    replace_from: Entity<Input>,
    replace_to: Entity<Input>,
    profile_name: Entity<Input>,
    term: Entity<Input>,
    /// The query last sent, so a notify that did not change the text does not search again.
    searched: String,
    /// The search result whose files are listed.
    opened: Option<String>,
    page: Page,
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
        cx.observe(&host.store, |_, _, cx| cx.notify()).detach();
        cx.observe(&host.library, |_, _, cx| cx.notify()).detach();
        cx.observe(&host.status, |_, _, cx| cx.notify()).detach();
        let search_input =
            cx.new(|cx| Input::new("Search Hugging Face", cx).icon("icons/search.svg"));
        cx.observe(&search_input, |this: &mut Self, _, cx| {
            this.query_changed(cx)
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
            search_input,
            #[cfg(not(target_os = "linux"))]
            hotkey: cx.new(crate::hotkey::Recorder::new),
            replace_from: cx.new(|cx| Input::new("Heard as", cx)),
            replace_to: cx.new(|cx| Input::new("Write as", cx)),
            profile_name: cx.new(|cx| Input::new("Profile name", cx)),
            term: cx.new(|cx| Input::new("Add a name or term", cx)),
            searched: String::new(),
            opened: None,
            page: Page::Model,
            results_scrollbar: cx.new(|_| {
                Scrollbar::new(results_scroll.0.borrow().base_handle.clone()).watching(view)
            }),
            results_scroll,
            files_scrollbar: cx.new(|_| {
                Scrollbar::new(files_scroll.0.borrow().base_handle.clone()).watching(view)
            }),
            files_scroll,
            _debounce: None,
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

    fn dictation(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
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
        let device_picker = Picker::new("device", &self.popovers, device_name)
            .menu(device_menu("device-menu", &self.device_scrollbar))
            .item(default_item)
            .items(device_items.collect::<Vec<_>>());

        let mut rows =
            vec![setting("Microphone", device_picker.into_any_element(), cx).into_any_element()];
        // Drafts at the cursor need the Wayland input method. Elsewhere Tokativ registers the
        // hotkey itself, so that is the setting in its place.
        #[cfg(target_os = "linux")]
        {
            let live = config.live_preview;
            let live_switch = Switch::new("live-preview", live)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |config| config.live_preview = !live);
                }))
                .into_any_element();
            rows.insert(
                0,
                row(
                    "Live text",
                    "A draft at the cursor while you speak. Needs the input method.",
                    live_switch,
                    cx,
                )
                .into_any_element(),
            );
        }
        #[cfg(not(target_os = "linux"))]
        rows.insert(0, self.hotkey(cx));
        group(None, rows, cx)
    }

    /// The hotkey that starts and confirms a session, recorded by pressing it.
    #[cfg(not(target_os = "linux"))]
    fn hotkey(&self, cx: &mut Context<Self>) -> AnyElement {
        setting("Hotkey", self.hotkey.clone().into_any_element(), cx).into_any_element()
    }

    fn detection(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let silence = self
            .silence
            .dragging
            .unwrap_or_else(|| fraction(config.vad.silence_ms as f32, SILENCE_MS));
        let silence_ms = value(silence, SILENCE_MS).round() as u32;
        let threshold = self
            .threshold
            .dragging
            .unwrap_or_else(|| fraction(config.vad.threshold, THRESHOLD));
        let threshold_value = value(threshold, THRESHOLD);

        group(
            Some("Voice detection"),
            vec![
                setting(
                    "Pause length",
                    self.slider(
                        &self.silence,
                        silence,
                        format!("{silence_ms} ms"),
                        |this| &mut this.silence,
                        |config, fraction| {
                            config.vad.silence_ms = value(fraction, SILENCE_MS).round() as u32
                        },
                        cx,
                    ),
                    cx,
                )
                .into_any_element(),
                row(
                    "Speech threshold",
                    "Higher ignores more noise but may clip quiet words.",
                    self.slider(
                        &self.threshold,
                        threshold,
                        format!("{threshold_value:.2}"),
                        |this| &mut this.threshold,
                        |config, fraction| {
                            config.vad.threshold =
                                (value(fraction, THRESHOLD) * 100.).round() / 100.
                        },
                        cx,
                    ),
                    cx,
                )
                .into_any_element(),
            ],
            cx,
        )
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
    ) -> gpui::AnyElement {
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

    fn insertion(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status.read(cx);
        let note = status
            .note
            .as_ref()
            .map(|note| format!("Using {}, because {note}.", status.method.to_lowercase()));
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
        group(
            Some("Typing"),
            vec![match note {
                Some(note) => row("Method", note, picker.into_any_element(), cx).into_any_element(),
                None => setting("Method", picker.into_any_element(), cx).into_any_element(),
            }],
            cx,
        )
    }

    fn appearance(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
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
        group(
            Some("Appearance"),
            vec![
                setting("Theme", picker.into_any_element(), cx).into_any_element(),
                row(
                    "Custom themes",
                    "JSON files in the themes folder, in the format Sonora reads. A saved file shows up in the list right away.",
                    folder.into_any_element(),
                    cx,
                )
                .into_any_element(),
            ],
            cx,
        )
    }

    fn pill(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let edge = config.pill.edge;
        let caption = config.pill.caption;
        let cancel_button = config.pill.cancel_button;
        let confirm_button = config.pill.confirm_button;
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
        let margin = self
            .margin
            .dragging
            .unwrap_or_else(|| fraction(config.pill.margin, MARGIN));
        let pixels = value(margin, MARGIN).round();
        group(
            Some("Pill"),
            vec![
                setting("Position", picker.into_any_element(), cx).into_any_element(),
                setting(
                    "Distance from the edge",
                    self.slider(
                        &self.margin,
                        margin,
                        format!("{pixels} px"),
                        |this| &mut this.margin,
                        |config, fraction| config.pill.margin = value(fraction, MARGIN).round(),
                        cx,
                    ),
                    cx,
                )
                .into_any_element(),
                row(
                    "Live caption",
                    "A floating text window below the top pill or above the bottom pill.",
                    Switch::new("caption", caption)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.pill.caption = !caption);
                        }))
                        .into_any_element(),
                    cx,
                )
                .into_any_element(),
                row(
                    "Cancel button",
                    "The cross on the left. Escape cancels without it.",
                    Switch::new("cancel-button", cancel_button)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.pill.cancel_button = !cancel_button);
                        }))
                        .into_any_element(),
                    cx,
                )
                .into_any_element(),
                row(
                    "Confirm button",
                    "The check mark on the right. Enter or the hotkey finishes without it.",
                    Switch::new("confirm-button", confirm_button)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.pill.confirm_button = !confirm_button);
                        }))
                        .into_any_element(),
                    cx,
                )
                .into_any_element(),
            ],
            cx,
        )
    }

    fn system(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let login = config.launch_at_login;
        let tray = config.tray;
        let idle = config.models.unload_after_minutes;
        let idle_name = |minutes: u32| match minutes {
            0 => "Never".to_string(),
            minutes => format!("After {minutes} min"),
        };
        let idle_picker = Picker::new("idle", &self.popovers, idle_name(idle))
            .width(Picker::NARROW)
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
        let mut rows = vec![
            row(
                "Launch at login",
                "Not needed with the Home Manager service.",
                Switch::new("login", login)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| config.launch_at_login = !login);
                    }))
                    .into_any_element(),
                cx,
            )
            .into_any_element(),
            setting(
                "Tray icon",
                Switch::new("tray", tray)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| config.tray = !tray);
                    }))
                    .into_any_element(),
                cx,
            )
            .into_any_element(),
            row(
                "Unload models",
                "Frees memory between sessions. The next one starts a moment later.",
                idle_picker.into_any_element(),
                cx,
            )
            .into_any_element(),
        ];
        if cfg!(not(target_os = "linux")) {
            let automatic = config.check_for_updates;
            rows.push(
                setting(
                    "Automatic update checks",
                    Switch::new("automatic-updates", automatic)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |config| config.check_for_updates = !automatic)
                        }))
                        .into_any_element(),
                    cx,
                )
                .into_any_element(),
            );
            rows.push(
                setting(
                    "Updates",
                    Button::new("check-updates")
                        .label("Check for updates")
                        .secondary()
                        .on_click(|_, _, cx| Host::global(cx).dispatch(crate::Action::Updates, cx))
                        .into_any_element(),
                    cx,
                )
                .into_any_element(),
            );
        }
        group(Some("System"), rows, cx)
    }

    /// Every profile with the one in use marked, and the field that names a new profile or
    /// renames the one in use.
    fn profiles(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let active = config.active_profile().name.clone();
        let removable = config.profiles.len() > 1;
        let mut rows: Vec<AnyElement> = config
            .profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let name = profile.name.clone();
                let chosen = name == active;
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| {
                            if index < config.profiles.len() && config.profiles.len() > 1 {
                                config.profiles.remove(index);
                            }
                        });
                    }));
                div()
                    .id(SharedString::from(format!("profile-{index}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px(chrome::GROUP_PADDING)
                    .py_2()
                    .cursor_pointer()
                    .on_click(choose)
                    .child(Radio::new(
                        SharedString::from(format!("use-profile-{index}")),
                        chosen,
                    ))
                    .child(div().flex_1().min_w_0().child(name))
                    .when(removable, |this| {
                        this.child(div().flex_none().child(remove))
                    })
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
        rows.push(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px(chrome::GROUP_PADDING)
                .py_3()
                .child(div().flex_1().min_w_0().child(self.profile_name.clone()))
                .child(div().flex_none().child(rename))
                .child(div().flex_none().child(add))
                .into_any_element(),
        );
        group(Some("Profiles"), rows, cx)
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

    /// The rules the profile in use rewrites each phrase by before it is inserted.
    fn style(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let style = config.active_profile().clone();
        let capitals = style.capitals;
        let capitals_picker = Picker::new("capitals", &self.popovers, capitals.name())
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
        let periods = style.periods;
        let periods_picker = Picker::new("periods", &self.popovers, periods.name())
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
        let laughter = style.join_laughter;
        let spoken_emoji = style.spoken_emoji;
        let mut rows = vec![
            setting("Capitals", capitals_picker.into_any_element(), cx).into_any_element(),
            row(
                "Periods",
                "Question and exclamation marks always stay.",
                periods_picker.into_any_element(),
                cx,
            )
            .into_any_element(),
            row(
                "Spoken emoji",
                "“Thumbs up emoji” → 👍. Names in 140+ languages.",
                Switch::new("spoken-emoji", spoken_emoji)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| {
                            config.active_profile_mut().spoken_emoji = !spoken_emoji
                        });
                    }))
                    .into_any_element(),
                cx,
            )
            .into_any_element(),
            setting(
                "Laughter as one word",
                Switch::new("join-laughter", laughter)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| {
                            config.active_profile_mut().join_laughter = !laughter
                        });
                    }))
                    .into_any_element(),
                cx,
            )
            .into_any_element(),
        ];
        let theme = cx.theme();
        rows.extend(
            style
                .replacements
                .iter()
                .enumerate()
                .map(|(index, replacement)| {
                    let remove =
                        Button::new(SharedString::from(format!("remove-replacement-{index}")))
                            .icon("icons/x.svg")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |config| {
                                    let replacements =
                                        &mut config.active_profile_mut().replacements;
                                    if index < replacements.len() {
                                        replacements.remove(index);
                                    }
                                });
                            }));
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px(chrome::GROUP_PADDING)
                        .py_2()
                        .child(div().flex_1().min_w_0().child(replacement.from.clone()))
                        .child(
                            gpui::svg()
                                .path(icons::path("icons/chevron-right.svg"))
                                .size(px(14.))
                                .flex_none()
                                .text_color(theme.muted_foreground),
                        )
                        .child(div().flex_1().min_w_0().child(replacement.to.clone()))
                        .child(div().flex_none().child(remove))
                        .into_any_element()
                }),
        );
        let add = Button::new("add-replacement")
            .label("Add")
            .small()
            .outline()
            .on_click(cx.listener(|this, _, window, cx| this.add_replacement(window, cx)));
        rows.push(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px(chrome::GROUP_PADDING)
                .py_3()
                .child(div().flex_1().min_w_0().child(self.replace_from.clone()))
                .child(div().flex_1().min_w_0().child(self.replace_to.clone()))
                .child(div().flex_none().child(add))
                .into_any_element(),
        );
        group(Some(&style.name), rows, cx).into_any_element()
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

    /// The names and terms the model is asked to spell as written, and whether the model in use
    /// takes them at all.
    fn vocabulary(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ignored = self
            .status
            .read(cx)
            .abilities
            .is_some_and(|abilities| !abilities.vocabulary);
        let mut rows: Vec<AnyElement> = config
            .vocabulary
            .iter()
            .enumerate()
            .map(|(index, term)| {
                let remove = Button::new(SharedString::from(format!("remove-term-{index}")))
                    .icon("icons/x.svg")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |config| {
                            if index < config.vocabulary.len() {
                                config.vocabulary.remove(index);
                            }
                        });
                    }));
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px(chrome::GROUP_PADDING)
                    .py_2()
                    .child(div().flex_1().min_w_0().child(term.clone()))
                    .child(div().flex_none().child(remove))
                    .into_any_element()
            })
            .collect();
        let add = Button::new("add-term")
            .label("Add")
            .small()
            .outline()
            .on_click(cx.listener(|this, _, window, cx| this.add_term(window, cx)));
        rows.push(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px(chrome::GROUP_PADDING)
                .py_3()
                .child(div().flex_1().min_w_0().child(self.term.clone()))
                .child(div().flex_none().child(add))
                .into_any_element(),
        );
        if ignored {
            let name = ModelId::parse(&config.model)
                .map(|id| models::Described::of(id).name)
                .unwrap_or_else(|| "This model".to_owned());
            rows.push(
                div()
                    .px(chrome::GROUP_PADDING)
                    .py_3()
                    .text_size(theme.text(Text::Small))
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{name} does not use the vocabulary. Whisper and Qwen3-ASR do, and they also follow the text before each phrase."
                    ))
                    .into_any_element(),
            );
        }
        group(Some("Vocabulary"), rows, cx)
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

    /// The daemon's own memory and processor use, refreshed every second.
    fn resources(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let value = |text: String| {
            div()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let memory = chrome::size(self.usage.memory);
        let device = self.status.read(cx).device.clone();
        let on_gpu = device.as_ref().is_some_and(|device| device.gpu);
        let weights =
            ModelId::parse(&self.store.read(cx).config().model).and_then(|id| {
                match self.library.read(cx).state(&id) {
                    ModelState::Installed { bytes } => Some(bytes),
                    _ => None,
                }
            });
        let memory_detail = match on_gpu {
            true => "The app itself. The model's weights sit on the graphics card instead.",
            false => "The app and the model's weights, which run on the CPU.",
        };
        let cpu = match self.usage.cpu {
            Some(cpu) => format!("{cpu:.0}% of a core"),
            None => "Measuring".to_string(),
        };
        group(
            Some("Resources"),
            [
                Some(row("Memory", memory_detail, value(memory), cx).into_any_element()),
                weights.filter(|_| on_gpu).map(|bytes| {
                    setting(
                        "Graphics memory",
                        value(format!("about {}", chrome::size(bytes))),
                        cx,
                    )
                    .into_any_element()
                }),
                Some(setting("Processor", value(cpu), cx).into_any_element()),
            ]
            .into_iter()
            .flatten()
            .collect(),
            cx,
        )
    }

    /// The models to choose from, with the device they run on. Clicking an installed model makes
    /// it the one sessions use.
    fn models(&self, config: &Config, cx: &mut Context<Self>) -> impl IntoElement {
        let chosen = ModelId::parse(&config.model);
        let listed = models::listed(self.library.read(cx), chosen.as_slice());
        let mut rows: Vec<AnyElement> = listed
            .into_iter()
            .map(|model| {
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
                models::model_row(
                    model,
                    &self.library,
                    Some(radio),
                    Some(Box::new(choose)),
                    cx,
                )
            })
            .collect();

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
        rows.push(setting("Run on", picker.into_any_element(), cx).into_any_element());
        group(None, rows, cx)
    }

    /// Search over Hugging Face for any model the engine can run. Results and a repository's
    /// files are virtualized lists, so only the rows in view are drawn.
    fn find(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let library = self.library.read(cx);
        let search = &library.search;
        let field = div().flex_none().child(self.search_input.clone());

        if let Some(repo) = self.opened.clone() {
            let found = search
                .results
                .iter()
                .find(|repository| repository.id == repo)
                .cloned();
            let files = match search.files.get(&repo) {
                Some(Files::Loaded(files)) => Some(files.clone()),
                _ => None,
            };
            let waiting = match search.files.get(&repo) {
                None | Some(Files::Loading) => Some("Listing files".to_string()),
                Some(Files::Failed(error)) => Some(format!("Cannot list files: {error}")),
                Some(Files::Loaded(files)) if files.is_empty() => {
                    Some("No GGUF files in this repository".to_string())
                }
                Some(Files::Loaded(_)) => None,
            };
            let back = Button::new("back-to-results")
                .label("Results")
                .icon("icons/chevron-left.svg")
                .small()
                .ghost()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.opened = None;
                    cx.notify();
                }));
            let header = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().flex().child(back))
                .child(
                    div()
                        .text_size(theme.text(Text::Large))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(repo.clone()),
                )
                .when_some(found.as_ref(), |this, repository| {
                    this.child(models::facts(repository, cx))
                });
            let list = match files {
                Some(files) => {
                    let count = files.len();
                    let repo = repo.clone();
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
                None => hint(&waiting.unwrap_or_default(), cx),
            };
            return div()
                .flex()
                .flex_col()
                .gap_4()
                .size_full()
                .child(header)
                .child(list)
                .into_any_element();
        }

        let note = if search.query.trim().is_empty() {
            Some("Search for Whisper, Parakeet, Canary, GigaAM, Moonshine, Qwen3-ASR, Voxtral and more. Only models in GGUF form that run here are listed.".to_string())
        } else if search.loading && search.results.is_empty() {
            Some("Searching".to_string())
        } else if let Some(error) = &search.error {
            Some(format!("Search failed: {error}"))
        } else if search.results.is_empty() {
            Some("No models that run here match that.".to_string())
        } else {
            None
        };
        let results = search.results.clone();
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
        div()
            .flex()
            .flex_col()
            .gap_4()
            .size_full()
            .child(field)
            .child(list)
            .into_any_element()
    }

    /// Searches once typing pauses, so each keystroke does not reach the network.
    fn query_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text().to_owned();
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

    /// The sidebar: the app's mark, one entry per page, and what the engine runs on.
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mark = theme.metrics.control;
        let current = self.page;
        let device = self.status.read(cx).device.clone();
        let entries = Page::ALL.into_iter().map(|page| {
            let selected = page == current;
            div()
                .id(SharedString::from(format!("page-{}", page.title())))
                .flex()
                .items_center()
                .gap_3()
                .h(theme.metrics.control)
                .px_3()
                .rounded(theme.radius)
                .squircle()
                .cursor_pointer()
                .text_color(match selected {
                    true => theme.foreground,
                    false => theme.muted_foreground,
                })
                .when(selected, |this| this.bg(theme.sidebar_accent))
                .when(!selected, |this| {
                    this.hover(|style| style.bg(theme.secondary.opacity(0.6)))
                })
                .on_click(cx.listener(move |this, _, window, cx| this.go(page, window, cx)))
                .child(
                    svg()
                        .path(icons::path(page.icon()))
                        .size(px(16.))
                        .flex_none()
                        .text_color(match selected {
                            true => theme.foreground,
                            false => theme.muted_foreground,
                        }),
                )
                .child(page.title())
        });
        div()
            .flex_none()
            .w(SIDEBAR)
            .h_full()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .pt_2()
                    .pb_4()
                    .child(
                        div()
                            .flex_none()
                            .size(mark)
                            .rounded(mark / 2.)
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme.foreground)
                            .child(
                                svg()
                                    .path(icons::path("icons/mic-vocal.svg"))
                                    .size(mark * 0.5)
                                    .text_color(theme.background),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Tokativ"),
                            )
                            .child(
                                div()
                                    .text_size(theme.text(Text::Small))
                                    .text_color(theme.muted_foreground)
                                    .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                            ),
                    ),
            )
            .children(entries)
            .child(div().flex_1())
            .when_some(device, |this, device| {
                this.child(
                    div()
                        .px_2()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.muted_foreground)
                        .child(format!("Running on {}", device.name)),
                )
            })
    }

    /// Switches page and starts the new one at its top. The search page puts the cursor in its
    /// field, since typing is the only thing to do there.
    fn go(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            self.scrollbar
                .read(cx)
                .scroll()
                .set_offset(gpui::Point::default());
            cx.notify();
        }
        if page == Page::Find {
            let focus = self.search_input.focus_handle(cx);
            focus.focus(window, cx);
        }
    }

    /// The groups of the current page.
    fn page(&self, config: &Config, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match self.page {
            Page::Model => vec![
                self.models(config, cx).into_any_element(),
                self.vocabulary(config, cx).into_any_element(),
            ],
            Page::Find => vec![self.find(cx)],
            Page::Dictation => vec![
                self.dictation(config, cx).into_any_element(),
                self.detection(config, cx).into_any_element(),
                self.insertion(config, cx).into_any_element(),
            ],
            Page::Style => vec![
                self.profiles(config, cx).into_any_element(),
                self.style(config, cx).into_any_element(),
            ],
            Page::General => vec![
                self.appearance(config, cx).into_any_element(),
                self.pill(config, cx).into_any_element(),
                self.system(config, cx).into_any_element(),
                self.resources(cx).into_any_element(),
            ],
        }
    }
}

/// A virtualized list on a group's raised surface, filling the space left on the page.
fn listed(list: AnyElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .p_1()
        .rounded(chrome::GROUP_RADIUS)
        .squircle()
        .bg(theme.secondary.opacity(0.45))
        .border_1()
        .border_color(theme.border.opacity(0.6))
        .child(list)
        .into_any_element()
}

/// A muted line under the search field, standing in for a list that has nothing to show yet.
fn hint(text: &str, cx: &App) -> AnyElement {
    div()
        .px_1()
        .text_size(cx.theme().text(Text::Small))
        .text_color(cx.theme().muted_foreground)
        .child(text.to_owned())
        .into_any_element()
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let config = self.store.read(cx).config().clone();
        let page = self.page;
        let heading = div()
            .flex_none()
            .pt_6()
            .pb_4()
            .text_size(theme.text(Text::Title))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(page.title());
        let groups = self.page(&config, cx);
        // The search page lays out its own lists, which scroll themselves, so it fills the
        // height instead of sitting in the page scroller.
        let body = match page {
            Page::Find => div()
                .flex()
                .flex_col()
                .size_full()
                .px_8()
                .pb_6()
                .child(heading)
                .children(groups)
                .into_any_element(),
            _ => Scroller::new("settings", &self.scrollbar)
                .child(
                    div()
                        .w_full()
                        .px_8()
                        .pb_8()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(heading)
                        .children(groups),
                )
                .into_any_element(),
        };
        let content = div()
            .flex()
            .size_full()
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(body),
            )
            .into_any_element();
        chrome::frame("Settings", content, window, cx)
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
