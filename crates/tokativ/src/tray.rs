//! The tray item: a StatusNotifierItem on Linux, a menu bar or notification area icon on macOS
//! and Windows. Menu clicks travel to the daemon over a channel, and the daemon pushes the
//! recording state and the models back.

#[cfg(not(target_os = "linux"))]
mod native;
#[cfg(target_os = "linux")]
mod sni;

use resvg::{tiny_skia, usvg};

#[cfg(not(target_os = "linux"))]
pub use native::Tray;
#[cfg(target_os = "linux")]
pub use sni::Tray;

const STANDBY: &str = include_str!("../../../assets/icons/mic.svg");
const RECORDING: &str = include_str!("../../../assets/icons/circle.svg");

/// The width and height of the drawn icon, in pixels. tray-icon shows the image on macOS at one
/// point per pixel, capped at the 22pt menu bar, so 44 pixels fill it at 2x without upscaling.
#[cfg(target_os = "macos")]
const SIZE: usize = 44;
/// The width and height of the drawn icon, in pixels. 32 is the notification area icon at 200%
/// on Windows and a hidpi panel on Linux; smaller panels scale it down.
#[cfg(not(target_os = "macos"))]
const SIZE: usize = 32;

/// The width and height the SVG is drawn at, centered in the canvas. Menu bar icons on macOS are
/// 16pt in the 22pt bar; Windows and the Linux panels pad the icon themselves.
#[cfg(target_os = "macos")]
const GLYPH: usize = 32;
#[cfg(not(target_os = "macos"))]
const GLYPH: usize = SIZE;

/// What a tray click asks of the daemon.
#[derive(Clone, Debug)]
pub enum TrayEvent {
    Toggle,
    /// Switch to the installed model with this name, `owner/repository/file.gguf`.
    Model(String),
    /// Write in the profile with this name.
    Profile(String),
    Settings,
    #[cfg(not(target_os = "linux"))]
    Updates,
    Quit,
}

/// What the tray menu lists: the model in use and every installed one, each with its full name
/// and the name to show, then the writing profile in use and every profile by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub model: String,
    pub installed: Vec<(String, String)>,
    pub profile: String,
    pub profiles: Vec<String>,
}

/// The tray icon as straight RGBA rows: a white Lucide microphone at rest, a red dot while
/// recording.
fn pixels(recording: bool) -> Vec<[u8; 4]> {
    let color = match recording {
        true => "#E53E3E",
        false => "#F2F2F2",
    };
    let svg = match recording {
        true => RECORDING,
        false => STANDBY,
    };
    let svg = svg.replace("currentColor", color);
    let tree =
        usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("the tray icon is valid svg");

    let mut pixmap = tiny_skia::Pixmap::new(SIZE as u32, SIZE as u32).expect("SIZE is not zero");
    let scale = GLYPH as f32 / tree.size().width();
    let inset = (SIZE - GLYPH) as f32 / 2.;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale).post_translate(inset, inset),
        &mut pixmap.as_mut(),
    );

    pixmap
        .pixels()
        .iter()
        .map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect()
}
