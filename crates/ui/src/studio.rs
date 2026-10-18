//! The appearance page's visual editor: the pill and its caption drawn live as a session shows
//! them, with the parts that can be switched off clicked off and on in place. The distance from
//! the edge is set by holding the arrow beside the pill, which drops the pill onto a drawing of
//! the main monitor to scale, and moving the mouse as if over that monitor. The pointer is held
//! still meanwhile, so the drag never runs into the edge of the window or the screen.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, CursorStyle, Empty, Entity, MouseButton, MouseDownEvent, Pixels, Point,
    SpringConfig, Task, Window, canvas, div, point, px, svg,
};
use tokativ_audio::BANDS;
use tokativ_config::{Pill as PillConfig, PillEdge};
use ui::{ActiveTheme as _, Perch, Text, Tipped as _};

use crate::caption;
use crate::chrome;
use crate::grab::Grab;
use crate::host::{Host, Store};
use crate::pill;
use crate::screen::{self, Screen};
use crate::spring::Spring;
use crate::text_morph::{FONT_SIZE, LINE_HEIGHT};

const STAGE_HEIGHT: f32 = 280.;
/// The room between the pill and the stage's edge, which the arrow sits in.
const ARROW_ROOM: f32 = 64.;
const ARROW_WIDTH: f32 = 120.;
/// How far the arrow's line stops short of the pill and of the stage's edge.
const ARROW_INSET: f32 = 6.;
/// How far the drawn monitor keeps from the stage's sides.
const SCREEN_INSET: f32 = 20.;
/// The tallest the drawn monitor grows. A landscape monitor fills the stage's width, and one
/// turned upright stops at this height.
const SCREEN_HEIGHT: f32 = 440.;
const SCREEN_RADIUS: f32 = 8.;
/// The share of the drop over which the stage's own parts fade out, and the point after which
/// the monitor's markings fade in.
const FADE_OUT: f32 = 0.3;
const FADE_IN: f32 = 0.7;
/// The height of the badge that reads the distance on the monitor.
const BADGE_HEIGHT: f32 = 18.;
/// The most the compositor scales the group while it drops onto the monitor or lifts off it;
/// past this it is laid out at another scale instead.
const MAX_SHRINK: f32 = 0.8;
/// How far each quarter of a stretched box reaches past its middle, so the quarters overlap.
const SEAM: f32 = 2.;
/// How wide the strip along each end of the pill is that drags it wider, half of it outside.
const GRIP_WIDTH: f32 = 12.;
/// The space between the pill and the outline that brings back a hidden button.
const GHOST_GAP: f32 = 10.;
/// Below this scale the sample caption's text is drawn as lines, which stay sharp where letters
/// would blur into specks, and above the other it reads as text. Between them one fades into the
/// other.
const SKETCH_BELOW: f32 = 0.45;
const LEGIBLE_ABOVE: f32 = 0.7;
/// What the sample caption reads, the settled part and the draft still being spoken.
const WRITTEN: &str = "The meeting moved to Thursday at three.";
const DRAFT: &str = "Let's sync after the demo";
/// The pill dropping onto the monitor, with a little bounce as it lands, and lifting back.
const DROP: SpringConfig = SpringConfig::new(300., 24., 1.);
const LIFT: SpringConfig = SpringConfig::new(360., 34., 1.);
/// The pill's width gliding to the nearest whole pair of bars.
const SNAP: SpringConfig = SpringConfig::new(520., 40., 1.);
/// A button sliding between its outline beside the pill and its place in it, with a little
/// give as it lands.
const SLIDE: SpringConfig = SpringConfig::new(380., 28., 1.);

/// What GPUI carries while the arrow is dragged without a pointer lock. It draws nothing.
#[derive(Clone)]
struct Grip;

impl Render for Grip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The arrow being held.
/// An edge of the pill being dragged to make it wider or narrower.
struct Resize {
    side: Side,
    /// Where the pointer went down, and the stretch the pill had then.
    from: Pixels,
    start: f32,
    stretch: f32,
}

struct Drag {
    grab: Grab,
    /// The compositor has locked the pointer, so the mouse's movement comes through the grab.
    locked: bool,
    /// The pill's top on the monitor, in logical pixels.
    top: f32,
    /// The pointer where it was last seen, for following it when it is not locked.
    pointer: Point<Pixels>,
}

pub struct Studio {
    store: Entity<Store>,
    screen: Option<Screen>,
    _screen: Task<()>,
    started: Instant,
    beat: Option<Instant>,
    /// Where each button is, from 0 as the outline beside the pill to 1 in its place in it.
    cancel: Spring,
    confirm: Spring,
    /// How far the caption has unfolded, from 0 hidden to 1 shown.
    caption: Spring,
    /// How much wider than it needs the pill is drawn, gliding to whole pairs of bars.
    stretch: Spring,
    /// How far the pill has dropped onto the monitor, from 0 at rest to 1 on it.
    place: Spring,
    /// The stage's width as last laid out.
    stage: Rc<Cell<f32>>,
    drag: Option<Drag>,
    resize: Option<Resize>,
}

impl Studio {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let store = Host::global(cx).store;
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let config = store.read(cx).config().pill.clone();
        let reading = screen::primary(cx);
        Self {
            store,
            screen: None,
            _screen: cx.spawn(async move |this, cx| {
                let screen = reading.await;
                this.update(cx, |this, cx| {
                    this.screen = Some(screen);
                    cx.notify();
                })
                .ok();
            }),
            started: Instant::now(),
            beat: None,
            caption: landed(caption::OPEN, config.caption),
            stretch: {
                let mut spring = Spring::new(SNAP);
                spring.aim(pill::snap_stretch(config.stretch), SNAP);
                spring.finish();
                spring
            },
            cancel: landed(SLIDE, config.cancel_button),
            confirm: landed(SLIDE, config.confirm_button),
            place: Spring::new(DROP),
            stage: Rc::new(Cell::new(0.)),
            drag: None,
            resize: None,
        }
    }

    fn edit(&self, cx: &mut Context<Self>, edit: impl FnOnce(&mut PillConfig)) {
        self.store.update(cx, |store, cx| {
            store.update(|config| edit(&mut config.pill), cx)
        });
    }

    fn screen(&self) -> Screen {
        self.screen.unwrap_or(Screen::FALLBACK)
    }

    /// Holds the pointer and drops the pill onto the monitor.
    fn begin(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config = &self.store.read(cx).config().pill;
        let height = self.screen().size.height.as_f32();
        let top = top_of(config.edge, config.margin, height);
        self.drag = Some(Drag {
            grab: Grab::start(window),
            locked: false,
            top,
            pointer: event.position,
        });
        cx.hide_cursor();
        self.place.aim(1., DROP);
        cx.notify();
    }

    /// Follows the pointer itself, while the compositor has not locked it.
    fn follow(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let height = self.screen().size.height.as_f32();
        let Some(drag) = self.drag.as_mut().filter(|drag| !drag.locked) else {
            return;
        };
        drag.top = clamp_top(drag.top + (position.y - drag.pointer.y).as_f32(), height);
        drag.pointer = position;
        cx.notify();
    }

    /// Lets go of the pointer, keeps where the pill was put and lifts it back off the monitor.
    fn end(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let (edge, margin) = placement(drag.top, self.screen().size.height.as_f32());
        self.edit(cx, |pill| {
            pill.edge = edge;
            pill.margin = margin;
        });
        self.place.aim(0., LIFT);
        cx.notify();
    }

    fn begin_resize(&mut self, side: Side, x: Pixels, cx: &mut Context<Self>) {
        let start = self.store.read(cx).config().pill.stretch;
        self.resize = Some(Resize {
            side,
            from: x,
            start,
            stretch: start,
        });
        cx.notify();
    }

    /// Moves the dragged edge with the pointer. The pill stays centred, so it grows by twice
    /// what the edge moves.
    fn resize_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(resize) = &mut self.resize else {
            return;
        };
        let moved = (x - resize.from).as_f32()
            * match resize.side {
                Side::Cancel => -2.,
                Side::Confirm => 2.,
            };
        resize.stretch = (resize.start + moved).clamp(0., pill::MAX_STRETCH);
        cx.notify();
    }

    fn end_resize(&mut self, cx: &mut Context<Self>) {
        let Some(resize) = self.resize.take() else {
            return;
        };
        self.edit(cx, |pill| pill.stretch = pill::snap_stretch(resize.stretch));
        cx.notify();
    }

    /// Takes the mouse's movement from the pointer lock, when it holds the pointer.
    fn track(&mut self) {
        let height = self.screen().size.height.as_f32();
        let Some(drag) = &mut self.drag else {
            return;
        };
        if let Some(moved) = drag.grab.moved() {
            drag.locked = true;
            drag.top = clamp_top(drag.top + moved.y, height);
        }
    }

    fn step(&mut self, still: bool) {
        let now = Instant::now();
        let elapsed = self.beat.replace(now).map_or(Duration::ZERO, |beat| {
            (now - beat).min(Duration::from_millis(50))
        });
        let springs = [
            &mut self.cancel,
            &mut self.confirm,
            &mut self.caption,
            &mut self.stretch,
            &mut self.place,
        ];
        for spring in springs {
            match still {
                true => spring.finish(),
                false => spring.step(elapsed),
            }
        }
    }
}

impl Render for Studio {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        self.track();
        let mut config = self.store.read(cx).config().pill.clone();
        self.cancel.aim(config.cancel_button as u8 as f32, SLIDE);
        self.confirm.aim(config.confirm_button as u8 as f32, SLIDE);
        match config.caption {
            true => self.caption.aim(1., caption::OPEN),
            false => self.caption.aim(0., caption::CLOSE),
        }
        self.step(cx.reduce_motion());

        let screen = self.screen();
        let screen_height = screen.size.height.as_f32();
        if let Some(drag) = &self.drag {
            (config.edge, config.margin) = placement(drag.top, screen_height);
        }
        // The width glides to whole pairs of bars, even while an end is dragged.
        let stretch = match &self.resize {
            Some(resize) => resize.stretch,
            None => config.stretch,
        };
        self.stretch.aim(pill::snap_stretch(stretch), SNAP);
        config.stretch = self.stretch.at().max(0.);
        let theme = *cx.theme();
        let stage_width = self.stage.get();
        let placing = self.place.at();
        let resting = (1. - placing).clamp(0., 1.);
        // What belongs to the stage at rest is gone within the first part of the drop, and the
        // markings on the monitor only come in over its last part, so the two never mix.
        let early = (1. - placing / FADE_OUT).clamp(0., 1.);
        let late = ((placing - FADE_IN) / (1. - FADE_IN)).clamp(0., 1.);
        let held = self.drag.is_some();

        // The pill and the caption go together as one group, the caption on the side away from
        // the edge.
        let pill_height = pill::HEIGHT.as_f32();
        let gap = caption::GAP.as_f32();
        let caption_width = caption::WIDTH.as_f32().min(stage_width - 32.).max(0.);
        let caption_height =
            (caption::TEXT_HEIGHT + caption::PADDING_Y * 2. + caption::BORDER * 2.).as_f32();
        let group_width = caption_width.max(pill::width(true, true, config.stretch).as_f32());
        let group_height = pill_height + gap + caption_height;
        let bottom = config.edge == PillEdge::Bottom;

        // At rest the pill sits towards its edge of the stage, with the arrow between them.
        let rest_pill_top = match bottom {
            true => STAGE_HEIGHT - ARROW_ROOM - pill_height,
            false => ARROW_ROOM,
        };
        // From the pill's middle to the group's.
        let to_group = (group_height - pill_height) / 2. * if bottom { -1. } else { 1. };
        let rest = point(
            stage_width / 2.,
            rest_pill_top + pill_height / 2. + to_group,
        );

        // The monitor fills the stage's width, and the stage grows to fit it. On the monitor
        // everything shrinks to its scale.
        let aspect = screen.size.width.as_f32() / screen_height;
        // Its rectangle sits on whole pixels, so the frame lands there with nothing to snap.
        let grid = |value: f32| ui::snapped(px(value), window).as_f32();
        let monitor_height =
            grid(((stage_width - SCREEN_INSET * 2.) / aspect).clamp(1., SCREEN_HEIGHT));
        let monitor_width = grid(monitor_height * aspect);
        let placed_height = STAGE_HEIGHT.max(monitor_height + SCREEN_INSET * 2.);
        let stage_height = lerp(STAGE_HEIGHT, placed_height, placing);
        let monitor = point(
            grid((stage_width - monitor_width) / 2.),
            grid((placed_height - monitor_height) / 2.),
        );
        let scale = monitor_height / screen_height;
        let pill_top = top_of(config.edge, config.margin, screen_height);
        let placed = point(
            monitor.x + monitor_width / 2.,
            monitor.y + (pill_top + pill_height / 2. + to_group) * scale,
        );

        // The group's corner rests on a whole pixel at both ends of the drop, so nothing is
        // left over to snap away when it lands.
        // Its box takes in the room it keeps for the shadows.
        let shadow = pill::SHADOW.as_f32();
        let on_grid = |center: f32, size: f32| {
            let corner = center - size / 2.;
            ui::snapped(px(corner), window).as_f32() - corner
        };
        let nudge = point(
            lerp(
                on_grid(rest.x, group_width + shadow * 2.),
                on_grid(placed.x, (group_width + shadow * 2.) * scale),
                placing,
            ),
            lerp(
                on_grid(rest.y, group_height + shadow * 2.),
                on_grid(placed.y, (group_height + shadow * 2.) * scale),
                placing,
            ),
        );
        let center = point(
            lerp(rest.x, placed.x, placing) + nudge.x,
            lerp(rest.y, placed.y, placing) + nudge.y,
        );
        let group_scale = lerp(1., scale, placing).max(0.01);
        // Laid out at a step near its drawn scale, and shrunk the rest of the way as one layer.
        // Compositor layers resample what they hold, so they are on only while the drop moves,
        // and anything resting, on the stage or on the monitor, is drawn plainly.
        let dropping = !self.place.settled();
        let level = match dropping {
            true => layout_scale(group_scale, scale, self.place.target == 1.),
            false => group_scale,
        };
        let presented = group_scale / level;
        let laid = (group_width * level, group_height * level);

        let frame_left = lerp(0., monitor.x, placing);
        let frame_top = lerp(0., monitor.y, placing);
        let frame_width = lerp(grid(stage_width), monitor_width, placing);
        let frame_height = lerp(STAGE_HEIGHT, monitor_height, placing);
        let shown = placing.clamp(0., 1.);
        let frame_radius = px(lerp(chrome::GROUP_RADIUS.as_f32(), SCREEN_RADIUS, shown));
        let moving_frame = dropping;
        let (laid_left, drift_left) = adrift(frame_left, moving_frame, window);
        let (laid_top, drift_top) = adrift(frame_top, moving_frame, window);
        let frame = div()
            .absolute()
            .left(laid_left)
            .top(laid_top)
            .when(moving_frame, |this| {
                this.layer_translate(point(drift_left, drift_top))
            })
            .child(stretched(
                frame_width,
                frame_height,
                moving_frame,
                window,
                || {
                    div()
                        .rounded(frame_radius)
                        .border_1()
                        .border_color(theme.border.opacity(shown))
                        .bg(theme.background.opacity(shown))
                },
            ))
            .child(
                div()
                    .absolute()
                    .top_2()
                    .left_3()
                    .opacity(late)
                    .text_size(theme.text(Text::Tiny))
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} × {}",
                        screen.resolution.width, screen.resolution.height
                    )),
            );

        // The distance drawn on the monitor, from its edge to the pill, both where the drop
        // has got them to so far, so the line never runs ahead of either.
        let pill_middle = center.y - to_group * group_scale;
        let pill_half = pill_height * group_scale / 2.;
        let (gap_top, gap_bottom) = match bottom {
            true => (pill_middle + pill_half, frame_top + frame_height),
            false => (frame_top, pill_middle - pill_half),
        };
        let gap_height = (gap_bottom - gap_top).max(0.);
        let line_x = center.x;
        let shown = late;
        let measure = div()
            .absolute()
            .left(px(line_x))
            .top(px(gap_top))
            .h(px(gap_height))
            .w(px(1.))
            .bg(theme.primary)
            .opacity(shown);
        // The badge sits by the line, or beside the pill once the line is too short for it,
        // always inside the monitor. It lands on whole pixels so its text never shifts in it.
        let beside = gap_height < BADGE_HEIGHT + 8.;
        let (badge_x, badge_y) = match beside {
            true => (
                line_x + self.capsule_width(config.stretch) * group_scale / 2. + 8.,
                pill_middle,
            ),
            false => (line_x + 8., gap_top + gap_height / 2.),
        };
        let badge_y = (badge_y - BADGE_HEIGHT / 2.).clamp(
            frame_top + 4.,
            (frame_top + frame_height - BADGE_HEIGHT - 4.).max(frame_top + 4.),
        );
        let badge = div()
            .absolute()
            .left(ui::snapped(px(badge_x), window))
            .top(ui::snapped(px(badge_y), window))
            .h(px(BADGE_HEIGHT))
            .flex()
            .items_center()
            .px(px(6.))
            .rounded_full()
            .bg(theme.primary)
            .opacity(shown)
            .text_color(theme.background)
            .text_size(theme.text(Text::Tiny))
            .line_height(px(BADGE_HEIGHT))
            .whitespace_nowrap()
            .child(format!("{} px", config.margin.round()));

        // The capsule only needs drawing to the fraction while its own width glides. In the drop
        // the whole group is scaled as one layer, laid out as it will land.
        let moving = !self.cancel.settled() || !self.confirm.settled() || !self.stretch.settled();
        let pill_row = self.pill_row(&config, level, early, moving, window, cx);
        let caption = self.caption(&config, caption_width, caption_height, level, early, cx);
        // The group's layer only carries what is painted inside it, so it reaches past the pill
        // and the caption far enough to hold their shadows.
        let margin = (pill::SHADOW * level).as_f32();
        let (group_left, drift_x) = adrift(center.x - laid.0 / 2. - margin, dropping, window);
        let (group_top, drift_y) = adrift(center.y - laid.1 / 2. - margin, dropping, window);
        let group = div()
            .absolute()
            .p(px(margin))
            .left(group_left)
            .top(group_top)
            .when(drift_x != px(0.) || drift_y != px(0.), |this| {
                this.layer_translate(point(drift_x, drift_y))
            })
            .when(presented != 1., |this| this.layer_scale(presented))
            .w(px(laid.0 + margin * 2.))
            .h(px(laid.1 + margin * 2.))
            .flex()
            .items_center()
            .gap(px(gap * level))
            .map(|this| match bottom {
                true => this.flex_col_reverse(),
                false => this.flex_col(),
            })
            .child(pill_row)
            .child(caption);

        let arrow = self.arrow(&config, rest_pill_top, early, held, cx);

        let stage_size = self.stage.clone();
        let stage = div()
            .relative()
            .w_full()
            .h(px(stage_height))
            .rounded(chrome::GROUP_RADIUS)
            .squircle()
            .border_1()
            .border_color(theme.border.opacity(0.6 * resting))
            .bg(theme.secondary.opacity(0.25))
            .font_family("Inter")
            .child(
                canvas(
                    move |bounds, _, _| stage_size.set(bounds.size.width.as_f32()),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(frame)
            .child(measure)
            .child(badge)
            .child(group)
            .child(arrow);

        let hint = match held {
            false => {
                "Click the buttons or the caption to hide or show them. Hold the arrow and move \
                 the mouse to set the distance from the edge."
            }
            true => {
                "Move the mouse up or down as if over your screen. Past the middle the pill goes \
                 to the other edge."
            }
        };
        div().flex().flex_col().gap_2().py_3().child(stage).child(
            div()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(hint),
        )
    }
}

impl Studio {
    /// The capsule's width as its buttons slide in and out, `stretch` wider than it needs.
    fn capsule_width(&self, stretch: f32) -> f32 {
        let buttons = self.cancel.at().max(0.) + self.confirm.at().max(0.);
        (pill::width(false, false, stretch) + pill::button_size() * buttons).as_f32()
    }

    /// The capsule with its live bars, the buttons that hide on a click, and outlines beside it
    /// where hidden buttons come back, all drawn at `scale`.
    fn pill_row(
        &self,
        config: &PillConfig,
        scale: f32,
        resting: f32,
        moving: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let levels = speech(self.started.elapsed().as_secs_f32());
        let height = pill::HEIGHT * scale;
        // The row is laid out on whole pixels and the capsule drawn to the fraction over it, see
        // [`stretched`]. Its shadow, soft enough that its steps never show, is cast by the row.
        let exact = self.capsule_width(config.stretch) * scale;
        let laid = ui::snapped(px(exact), window).as_f32();
        let face = stretched(exact, height.as_f32(), moving, window, || {
            pill::face(px(0.), height, cx)
        });
        let layout = config.bars;
        let edges = (laid / 2. - exact / 2., laid / 2. + exact / 2.);
        let bars_width = (pill::bars_span() + px(config.stretch)) * scale;
        // The bars follow the capsule's padding and the room the cancel button takes, as they
        // do in the session's pill.
        let padding = (pill::HEIGHT - pill::button_size()) / 2. * scale;
        let bars_left =
            px(edges.0) + padding + pill::button_size() * scale * self.cancel.at().max(0.);
        let bars = pill::bars(levels, [1.; BANDS], theme.foreground, scale, layout)
            .id("studio-bars")
            .absolute()
            .top_0()
            .left(bars_left)
            .w(bars_width)
            .h(height)
            .when(resting == 1., |this| {
                this.cursor_pointer()
                    .tip_label(
                        format!(
                            "Bars: {}. Click for {}.",
                            layout.name(),
                            layout.next().name().to_lowercase()
                        ),
                        Perch::Above,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |pill| pill.bars = layout.next())
                    }))
            });
        let cancel = self.button(
            Side::Cancel,
            config.cancel_button,
            edges,
            scale,
            resting,
            moving,
            window,
            cx,
        );
        let confirm = self.button(
            Side::Confirm,
            config.confirm_button,
            edges,
            scale,
            resting,
            moving,
            window,
            cx,
        );
        div()
            .relative()
            .flex_none()
            .w(px(laid))
            .h(height)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(height / 2.)
                    .shadow(pill::shadow(scale)),
            )
            .child(div().absolute().top_0().left_0().child(face))
            .child(bars)
            .child(cancel)
            .child(confirm)
            .when(resting == 1., |this| {
                this.child(self.grip(Side::Cancel, cx))
                    .child(self.grip(Side::Confirm, cx))
            })
            .into_any_element()
    }

    /// The strip along one end of the pill that drags that end out or in. Its handle shows on
    /// hover and while it is dragged.
    fn grip(&self, side: Side, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let (id, group) = match side {
            Side::Cancel => ("studio-grip-left", "studio-grip-left"),
            Side::Confirm => ("studio-grip-right", "studio-grip-right"),
        };
        let held = self
            .resize
            .as_ref()
            .is_some_and(|resize| resize.side == side);
        let reach = px(GRIP_WIDTH / 2.);
        div()
            .id(id)
            .group(group)
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(GRIP_WIDTH))
            .map(|this| match side {
                Side::Cancel => this.left(-reach),
                Side::Confirm => this.right(-reach),
            })
            .flex()
            .items_center()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .when(!held, |this| {
                this.tip_label("Drag to make the pill wider or narrower", Perch::Above)
            })
            .child(
                div()
                    .w(px(3.))
                    .h(px(18.))
                    .rounded_full()
                    .bg(theme.muted_foreground)
                    .opacity(if held { 1. } else { 0. })
                    .group_hover(group, |style| style.opacity(1.)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_resize(side, event.position.x, cx)
                }),
            )
            .on_drag(Grip, |grip, _, _, cx| cx.new(|_| grip.clone()))
            .on_drag_move(
                cx.listener(|this, event: &gpui::DragMoveEvent<Grip>, _, cx| {
                    this.resize_to(event.event.position.x, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_resize(cx)),
            )
            .into_any_element()
    }

    /// One button, as far along as it has slid: its dashed outline beside the pill at 0 and
    /// the button itself in its place at 1, turning from one into the other on the way.
    #[allow(clippy::too_many_arguments)]
    fn button(
        &self,
        side: Side,
        on: bool,
        (left_edge, right_edge): (f32, f32),
        scale: f32,
        resting: f32,
        moving: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let shown = match side {
            Side::Cancel => self.cancel.at(),
            Side::Confirm => self.confirm.at(),
        };
        let solid = shown.clamp(0., 1.);
        let size = pill::button_size() * scale;
        let inside = (pill::HEIGHT - pill::button_size()) / 2. * scale;
        let outside = -(size + px(GHOST_GAP) * scale);
        // How far in from its end of the capsule the button sits, and so where it is in the row,
        // to the fraction, laid out on a whole pixel with the rest added by the compositor.
        let offset = (outside + (inside - outside) * shown).as_f32();
        let left = match side {
            Side::Cancel => left_edge + offset,
            Side::Confirm => right_edge - offset - size.as_f32(),
        };
        let (left, rest) = adrift(left, moving, window);
        let (id, filled, name) = match side {
            Side::Cancel => (
                "studio-cancel",
                pill::cancel_button("studio-cancel-fill", scale, cx),
                "cancel",
            ),
            Side::Confirm => (
                "studio-confirm",
                pill::confirm_button("studio-confirm-fill", scale, cx),
                "confirm",
            ),
        };
        let tip = match on {
            true => format!("Hide the {name} button"),
            false => format!("Show the {name} button"),
        };
        let outline = div()
            .absolute()
            .inset_0()
            .rounded(size / 2.)
            .border_1()
            .border_dashed()
            .border_color(theme.muted_foreground.opacity(0.6))
            .flex()
            .items_center()
            .justify_center()
            .opacity(0.7 * resting * (1. - solid))
            .child(
                svg()
                    .path(icons::path("icons/plus.svg"))
                    .size(px(14.) * scale)
                    .text_color(theme.muted_foreground),
            );
        div()
            .id(id)
            .absolute()
            .top(inside)
            .size(size)
            .left(left)
            .when(rest != px(0.), |this| {
                this.layer_translate(point(rest, px(0.)))
            })
            // Off the stage the pointer sits still where the arrow was grabbed, and whatever
            // moves under it must not light up.
            .when(resting == 1., |this| {
                this.cursor_pointer().tip_label(tip, Perch::Above)
            })
            .when(solid < 1., |this| this.child(outline))
            .when(solid > 0., |this| {
                this.child(div().absolute().inset_0().opacity(solid).child(filled))
            })
            .when(!on && resting == 1., |this| {
                this.hover(|style| style.bg(theme.secondary.opacity(0.6)).rounded(size / 2.))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.edit(cx, |pill| match side {
                    Side::Cancel => pill.cancel_button = !on,
                    Side::Confirm => pill.confirm_button = !on,
                })
            }))
            .into_any_element()
    }

    /// The caption with a sample in it, or the outline that brings it back, drawn at `scale`.
    /// Too small to read, the sample turns into lines standing in for its text.
    fn caption(
        &self,
        config: &PillConfig,
        width: f32,
        height: f32,
        scale: f32,
        resting: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let (width, height) = (px(width * scale), px(height * scale));
        let on = config.caption;
        // The caption unfolds out of the pill's side the way it does in a session, and the
        // outline that brings it back fades in its place.
        let unfolded = self.caption.at();
        let shown = unfolded.clamp(0., 1.);
        let towards_pill = match config.edge {
            PillEdge::Bottom => caption::FOLDED_SHIFT,
            PillEdge::Top => -caption::FOLDED_SHIFT,
        };
        let outline = div()
            .id("studio-show-caption")
            .absolute()
            .inset_0()
            .rounded(caption::RADIUS * scale)
            .squircle()
            .border_1()
            .border_dashed()
            .border_color(theme.muted_foreground.opacity(0.6))
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .opacity(0.7 * resting * (1. - shown))
            .text_size(theme.text(Text::Small))
            .text_color(theme.muted_foreground)
            .child(
                svg()
                    .path(icons::path("icons/plus.svg"))
                    .size(px(14.))
                    .text_color(theme.muted_foreground),
            )
            .child("Live caption")
            .when(!on && resting == 1., |this| {
                this.hover(|style| style.opacity(resting).bg(theme.secondary.opacity(0.4)))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(|this, _, _, cx| this.edit(cx, |pill| pill.caption = true)),
                    )
            });

        let legible = ((scale - SKETCH_BELOW) / (LEGIBLE_ABOVE - SKETCH_BELOW)).clamp(0., 1.);
        let line = LINE_HEIGHT * scale;
        let sketch = |share: f32, color: gpui::Hsla| {
            div().h(line).flex().items_center().child(
                div()
                    .h(line * 0.4)
                    .w(gpui::relative(share))
                    .rounded_full()
                    .bg(color),
            )
        };
        let bubble = caption::bubble(scale, cx)
            .id("studio-caption")
            .absolute()
            .top_0()
            .left_0()
            .w(width)
            .h(height)
            .when(unfolded != 1., |this| {
                this.opacity((shown * 1.6).min(1.))
                    .layer_scale(
                        (caption::FOLDED_SCALE + (1. - caption::FOLDED_SCALE) * unfolded).max(0.01),
                    )
                    .layer_scale_origin(point(
                        0.5,
                        match config.edge {
                            PillEdge::Bottom => 1.,
                            PillEdge::Top => 0.,
                        },
                    ))
                    .layer_translate(point(px(0.), towards_pill * scale * (1. - shown)))
                    .blur(caption::FOLDED_BLUR * scale * (1. - shown))
            })
            .when(legible > 0., |this| {
                this.child(
                    div()
                        .opacity(legible)
                        .text_size(FONT_SIZE * scale)
                        .line_height(line)
                        .text_color(theme.foreground)
                        .child(WRITTEN)
                        .child(div().text_color(theme.muted_foreground).child(DRAFT)),
                )
            })
            .when(legible < 1., |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .px(caption::PADDING_X * scale)
                        .py(caption::PADDING_Y * scale)
                        .opacity(1. - legible)
                        .child(sketch(0.7, theme.foreground.opacity(0.8)))
                        .child(sketch(0.45, theme.muted_foreground.opacity(0.8))),
                )
            })
            .when(on && resting == 1., |this| {
                this.cursor_pointer()
                    .hover(|style| style.border_color(theme.muted_foreground))
                    .tip_label("Hide the live caption", Perch::Above)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.edit(cx, |pill| pill.caption = false)),
                    )
            });

        div()
            .relative()
            .flex_none()
            .w(width)
            .h(height)
            .when(shown < 1., |this| this.child(outline))
            .when(unfolded > 0.001, |this| this.child(bubble))
            .into_any_element()
    }

    /// The arrow between the pill and its edge of the stage, held to place the pill.
    fn arrow(
        &self,
        config: &PillConfig,
        pill_top: f32,
        resting: f32,
        held: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let bottom = config.edge == PillEdge::Bottom;
        let (top, height) = match bottom {
            true => (pill_top + pill::HEIGHT.as_f32(), ARROW_ROOM),
            false => (0., pill_top),
        };
        let (top, height) = (top + ARROW_INSET, height - ARROW_INSET * 2.);
        let line = || {
            div()
                .flex_1()
                .w(px(1.))
                .bg(theme.muted_foreground.opacity(0.35))
        };
        let chevron = match bottom {
            true => "icons/chevron-down.svg",
            false => "icons/chevron-up.svg",
        };
        let label = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_full()
            .bg(theme.secondary.opacity(0.8))
            .text_size(theme.text(Text::Small))
            .text_color(theme.muted_foreground)
            .child(
                svg()
                    .path(icons::path(chevron))
                    .size(px(14.))
                    .text_color(theme.muted_foreground),
            )
            .child(format!("{} px", config.margin.round()));
        div()
            .id("studio-distance")
            .absolute()
            .left(px((self.stage.get() - ARROW_WIDTH) / 2.))
            .top(px(top))
            .w(px(ARROW_WIDTH))
            .h(px(height))
            .flex()
            .flex_col()
            .items_center()
            .opacity(0.8 * resting)
            .cursor(CursorStyle::ResizeUpDown)
            .when(!held && resting == 1., |this| {
                this.tip_label("Hold and move the mouse up or down", Perch::Above)
                    .hover(|style| style.opacity(1.))
            })
            .child(line())
            .child(label)
            .child(line())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| this.begin(event, window, cx)),
            )
            .on_drag(Grip, |grip, _, _, cx| cx.new(|_| grip.clone()))
            .on_drag_move(
                cx.listener(|this, event: &gpui::DragMoveEvent<Grip>, _, cx| {
                    this.follow(event.event.position, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end(cx)),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Cancel,
    Confirm,
}

/// Splits `position` into the whole pixel layout puts it on and the fraction left over, which
/// the compositor adds while `moving`, so a moving part glides rather than stepping from pixel
/// to pixel. At rest the fraction is dropped and the part is drawn plainly.
fn adrift(position: f32, moving: bool, window: &Window) -> (Pixels, Pixels) {
    let laid = ui::snapped(px(position), window);
    match moving {
        true => (laid, px(position) - laid),
        false => (laid, px(0.)),
    }
}

/// A box `width` by `height` to the fraction, drawn by `face`. Layout puts every edge on a whole
/// pixel, which would walk a growing box from pixel to pixel and stop it with a hop, so the box
/// is laid out on whole pixels and drawn as four overlapping quarters, each pushed out to its
/// corner by the compositor, which keeps the fraction. The quarters overlap where the face is
/// flat, so no seam shows. At rest it is drawn plainly.
fn stretched(
    width: f32,
    height: f32,
    moving: bool,
    window: &Window,
    face: impl Fn() -> gpui::Div,
) -> gpui::Div {
    let laid = (
        ui::snapped(px(width), window),
        ui::snapped(px(height), window),
    );
    let rest = match moving {
        true => ((px(width) - laid.0) / 2., (px(height) - laid.1) / 2.),
        false => (px(0.), px(0.)),
    };
    let quarter = |right: bool, bottom: bool| {
        let anchor = |this: gpui::Div| {
            let this = match right {
                true => this.right_0(),
                false => this.left_0(),
            };
            match bottom {
                true => this.bottom_0(),
                false => this.top_0(),
            }
        };
        let shift = point(
            if right { rest.0 } else { -rest.0 },
            if bottom { rest.1 } else { -rest.1 },
        );
        anchor(
            div()
                .absolute()
                .w(laid.0 / 2. + px(SEAM))
                .h(laid.1 / 2. + px(SEAM))
                .overflow_hidden(),
        )
        .child(anchor(face().absolute().w(laid.0).h(laid.1)))
        .when(shift != point(px(0.), px(0.)), |this| {
            this.layer_translate(shift)
        })
    };
    div()
        .relative()
        .w(laid.0)
        .h(laid.1)
        .child(quarter(false, false))
        .child(quarter(true, false))
        .child(quarter(false, true))
        .child(quarter(true, true))
}

/// The scale the group is laid out at while it is drawn at `scale`, on its way between 1 and
/// `end`: one of a few steps from 1 to `end`, the nearest on the side it is heading for. The
/// compositor makes up the rest, never by much, so every edge moves smoothly, and the last
/// stretch before it lands is already laid out as it will rest, so turning the layer off at the
/// end changes nothing.
fn layout_scale(scale: f32, end: f32, to_end: bool) -> f32 {
    if end >= 1. {
        return 1.;
    }
    let steps = (end.ln() / MAX_SHRINK.ln()).ceil().max(1.);
    let step = end.powf(1. / steps);
    let along = scale.max(f32::MIN_POSITIVE).ln() / step.ln();
    let k = match to_end {
        true => along.ceil(),
        false => along.floor(),
    };
    step.powf(k.clamp(0., steps))
}

/// A spring already resting at `on`.
fn landed(config: SpringConfig, on: bool) -> Spring {
    let mut spring = Spring::new(config);
    spring.aim(on as u8 as f32, config);
    spring.finish();
    spring
}

/// The pill's top on a monitor `height` tall, for a pill `margin` from `edge`.
fn top_of(edge: PillEdge, margin: f32, height: f32) -> f32 {
    let top = match edge {
        PillEdge::Top => margin,
        PillEdge::Bottom => height - pill::HEIGHT.as_f32() - margin,
    };
    clamp_top(top, height)
}

fn clamp_top(top: f32, height: f32) -> f32 {
    top.clamp(0., (height - pill::HEIGHT.as_f32()).max(0.))
}

/// The edge a pill with its top at `top` sits on, the nearer one, and its distance from it.
fn placement(top: f32, height: f32) -> (PillEdge, f32) {
    let pill = pill::HEIGHT.as_f32();
    match top + pill / 2. < height / 2. {
        true => (PillEdge::Top, top.round()),
        false => (PillEdge::Bottom, (height - pill - top).round()),
    }
}

fn lerp(from: f32, to: f32, by: f32) -> f32 {
    from + (to - from) * by
}

/// Bars that rise and fall like a voice, loudest in the low middle bands.
fn speech(time: f32) -> [f32; BANDS] {
    let syllables = (0.5 + 0.5 * (time * TAU * 2.3).sin()) * (0.6 + 0.4 * (time * TAU * 0.7).sin());
    std::array::from_fn(|band| {
        let place = band as f32 / (BANDS - 1) as f32;
        let shape = (-((place - 0.35) / 0.28).powi(2)).exp();
        let flicker = 0.85 + 0.15 * (time * TAU * 5.1 + band as f32 * 1.7).sin();
        (shape * (0.45 + 0.55 * syllables) * flicker).clamp(0., 1.)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_round_trips() {
        for (edge, margin) in [
            (PillEdge::Bottom, 48.),
            (PillEdge::Top, 0.),
            (PillEdge::Top, 300.),
        ] {
            let top = top_of(edge, margin, 1080.);
            assert_eq!(placement(top, 1080.), (edge, margin));
        }
    }

    #[test]
    fn the_group_lands_laid_out_as_it_rests() {
        let end = 0.27;
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        // Just short of either end, it is already laid out as it will rest there.
        assert!(close(layout_scale(end * 1.01, end, true), end));
        assert!(close(layout_scale(0.99, end, false), 1.));
        assert!(close(layout_scale(1., end, true), 1.));
        assert!(close(layout_scale(end, end, false), end));
        for scale in [0.95, 0.8, 0.6, 0.41, 0.3] {
            for to_end in [true, false] {
                let ratio = scale / layout_scale(scale, end, to_end);
                assert!(
                    (MAX_SHRINK - 1e-4..=1. / MAX_SHRINK + 1e-4).contains(&ratio),
                    "{scale} drawn at {ratio} of its layout"
                );
            }
        }
    }

    #[test]
    fn crossing_the_middle_moves_the_pill_to_the_other_edge() {
        assert_eq!(placement(100., 1080.).0, PillEdge::Top);
        assert_eq!(placement(900., 1080.).0, PillEdge::Bottom);
        assert_eq!(clamp_top(-50., 1080.), 0.);
        assert_eq!(clamp_top(5000., 1080.), 1080. - pill::HEIGHT.as_f32());
    }
}
