//! The frame of the settings and update windows: a client-drawn title bar with Sonora's
//! window controls, rounded corners, and the resize edges.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Decorations, MouseButton, Pixels, Size, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions, div, px,
};
use tokativ_config::APP_ID;
#[cfg(target_os = "linux")]
use ui::WindowFrame;
use ui::{ActiveTheme as _, Text, WindowControls};

/// Room the title leaves on its left. macOS draws its window buttons over the title bar there.
const TITLE_INSET: Pixels = match cfg!(target_os = "macos") {
    true => px(78.),
    false => px(16.),
};

/// The options for a normal toplevel of `size` with client-side decorations.
pub fn options(title: &str, size: Size<Pixels>, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(title.to_owned().into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        app_id: Some(APP_ID.into()),
        window_background: WindowBackgroundAppearance::Transparent,
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(Size::new(px(480.), px(420.))),
        is_movable: true,
        is_resizable: true,
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// The corner radius of the window itself. Square when tiled, maximized or decorated by the
/// compositor, since those sit flush against something.
pub fn window_radius(window: &Window) -> Pixels {
    match window.window_decorations() {
        Decorations::Client { tiling } if !tiling.is_tiled() && !window.is_maximized() => {
            WINDOW_RADIUS
        }
        _ => Pixels::ZERO,
    }
}

/// Wraps `content` in the window frame with a title bar reading `title`.
pub fn frame(title: &str, content: AnyElement, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let radius = window_radius(window);
    let height = ui::snapped(theme.metrics.title_bar, window);
    let client = matches!(window.window_decorations(), Decorations::Client { .. });

    let title_bar = div()
        .id("title-bar")
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .w_full()
        .h(height)
        .pl(TITLE_INSET)
        .bg(theme.background)
        .border_b_1()
        .border_color(theme.title_bar_border)
        .window_control_area(gpui::WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            match event.click_count {
                2 => window.zoom_window(),
                _ => window.start_window_move(),
            }
        })
        .child(
            div()
                .text_size(theme.text(Text::Label))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(title.to_owned()),
        )
        .when(client, |this| {
            this.child(div().flex_none().pr_2().child(WindowControls::new(false)))
        });

    let frame = div()
        .size_full()
        .flex()
        .flex_col()
        .font_family("Inter")
        .text_size(theme.text(Text::Body))
        .text_color(theme.foreground)
        .bg(theme.background)
        .rounded(radius)
        .squircle()
        .overflow_hidden()
        // Middle-button auto-scrolling, wired the way Sonora's root wires it: any press ends the
        // mode, a held middle drag ends on release, Escape ends it, and the pointer steers it.
        .capture_any_mouse_down(|_, window, cx| {
            if ui::cancel_middle_scroll(cx) {
                window.refresh();
                cx.stop_propagation();
            }
        })
        .capture_any_mouse_up(|event, window, cx| {
            if event.button == MouseButton::Middle && ui::release_middle_scroll(event.position, cx)
            {
                window.refresh();
            }
        })
        .on_mouse_up_out(MouseButton::Middle, |event, window, cx| {
            if ui::release_middle_scroll(event.position, cx) {
                window.refresh();
            }
        })
        .capture_key_down(|event, window, cx| {
            if event.keystroke.key == "escape" && ui::cancel_middle_scroll(cx) {
                window.refresh();
                cx.stop_propagation();
            }
        })
        .on_mouse_move(|event, window, cx| ui::update_middle_scroll(event.position, window, cx))
        .child(title_bar)
        .child(div().flex_1().min_h_0().child(content));
    // The resize edges and shadow of a client-decorated window on Wayland. macOS and Windows
    // draw their own.
    #[cfg(target_os = "linux")]
    let frame = frame.child(WindowFrame::new());
    frame.into_any_element()
}

/// The corner radius of a raised surface such as a card. Anything inset in one takes its corners
/// from this through [`crate::nested_radius`].
pub const GROUP_RADIUS: Pixels = px(14.);
/// How far a card's content sits from its edges.
pub const GROUP_PADDING: Pixels = px(16.);
/// The corner radius of a free-floating window, one step rounder than the cards inside it so
/// no inner curve is rounder than the frame around it.
pub const WINDOW_RADIUS: Pixels = px(18.);
/// How far a list sits inside the raised surface around it.
pub const LISTED_INSET: Pixels = px(4.);

/// A horizontal track filled to `fraction`, with the fill's corners nested inside the track's.
pub fn meter(fraction: f32, height: Pixels, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let inset = px(2.);
    let outer = height / 2.;
    div()
        .w_full()
        .h(height)
        .p(inset)
        .rounded(outer)
        .bg(theme.muted)
        .child(
            div()
                .h_full()
                .w(gpui::relative(fraction.clamp(0., 1.)))
                .min_w(height - inset * 2.)
                .rounded(crate::nested_radius(outer, inset))
                .bg(theme.progress_bar),
        )
}

/// A box of monospaced text, such as a config snippet to copy, shaped like the cards around it.
#[cfg(target_os = "linux")]
pub fn code(text: &str, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .w_full()
        .px(GROUP_PADDING)
        .py_3()
        .rounded(GROUP_RADIUS)
        .squircle()
        .bg(theme.secondary.opacity(0.45))
        .border_1()
        .border_color(theme.border.opacity(0.6))
        .font_family("monospace")
        .text_size(theme.text(Text::Small))
        .child(text.to_owned())
}

/// Bytes as megabytes or gigabytes, the way the model list shows sizes.
pub fn size(bytes: u64) -> String {
    let megabytes = bytes as f64 / 1_000_000.;
    match megabytes >= 1000. {
        true => format!("{:.1} GB", megabytes / 1000.),
        false => format!("{megabytes:.0} MB"),
    }
}

/// How many strips the header blur fades in through. Each is one quad, so more of them only
/// make the crossfade smoother.
const STRIPS: usize = 64;
/// The curve of the crossfade, as the exponent on a strip's distance from the solid edge. Below
/// one, the haze reaches working strength quickly and only the far edge reads as clear.
const HAZE: f32 = 0.4;

/// A band that blurs what passes under it, at full strength along its top and fading to nothing
/// across `height`, so a header floating over content stays readable without a hard line where
/// the blur stops. It fills its parent, which has to be `height` tall and `relative`. Sonora
/// floats its settings header over the same band.
pub fn veil(height: Pixels, blur: Pixels, window: &Window) -> impl IntoElement {
    let edges: Vec<Pixels> = (0..=STRIPS)
        .map(|slice| ui::snapped(height * (slice as f32 / STRIPS as f32), window))
        .collect();
    let strips = edges.windows(2).filter_map(move |span| {
        let cut = span[1] - span[0];
        let across = (span[0] + cut / 2.) / height;
        let held = 1. - across;
        (cut > Pixels::ZERO).then(|| {
            div()
                .flex_none()
                .w_full()
                .h(cut)
                .opacity(held.powf(HAZE))
                .backdrop_blur(blur)
        })
    });
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .children(strips.collect::<Vec<_>>())
}
