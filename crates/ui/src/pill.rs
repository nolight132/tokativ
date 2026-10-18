//! The floating pill shown while a dictation session runs: cancel on the left, live bars in the
//! middle, confirm on the right. It lives on its own layer surface on Wayland and in a floating
//! window elsewhere, and never takes keyboard focus, so the app being dictated into keeps it.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Bounds, BoxShadow, Context, Hsla, Pixels, Size, SpringConfig, Window, WindowHandle,
    canvas, div, point, px, svg,
};
use tokativ_audio::{BANDS, Levels};
use tokativ_config::{Bars, Pill as PillConfig};
use ui::ActiveTheme as _;

use crate::overlay;
use crate::radius::nested_radius;
use crate::spring::Spring;

const HEIGHT_PX: f32 = 44.;
pub(crate) const HEIGHT: Pixels = px(HEIGHT_PX);
/// The gap between the pill's edge and its buttons, the same on every side a button touches.
const PADDING_PX: f32 = 6.;
const PADDING: Pixels = px(PADDING_PX);
/// The pill's outline, which counts towards the padding so the buttons sit exactly `PADDING`
/// from the outer edge.
const BORDER: Pixels = px(1.);
/// Covers shadow_lg's offset and full Gaussian tail (10 - 3 + 3 * 15 = 52px),
/// plus room for the opening spring. Clicks in this margin pass through.
pub(crate) const SHADOW: Pixels = px(56.);
const BARS: usize = BANDS;
const BAR_WIDTH_PX: f32 = 3.;
const BAR_GAP_PX: f32 = 4.;
/// The space between each button and the nearest bar.
const BAR_MARGIN_PX: f32 = 12.;
/// The diameter of a button, which fills the pill's height inside the padding.
const BUTTON_PX: f32 = HEIGHT_PX - PADDING_PX * 2.;
/// The bars and the room on either side of them.
const BARS_SPAN_PX: f32 =
    BAR_MARGIN_PX * 2. + BAR_WIDTH_PX * BARS as f32 + BAR_GAP_PX * (BARS - 1) as f32;
/// The tallest a bar grows, as a share of the pill's height.
const BAR_REACH: f32 = 0.62;
/// How long the bars take to settle a change, as the time constant of their easing.
const SETTLE: Duration = Duration::from_millis(45);
const PULSE: Duration = Duration::from_millis(1100);
const ERROR_HOLD: Duration = Duration::from_millis(1400);
const ICON: Pixels = px(16.);
/// The capsule's opening: quick, with a little overshoot so it lands with a bounce.
const OPEN: SpringConfig = SpringConfig::new(320., 19., 1.);
/// The buttons popping in once the capsule has room for them.
const POP: SpringConfig = SpringConfig::new(420., 22., 1.);
/// Closing is near critically damped, so the capsule folds away without wobbling.
const CLOSE: SpringConfig = SpringConfig::new(520., 44., 1.);
/// How long after opening the buttons start to pop in.
const CONTROLS_DELAY: Duration = Duration::from_millis(70);
/// When the middle bars light up, and how much later each bar further out follows.
const BARS_DELAY: Duration = Duration::from_millis(110);
const BARS_STAGGER: Duration = Duration::from_millis(24);
const BAR_REVEAL: Duration = Duration::from_millis(200);
/// How fast the bars go out when the pill closes, before the capsule folds.
const BARS_FADE: Duration = Duration::from_millis(90);
/// How much of the spring's overshoot past full width shows as growth in scale, once the
/// capsule has reached its full width.
const OVERSHOOT_SCALE: f32 = 0.5;
/// The longest a close may take before the window goes regardless.
const CLOSE_LIMIT: Duration = Duration::from_millis(700);
/// How small and how blurred the capsule is when folded.
const FOLDED_SCALE: f32 = 0.82;
const FOLDED_BLUR: Pixels = px(8.);
/// How much wider each pair of bars beyond the bands makes the pill.
pub(crate) const STRETCH_STEP: f32 = (BAR_WIDTH_PX + BAR_GAP_PX) * 2.;
/// The most the pill can be made wider than it needs.
pub(crate) const MAX_STRETCH: f32 = STRETCH_STEP * 17.;

/// `stretch` rounded to whole pairs of bars, so a pill at rest never shows a pair half in.
pub(crate) fn snap_stretch(stretch: f32) -> f32 {
    ((stretch / STRETCH_STEP).round() * STRETCH_STEP).clamp(0., MAX_STRETCH)
}

/// Wide enough for the buttons shown and the bars beside them, and `stretch` wider.
pub(crate) fn width(cancel: bool, confirm: bool, stretch: f32) -> Pixels {
    let buttons = cancel as u8 + confirm as u8;
    px(
        PADDING_PX * 2.
            + BUTTON_PX * buttons as f32
            + BARS_SPAN_PX
            + stretch.clamp(0., MAX_STRETCH),
    )
}

/// The capsule `width` wide as it looks at rest, for the buttons and the bars to go in, drawn at
/// `scale`. The pill moves it as it opens and closes, and the settings show it still or shrunk
/// onto a drawing of the screen. Drawing it small rather than shrinking a full-size picture keeps
/// its edges sharp.
pub(crate) fn capsule(width: Pixels, scale: f32, cx: &App) -> gpui::Div {
    face(width * scale, HEIGHT * scale, cx)
        .flex()
        .items_center()
        .justify_between()
        .p((PADDING * scale - BORDER).max(Pixels::ZERO))
        .shadow(shadow(scale))
        .overflow_hidden()
}

/// The capsule's fill and outline alone, `width` by `height`, without its shadow or contents.
pub(crate) fn face(width: Pixels, height: Pixels, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .w(width)
        .h(height)
        .rounded(height / 2.)
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
}

/// Tailwind's large shadow, which the pill and the caption cast, at `scale`.
pub(crate) fn shadow(scale: f32) -> Vec<BoxShadow> {
    let color = gpui::hsla(0., 0., 0., 0.1);
    [(10., 15., -3.), (4., 6., -4.)]
        .into_iter()
        .map(|(offset, blur, spread)| {
            BoxShadow::new(px(0.), px(offset * scale), color)
                .blur_radius(px(blur * scale))
                .spread_radius(px(spread * scale))
        })
        .collect()
}

/// How wide the bars and the room on either side of them are.
pub(crate) fn bars_span() -> Pixels {
    px(BARS_SPAN_PX)
}

/// The diameter of a button, inside the capsule's padding.
pub(crate) fn button_size() -> Pixels {
    nested_radius(HEIGHT / 2., PADDING) * 2.
}

/// The cross on the left that cancels the session, drawn at `scale`.
pub(crate) fn cancel_button(
    id: impl Into<gpui::ElementId>,
    scale: f32,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    circle(
        id,
        button_size() * scale,
        theme.secondary,
        theme.secondary_hover,
    )
    .child(icon("icons/x.svg", ICON * scale, theme.foreground))
}

/// The check mark on the right that finishes the session, drawn at `scale`.
pub(crate) fn confirm_button(
    id: impl Into<gpui::ElementId>,
    scale: f32,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    circle(
        id,
        button_size() * scale,
        theme.foreground,
        theme.foreground.opacity(0.85),
    )
    .child(icon("icons/check.svg", ICON * scale, theme.background))
}

/// Where the pill takes clicks: the capsule at full width, not the room around it for the shadow.
fn region(width: Pixels) -> Bounds<Pixels> {
    Bounds {
        origin: point(SHADOW, SHADOW),
        size: Size {
            width,
            height: HEIGHT,
        },
    }
}

/// What the session is doing, which is what the bars show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Listening,
    /// Capture has stopped and the last segments are being transcribed.
    Processing,
    /// Something failed. The indicator turns red and the pill closes shortly after.
    Error,
}

type Handler = Rc<dyn Fn(&mut App)>;

pub struct Pill {
    levels: Levels,
    phase: Phase,
    shown: [f32; BARS],
    beat: Option<Instant>,
    since: Instant,
    on_confirm: Handler,
    on_cancel: Handler,
    opened: Instant,
    /// When the pill started to close, if it has.
    closing: Option<Instant>,
    /// How far the capsule has unfolded, from a dot at 0 to full width at 1.
    body: Spring,
    /// How far the buttons have popped in.
    controls: Spring,
    /// The pill has folded away and waits, invisible and click-through, for the next session.
    /// Keeping the surface instead of closing it lets the next session show its first frame at
    /// once, where a new layer surface takes over a hundred milliseconds to appear.
    resting: bool,
    /// The capsule has reached its full width since it last started to open. From then on its
    /// width holds and the spring's bounce shows as scale, which moves by fractions of a pixel
    /// where the width can only step by whole ones.
    widened: bool,
    /// The capsule's width once open.
    width: Pixels,
    cancel: bool,
    confirm: bool,
    layout: Bars,
}

/// Opens the pill on the focused output where `config` places it, with the buttons it asks for.
/// `levels` feeds the bars, and the handlers run when the buttons are clicked. A `resting` pill starts folded away, ready to unfold at once.
pub fn open(
    levels: Levels,
    config: &PillConfig,
    on_confirm: impl Fn(&mut App) + 'static,
    on_cancel: impl Fn(&mut App) + 'static,
    resting: bool,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Pill>> {
    let width = width(
        config.cancel_button,
        config.confirm_button,
        snap_stretch(config.stretch),
    );
    let (cancel, confirm) = (config.cancel_button, config.confirm_button);
    let layout = config.bars;
    let size = Size::new(width + SHADOW * 2., HEIGHT + SHADOW * 2.);
    // The shadow margin is part of the surface, so the visible gap to the edge takes it off.
    let gap = px(config.margin) - SHADOW;
    let options = overlay::options(config.edge, gap, size, None, cx);
    let on_confirm: Handler = Rc::new(on_confirm);
    let on_cancel: Handler = Rc::new(on_cancel);
    cx.open_window(options, move |window, cx| {
        overlay::prepare(window, false);
        let region = [region(width)];
        window.set_input_region(Some(match resting {
            true => &[],
            false => &region,
        }));
        cx.new(|_| Pill {
            levels,
            phase: Phase::Listening,
            shown: [0.; BARS],
            beat: None,
            since: Instant::now(),
            on_confirm,
            on_cancel,
            opened: Instant::now(),
            closing: None,
            body: Spring::new(OPEN),
            controls: Spring::new(POP),
            resting,
            widened: false,
            width,
            cancel,
            confirm,
            layout,
        })
    })
}

impl Pill {
    pub fn set_phase(&mut self, phase: Phase, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase == phase {
            return;
        }
        self.phase = phase;
        self.since = Instant::now();
        if phase == Phase::Error {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(ERROR_HOLD).await;
                this.update_in(cx, |pill, _, cx| pill.dismiss(cx)).ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// Unfolds the pill again for a new session, from wherever its folding has got to, so a
    /// quick second press turns the motion around instead of starting over.
    pub fn reopen(&mut self, levels: Levels, window: &mut Window, cx: &mut Context<Self>) {
        let folded = self.closing.take().is_some() || self.resting;
        if self.resting {
            self.resting = false;
            window.set_input_region(Some(&[region(self.width)]));
        }
        self.levels = levels;
        self.phase = Phase::Listening;
        self.since = Instant::now();
        if folded {
            self.opened = Instant::now();
            self.widened = false;
        }
        cx.notify();
    }

    /// Folds the pill away. Once folded it rests, invisible, until [`Pill::reopen`].
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.closing.is_none() {
            self.closing = Some(Instant::now());
            self.body.aim(0., CLOSE);
            self.controls.aim(0., CLOSE);
            cx.notify();
        }
    }

    /// Advances the opening or closing motion. Returns false once a close has finished.
    fn animate(&mut self, elapsed: Duration, still: bool) -> bool {
        match self.closing {
            None => {
                self.body.aim(1., OPEN);
                if self.opened.elapsed() >= CONTROLS_DELAY {
                    self.controls.aim(1., POP);
                }
            }
            Some(closing) if closing.elapsed() < BARS_FADE => {
                // The bars go out first, then the buttons and the capsule fold.
                self.controls.target = self.controls.at();
                self.body.target = self.body.at();
            }
            Some(_) => {
                self.controls.aim(0., CLOSE);
                self.body.aim(0., CLOSE);
            }
        }
        if still {
            self.body.finish();
            self.controls.finish();
        } else {
            self.body.step(elapsed);
            self.controls.step(elapsed);
        }
        let Some(closing) = self.closing else {
            return true;
        };
        let folded = closing.elapsed() >= BARS_FADE && self.body.at() < 0.02 && self.body.settled();
        !(folded || still || closing.elapsed() >= CLOSE_LIMIT)
    }

    /// How much of each bar shows: they light up from the middle outwards after opening and all
    /// go out together on close.
    fn reveal(&self) -> [f32; BARS] {
        let opened = self.opened.elapsed().as_secs_f32();
        let fade = self.closing.map_or(1., |closing| {
            1. - (closing.elapsed().as_secs_f32() / BARS_FADE.as_secs_f32()).min(1.)
        });
        let middle = (BARS - 1) as f32 / 2.;
        std::array::from_fn(|index| {
            let delay = BARS_DELAY.as_secs_f32()
                + BARS_STAGGER.as_secs_f32() * (index as f32 - middle).abs().floor();
            let t = ((opened - delay) / BAR_REVEAL.as_secs_f32()).clamp(0., 1.);
            ui::ease_out_cubic(t) * fade
        })
    }

    /// Eases the drawn bars towards the latest levels at a rate that follows the real time
    /// between frames, so the motion looks the same at any refresh rate.
    fn step(&mut self) -> Duration {
        let now = Instant::now();
        let elapsed = self.beat.replace(now).map_or(Duration::ZERO, |beat| {
            (now - beat).min(Duration::from_millis(50))
        });
        let rate = 1. - (-elapsed.as_secs_f32() / SETTLE.as_secs_f32()).exp();
        let target = match self.phase {
            Phase::Listening => self.levels.read(),
            Phase::Processing | Phase::Error => [0.; BARS],
        };
        for (shown, target) in self.shown.iter_mut().zip(target) {
            *shown += (target - *shown) * rate;
        }
        elapsed
    }

    /// How opaque the bars are, which pulses while the last segments are transcribed.
    fn intensity(&self) -> f32 {
        match self.phase {
            Phase::Listening => 1.,
            Phase::Processing => {
                let t = self.since.elapsed().as_secs_f32() / PULSE.as_secs_f32();
                0.35 + 0.3 * (0.5 - 0.5 * (t * std::f32::consts::TAU).cos())
            }
            Phase::Error => 0.5,
        }
    }
}

impl Render for Pill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.resting {
            return div().size_full();
        }
        let elapsed = self.step();
        if !self.animate(elapsed, cx.reduce_motion()) {
            // Folded: clear the surface, stop taking clicks and stop asking for frames. A floating
            // window cannot let clicks through, so off Linux it closes and the next session opens
            // a new one.
            self.resting = true;
            self.beat = None;
            window.set_input_region(Some(&[]));
            #[cfg(not(target_os = "linux"))]
            window.remove_window();
            return div().size_full();
        }
        window.request_animation_frame();

        let theme = cx.theme();
        let error = self.phase == Phase::Error;
        // Keep the capsule's neutral surface in every phase; errors tint only the indicator.
        let bar_color = match error {
            true => theme.danger,
            false => theme.foreground,
        }
        .opacity(self.intensity());
        let shown = self.shown;
        let reveal = self.reveal();
        let on_confirm = self.on_confirm.clone();
        let on_cancel = self.on_cancel.clone();
        let listening = self.phase == Phase::Listening;

        // The capsule unfolds sideways from a dot, growing and sharpening as it goes. The spring
        // overshoots a little, so it lands slightly wide and settles back.
        let body = self.body.at().max(0.);
        let unfolded = body.min(1.);
        let opening = self.closing.is_none();
        if opening && body >= 1. {
            self.widened = true;
        }
        let (spread, scale) = match opening && self.widened {
            true => (1., 1. + (body - 1.) * OVERSHOOT_SCALE),
            false => (unfolded, FOLDED_SCALE + (1. - FOLDED_SCALE) * body),
        };
        let width = ui::snapped(HEIGHT + (self.width - HEIGHT) * spread, window);
        let controls = self.controls.at().max(0.);
        let pop = (0.4 + 0.6 * controls).max(0.01);
        let control_opacity = controls.min(1.);
        // At rest the capsule is drawn plainly. Any layer effect left on, even at a scale of
        // 0.9999, resamples it and shifts its edges by a fraction of a pixel.
        let moving = body != 1. || controls != 1.;

        // The capsule is placed on whole device pixels. Centring it with flex would put it on a
        // half pixel whenever the room around it is an odd number of them.
        let left = ui::snapped((window.viewport_size().width - width) / 2., window);
        let top = ui::snapped(SHADOW, window);

        div().size_full().relative().font_family("Inter").child(
            capsule(width, 1., cx)
                .absolute()
                .left(left)
                .top(top)
                .when(moving, |this| {
                    this.opacity((unfolded * 2.5).min(1.))
                        .layer_scale(scale.max(0.01))
                })
                .when(unfolded < 1., |this| {
                    this.blur(FOLDED_BLUR * (1. - unfolded))
                })
                .when(self.cancel, |this| {
                    this.child(
                        cancel_button("cancel", 1., cx)
                            .when(moving, |this| {
                                this.opacity(control_opacity).layer_scale(pop)
                            })
                            .on_click(move |_, _, cx| on_cancel(cx)),
                    )
                })
                .child(
                    bars(shown, reveal, bar_color, 1., self.layout)
                        .flex_1()
                        .h_full(),
                )
                .when(self.confirm, |this| {
                    this.child(
                        confirm_button("confirm", 1., cx)
                            .opacity(match listening {
                                true => control_opacity,
                                false => control_opacity * 0.5,
                            })
                            .when(moving, |this| this.layer_scale(pop))
                            .on_click(move |_, _, cx| on_confirm(cx)),
                    )
                }),
        )
    }
}

fn circle(
    id: impl Into<gpui::ElementId>,
    size: Pixels,
    fill: Hsla,
    hover: Hsla,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .size(size)
        .rounded(size / 2.)
        .flex()
        .items_center()
        .justify_center()
        .bg(fill)
        .hover(move |style| style.bg(hover))
        .cursor_pointer()
}

fn icon(path: &'static str, size: Pixels, color: Hsla) -> impl IntoElement {
    svg().path(icons::path(path)).size(size).text_color(color)
}

/// Paints the bars as rounded rectangles centered on the pill's midline, laid out as `layout`
/// says. `reveal` says how much of each bar shows while the pill opens and closes, from the
/// middle out. A silent band draws a dot as tall as the bar is wide, so silence reads as a flat
/// dotted line. Given more room than the bands need, more bars fill it, the bands spread across
/// all of them. They come in pairs, one at each end, so the bars already there keep their
/// places, and a pair still coming in fades and grows in as the room for it opens. A pill at
/// rest is a whole number of pairs wide, see [`snap_stretch`]. `size` draws them smaller, for a
/// pill drawn small. Every edge lands on a whole device pixel, so at fractional scales all bars
/// come out the same width and the dots sit on one line.
pub(crate) fn bars(
    levels: [f32; BARS],
    reveal: [f32; BARS],
    color: Hsla,
    size: f32,
    layout: Bars,
) -> gpui::Div {
    // The peak reads the bands loudest first, folded out from the middle.
    let levels = match layout {
        Bars::Peak => {
            let mut ranked = levels;
            ranked.sort_by(|a, b| b.total_cmp(a));
            ranked
        }
        Bars::Spectrum | Bars::Mirrored => levels,
    };
    div().child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let scale = window.scale_factor();
                let device = |logical: f32| (logical * scale).round();
                let width = device(BAR_WIDTH_PX * size).max(1.);
                let gap = device(BAR_GAP_PX * size).max(1.);
                // How many bars the room holds, fractions included, counted in logical pixels
                // so a pill a whole number of pairs wide holds exactly that many at any scale.
                let room = bounds.size.width.as_f32() - BAR_MARGIN_PX * size * 2.;
                let step = (BAR_WIDTH_PX + BAR_GAP_PX) * size;
                let fits = ((room + BAR_GAP_PX * size) / step).max(BARS as f32);
                let pairs = ((fits - BARS as f32) / 2. * 1000.).round() / 1000.;
                let count = BARS + pairs.ceil() as usize * 2;
                let incoming = match pairs.fract() {
                    0. => 1.,
                    part => part,
                };
                let span = width * count as f32 + gap * (count - 1) as f32;
                let left = device(bounds.origin.x.as_f32())
                    + ((device(bounds.size.width.as_f32()) - span) / 2.).round();
                let middle = device(bounds.origin.y.as_f32() + bounds.size.height.as_f32() / 2.);
                let reach = bounds.size.height.as_f32() * BAR_REACH * scale;
                // The bands spread over the room as it is rather than over whole bars, so at
                // rest the outermost bars read the outermost bands, and while a pair comes in
                // the bands slide outwards smoothly to make way for it.
                let half = (fits - 1.) / 2.;
                let middle_band = (BARS - 1) as f32 / 2.;
                let last = (BARS - 1) as f32;
                for index in 0..count {
                    let from_middle = index as f32 - (count - 1) as f32 / 2.;
                    // Where the bar sits, as a band. The bars open from the middle by it, and
                    // read their level from it, folded in half when mirrored or peaked.
                    let place = (middle_band + from_middle / half * middle_band).clamp(0., last);
                    let band = match layout {
                        Bars::Spectrum => place,
                        Bars::Mirrored | Bars::Peak => {
                            ((place - middle_band).abs() / middle_band * last).min(last)
                        }
                    };
                    let outermost = index == 0 || index == count - 1;
                    let arriving = match outermost && count > BARS {
                        true => incoming,
                        false => 1.,
                    };
                    let level = between(&levels, band);
                    let shown = between(&reveal, place) * arriving;
                    if shown <= 0. {
                        continue;
                    }
                    // Rounding to the bar's own parity keeps every bar centered on the same line.
                    let mut height = (reach * level.clamp(0., 1.) * shown).round().max(width);
                    if (height - width) % 2. != 0. {
                        height += 1.;
                    }
                    let x = left + (width + gap) * index as f32;
                    let top = middle - ((height - width) / 2.) - (width / 2.).floor();
                    let bar = Bounds {
                        origin: point(px(x / scale), px(top / scale)),
                        size: Size::new(px(width / scale), px(height / scale)),
                    };
                    let color = color.opacity(shown);
                    window.paint_quad(gpui::fill(bar, color).corner_radii(px(width / scale / 2.)));
                }
            },
        )
        .size_full(),
    )
}

/// The value at `at` along `values`, read between the two either side of it.
fn between(values: &[f32; BARS], at: f32) -> f32 {
    let low = (at.floor() as usize).min(BARS - 1);
    let high = (low + 1).min(BARS - 1);
    let along = at - low as f32;
    values[low] + (values[high] - values[low]) * along
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_bars_follow_the_bands_beside_them() {
        let levels = std::array::from_fn(|band| band as f32);
        assert_eq!(between(&levels, 0.), 0.);
        assert_eq!(between(&levels, 2.5), 2.5);
        assert_eq!(between(&levels, (BARS - 1) as f32), (BARS - 1) as f32);
    }

    #[test]
    fn stretch_snaps_to_whole_pairs() {
        assert_eq!(snap_stretch(0.), 0.);
        assert_eq!(snap_stretch(STRETCH_STEP * 0.4), 0.);
        assert_eq!(snap_stretch(STRETCH_STEP * 2.6), STRETCH_STEP * 3.);
        assert_eq!(snap_stretch(10_000.), MAX_STRETCH);
    }

    #[test]
    fn buttons_follow_the_radius_rule() {
        assert_eq!(button_size() + PADDING * 2., HEIGHT);
    }
}
