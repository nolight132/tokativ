//! The first run: download models, pick a microphone, try a dictation, and copy the niri
//! keybind. The settings window shows it in place of the settings until it is finished, so
//! nothing else can be reached before a model is in place.

use gpui::Focusable as _;
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, App, Context, Entity, ScrollHandle, SharedString, Task, Window,
    div, px, relative, svg,
};
use std::time::Instant;

use tokativ_audio::{InputDevice, Levels, Recorder};
use tokativ_engine::ModelId;
use ui::{
    ActiveTheme as _, Button, Input, MenuItem, Picker, Popovers, Radio, Scrollbar, Scroller, Text,
};

use crate::chrome::{self, meter};
use crate::desktop::{Bound, Desktop};
use crate::host::{Action, Host, Library, Status, Store};
use crate::models;
use crate::settings::device_menu;

/// Names the step the setup opens on, for working on one step in the sandbox.
pub const STEP_VARIABLE: &str = "TOKATIV_SETUP_STEP";
/// The height of a step segment and how long the newest one takes to fill.
const SEGMENT: gpui::Pixels = px(6.);
const FILL: std::time::Duration = std::time::Duration::from_millis(420);
/// The level meter spans this many dBFS up to full scale, and settles up and down with these
/// time constants in seconds.
const METER_FLOOR: f32 = -60.;
/// How often the hotkey step looks at the desktop config again.
const BINDING_POLL: std::time::Duration = std::time::Duration::from_secs(1);
const METER_ATTACK: f32 = 0.03;
const METER_RELEASE: f32 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Models,
    Microphone,
    Try,
    Hotkey,
}

impl Step {
    const ALL: [Self; 4] = [Self::Models, Self::Microphone, Self::Try, Self::Hotkey];

    /// The name the sandbox's `--step` flag takes.
    fn id(self) -> &'static str {
        match self {
            Self::Models => "models",
            Self::Microphone => "microphone",
            Self::Try => "try",
            Self::Hotkey => "hotkey",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Models => "Models",
            Self::Microphone => "Microphone",
            Self::Try => "Try it",
            Self::Hotkey => "Hotkey",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Models => "icons/library-big.svg",
            Self::Microphone => "icons/mic-vocal.svg",
            Self::Try => "icons/text-select.svg",
            Self::Hotkey => "icons/sliders-horizontal.svg",
        }
    }

    /// One line on what the step asks for.
    fn summary(self) -> &'static str {
        match self {
            Self::Models => "Choose the speech model to dictate with",
            Self::Microphone => "Choose what to listen to and check the level",
            Self::Try => "Dictate a sentence into the field below",
            Self::Hotkey => "Bind a key to start and finish dictating",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }
}

pub struct Setup {
    store: Entity<Store>,
    library: Entity<Library>,
    status: Entity<Status>,
    step: Step,
    popovers: Popovers,
    devices: Vec<InputDevice>,
    /// Feeds the level meter while the microphone step is shown.
    monitor: Option<Recorder>,
    /// The step left by the last move, which tells the progress bar which way to animate.
    from: Option<Step>,
    /// Counts moves between steps.
    moves: u32,
    desktop: Desktop,
    #[cfg(not(target_os = "linux"))]
    recorder: Entity<crate::hotkey::Recorder>,
    /// The binding found in the desktop's config, looked for again while the hotkey step shows.
    bound: Option<Bound>,
    /// A session started while the hotkey step showed, so the key is known to work.
    pressed: bool,
    _watching: Option<Task<()>>,
    /// What the meter reads, shared with the recorder as it opens.
    levels: Levels,
    /// Counts microphone openings, so one that finishes late can tell it is stale.
    listening: u64,
    meter_shown: f32,
    meter_beat: Option<Instant>,
    field: Entity<Input>,
    scrollbar: Entity<Scrollbar>,
    device_scrollbar: Entity<Scrollbar>,
}

impl Setup {
    /// A setup on its first step, or on the step `TOKATIV_SETUP_STEP` names.
    pub(crate) fn begin(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut setup = Self::new(window, cx);
        let first = std::env::var(STEP_VARIABLE)
            .ok()
            .and_then(|id| Step::ALL.into_iter().find(|step| step.id() == id))
            .unwrap_or(Step::Models);
        setup.go(first, window, cx);
        setup
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = Host::global(cx);
        cx.observe(&host.store, |_, _, cx| cx.notify()).detach();
        cx.observe(&host.library, |_, _, cx| cx.notify()).detach();
        cx.observe(&host.status, |this, status, cx| {
            if this.step == Step::Hotkey && status.read(cx).recording {
                this.pressed = true;
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_appearance(window, |_, _, cx| {
            crate::refresh_theme(cx);
            cx.notify();
        })
        .detach();
        let field = cx.new(|cx| Input::new("Your words appear here", cx));
        let view = cx.entity_id();
        Self {
            store: host.store,
            library: host.library,
            status: host.status,
            step: Step::Models,
            popovers: Popovers::default(),
            devices: tokativ_audio::input_devices(),
            monitor: None,
            from: None,
            moves: 0,
            desktop: Desktop::detect(),
            #[cfg(not(target_os = "linux"))]
            recorder: cx.new(crate::hotkey::Recorder::new),
            bound: None,
            pressed: false,
            _watching: None,
            levels: Levels::default(),
            listening: 0,
            meter_shown: 0.,
            meter_beat: None,
            field,
            scrollbar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(view)),
            device_scrollbar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(view)),
        }
    }

    fn go(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.from = (self.moves > 0).then_some(self.step);
        self.moves += 1;
        self.step = step;
        self.monitor = None;
        self._watching = None;
        match step {
            Step::Hotkey => self.watch_binding(cx),
            Step::Microphone => self.listen(cx),
            Step::Try => {
                let focus = self.field.focus_handle(cx);
                focus.focus(window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Opens the chosen microphone for the level meter.
    /// Opens the chosen microphone for the level meter. Opening takes a moment, so it happens in
    /// the background, and a device that opens after the step was left or another was chosen is
    /// closed again.
    fn listen(&mut self, cx: &mut Context<Self>) {
        self.monitor = None;
        self.listening += 1;
        let generation = self.listening;
        self.levels = Levels::default();
        let levels = self.levels.clone();
        let device = self.store.read(cx).config().input_device.clone();
        let timing = tokativ_audio::Timing {
            threshold: 0.5,
            silence_ms: 500,
            max_segment_s: 20,
            preroll_ms: 200,
        };
        let opening = cx.background_spawn(async move {
            Recorder::start(
                tokativ_audio::Options {
                    device,
                    timing,
                    progress: None,
                },
                None,
                levels,
                |_| {},
            )
        });
        cx.spawn(async move |this, cx| {
            let opened = opening.await;
            this.update(cx, |this, _| match opened {
                Ok(recorder) if this.listening == generation && this.step == Step::Microphone => {
                    this.monitor = Some(recorder);
                }
                Ok(recorder) => recorder.cancel(),
                Err(error) => log::error!("setup: cannot open the microphone: {error:#}"),
            })
            .ok();
        })
        .detach();
    }

    /// Looks for a binding now and every second while the hotkey step shows, so one added in
    /// another window is picked up without leaving the step.
    fn watch_binding(&mut self, cx: &mut Context<Self>) {
        let desktop = self.desktop;
        self._watching = Some(cx.spawn(async move |this, cx| {
            loop {
                let bound = cx.background_spawn(async move { desktop.bound() }).await;
                let alive = this.update(cx, |this, cx| {
                    if this.bound != bound {
                        this.bound = bound;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
                cx.background_executor().timer(BINDING_POLL).await;
            }
        }));
    }

    /// Moves the meter towards the input's loudness: up at once so peaks register, down slowly
    /// so the eye can follow, at a rate set by the real time between frames.
    fn meter(&mut self) -> f32 {
        let now = Instant::now();
        let elapsed = self
            .meter_beat
            .replace(now)
            .map_or(0., |beat| (now - beat).as_secs_f32().min(0.1));
        let target = ((self.levels.loudness() - METER_FLOOR) / -METER_FLOOR).clamp(0., 1.);
        let settle = match target > self.meter_shown {
            true => METER_ATTACK,
            false => METER_RELEASE,
        };
        self.meter_shown += (target - self.meter_shown) * (1. - (-elapsed / settle).exp());
        self.meter_shown
    }

    fn models(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chosen = ModelId::parse(&self.store.read(cx).config().model);
        let listed = models::listed(self.library.read(cx), chosen.as_slice(), true);

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(intro(
                "It runs on this computer, and nothing you say leaves it.",
                cx,
            ))
            .children(listed.into_iter().map(|model| {
                let id = model.id.clone();
                let picked = Some(&id) == chosen.as_ref();
                let radio = Radio::new(SharedString::from(format!("pick-{}", id.key())), picked)
                    .into_any_element();
                let key = id.key();
                let choose = cx.listener(move |this, _, _, cx| {
                    let key = key.clone();
                    this.store.update(cx, |store, cx| {
                        store.update(|config| config.model = key, cx)
                    });
                });
                div()
                    .rounded(chrome::GROUP_RADIUS)
                    .squircle()
                    .border_1()
                    .border_color(match picked {
                        true => cx.theme().foreground.opacity(0.5),
                        false => cx.theme().border.opacity(0.6),
                    })
                    .child(models::model_row(
                        model,
                        &self.library,
                        Some(radio),
                        Some(Box::new(choose)),
                        chrome::GROUP_PADDING,
                        crate::nested_radius(chrome::GROUP_RADIUS, px(1.)),
                        cx,
                    ))
            }))
    }

    fn microphone(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let device = self.store.read(cx).config().input_device.clone();
        let name = device
            .as_ref()
            .and_then(|id| self.devices.iter().find(|known| &known.id == id))
            .map(|known| known.name.clone())
            .unwrap_or_else(|| "System default".into());
        let mut items = vec![
            MenuItem::new("device-default", "System default")
                .selected(device.is_none())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.store.update(cx, |store, cx| {
                        store.update(|config| config.input_device = None, cx)
                    });
                    this.listen(cx);
                })),
        ];
        items.extend(self.devices.iter().map(|known| {
            let id = known.id.clone();
            MenuItem::new(
                SharedString::from(format!("device-{}", known.id)),
                known.name.clone(),
            )
            .selected(device.as_deref() == Some(&known.id))
            .on_click(cx.listener(move |this, _, _, cx| {
                let id = id.clone();
                this.store.update(cx, |store, cx| {
                    store.update(|config| config.input_device = Some(id), cx)
                });
                this.listen(cx);
            }))
        }));
        let level = self.meter();
        window.request_animation_frame();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                Picker::new("setup-device", &self.popovers, name)
                    .menu(device_menu("setup-device-menu", &self.device_scrollbar))
                    .items(items),
            )
            .child(meter(level, px(12.), cx))
    }

    fn try_it(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let recording = self.status.read(cx).recording;
        let field = self.field.clone();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(intro(
                "Press Start, speak, then press the check mark on the pill.",
                cx,
            ))
            .child(self.field.clone())
            .child(
                div().flex().gap_2().child(
                    Button::new("try")
                        .label(match recording {
                            true => "Finish",
                            false => "Start dictation",
                        })
                        .primary()
                        .on_click(move |_, window, cx| {
                            let focus = field.focus_handle(cx);
                            focus.focus(window, cx);
                            Host::global(cx).dispatch(Action::Toggle, cx);
                        }),
                ),
            )
            .when(!window.is_window_active(), |this| {
                this.child(intro(
                    "Keep this window focused so the text lands in the field.",
                    cx,
                ))
            })
    }

    /// Directions for binding the toggle, written for the desktop this session runs in. The step
    /// watches the config for a binding and confirms once the key actually starts a session.
    #[cfg(target_os = "linux")]
    fn hotkey(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let desktop = self.desktop;
        let snippet = desktop.snippet();
        let (icon, status, ready) = match (&self.bound, self.pressed) {
            (_, true) => (
                "icons/circle-check.svg",
                "The key works. Press it once more to close the pill.".to_string(),
                true,
            ),
            (Some(bound), false) => (
                "icons/info.svg",
                format!(
                    "Found {} in {}. Press it now to check it.",
                    bound.keys,
                    bound
                        .file
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                false,
            ),
            (None, false) => (
                "icons/info.svg",
                "No key runs Tokativ yet. Add one, then press it here to check it.".to_string(),
                false,
            ),
        };
        let status = check(icon, status, ready, cx);

        let config = desktop.config_file();
        let actions = div()
            .flex()
            .gap_2()
            .child(
                Button::new("copy-bind")
                    .label("Copy")
                    .icon("icons/copy.svg")
                    .small()
                    .outline()
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(snippet.to_owned()))
                    }),
            )
            .when_some(config, |this, config| {
                this.child(
                    Button::new("open-config")
                        .label("Open config")
                        .icon("icons/external-link.svg")
                        .small()
                        .outline()
                        .on_click(move |_, _, _| crate::desktop::open(&config)),
                )
            });

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(status)
            .child(
                div()
                    .pt_2()
                    .child(ui::eyebrow(format!("{} detected", desktop.name()), cx)),
            )
            .child(intro(desktop.directions(), cx))
            .when(desktop.supported(), |this| {
                this.child(chrome::code(snippet, cx)).child(actions)
            })
    }

    /// The hotkey Tokativ registers itself on macOS and Windows. The step confirms once the key
    /// starts a session.
    #[cfg(not(target_os = "linux"))]
    fn hotkey(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hotkey = crate::hotkey::label(&self.store.read(cx).config().hotkey);
        let (icon, status, ready) = match self.pressed {
            true => (
                "icons/circle-check.svg",
                "The key works. Press it once more to close the pill.".to_string(),
                true,
            ),
            false => (
                "icons/info.svg",
                format!("Press {hotkey} to check it."),
                false,
            ),
        };
        let permission = match cfg!(target_os = "macos") {
            true => {
                " macOS asks for Accessibility access the first time Tokativ types, and the text only arrives once it is allowed."
            }
            false => "",
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(check(icon, status, ready, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child("Hotkey")
                    .child(self.recorder.clone()),
            )
            .child(intro(
                format!(
                    "Tokativ listens for this key in every app. Click it to record another.{permission}"
                ),
                cx,
            ))
    }

    fn can_advance(&self, cx: &App) -> bool {
        match self.step {
            Step::Models => ModelId::parse(&self.store.read(cx).config().model)
                .is_some_and(|id| self.library.read(cx).installed(&id)),
            _ => true,
        }
    }

    /// Marks the setup done. The settings window sees that and shows the settings in its place.
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.monitor = None;
        self.store.update(cx, |store, cx| {
            store.update(|config| config.setup_done = true, cx)
        });
    }
}

fn intro(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_color(cx.theme().muted_foreground)
        .text_size(cx.theme().text(Text::Small))
        .child(text.into())
}

/// A line in a rounded frame saying where the hotkey check stands. The frame and icon brighten
/// once the key has been seen to work.
fn check(icon: &str, status: String, ready: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_2()
        .px(chrome::GROUP_PADDING)
        .py_3()
        .rounded(chrome::GROUP_RADIUS)
        .squircle()
        .border_1()
        .border_color(match ready {
            true => theme.foreground.opacity(0.5),
            false => theme.border.opacity(0.6),
        })
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(16.))
                .flex_none()
                .text_color(match ready {
                    true => theme.foreground,
                    false => theme.muted_foreground,
                }),
        )
        .child(div().text_size(theme.text(Text::Small)).child(status))
}

/// The step header: the step's icon, name and number, and a bar of one segment per step. Moving
/// forward fills the new step's segment, moving back drains the segment of the step left.
fn progress(step: Step, from: Option<Step>, moves: u32, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let index = step.index();
    let count = Step::ALL.len();
    let back = from.is_some_and(|from| from.index() > index);
    let segments = Step::ALL.into_iter().map(move |each| {
        let fill = move || div().h_full().rounded_full().bg(theme.progress_bar);
        // Each move gets fresh animation ids, so going back and forth replays the motion.
        let animated = |id: &str, grow: bool| {
            fill().with_animation(
                SharedString::from(format!("segment-{id}-{moves}")),
                Animation::new(FILL).with_easing(ui::ease_out_cubic),
                move |fill, delta| match grow {
                    true => fill.w(relative(delta)),
                    false => fill.w(relative(1. - delta)),
                },
            )
        };
        let segment = div()
            .flex_1()
            .h(SEGMENT)
            .rounded_full()
            .bg(theme.muted)
            .overflow_hidden();
        if each.index() < index || (each == step && back) {
            segment.child(fill().w_full())
        } else if each == step {
            segment.child(animated("fill", true))
        } else if back && Some(each) == from {
            segment.child(animated("drain", false))
        } else {
            segment
        }
    });
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p(chrome::GROUP_PADDING)
        .rounded(chrome::GROUP_RADIUS)
        .squircle()
        .bg(theme.secondary.opacity(0.45))
        .border_1()
        .border_color(theme.border.opacity(0.6))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(icons::path(step.icon()))
                        .size(px(16.))
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(step.title()),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}/{count}", index + 1)),
                ),
        )
        .child(
            div()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(step.summary()),
        )
        .child(div().flex().gap_1().children(segments))
}

impl Render for Setup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let step = self.step;
        let index = step.index();
        let last = index == Step::ALL.len() - 1;

        let body = match step {
            Step::Models => self.models(cx).into_any_element(),
            Step::Microphone => self.microphone(window, cx).into_any_element(),
            Step::Try => self.try_it(window, cx).into_any_element(),
            Step::Hotkey => self.hotkey(cx).into_any_element(),
        };
        let advance = self.can_advance(cx);

        let footer = div()
            .flex()
            .justify_between()
            .items_center()
            .child(
                Button::new("back")
                    .label("Back")
                    .outline()
                    .disabled(index == 0)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if index > 0 {
                            this.go(Step::ALL[index - 1], window, cx);
                        }
                    })),
            )
            .child(
                Button::new("next")
                    .label(match last {
                        true => "Finish",
                        false => "Next",
                    })
                    .primary()
                    .disabled(!advance)
                    .on_click(cx.listener(move |this, _, window, cx| match last {
                        true => this.finish(cx),
                        false => this.go(Step::ALL[index + 1], window, cx),
                    })),
            );

        div()
            .id("setup")
            .size_full()
            .flex()
            .flex_col()
            .px_6()
            .pb_6()
            .gap_4()
            .pt_4()
            .child(progress(step, self.from, self.moves, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(Scroller::new("setup-body", &self.scrollbar).child(body)),
            )
            .child(footer)
    }
}
