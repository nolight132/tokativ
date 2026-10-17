//! The tray item: a StatusNotifierItem on Linux, a menu bar or notification area icon on macOS
//! and Windows. Menu clicks travel to the daemon over a channel, and the daemon pushes the
//! recording state and the models back.

#[cfg(not(target_os = "linux"))]
mod native;
#[cfg(target_os = "linux")]
mod sni;

#[cfg(not(target_os = "linux"))]
pub use native::Tray;
#[cfg(target_os = "linux")]
pub use sni::Tray;

/// The width and height of the drawn icon, in pixels.
const SIZE: usize = 32;

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

/// The tray icon as straight RGBA rows: a hollow ring at rest, a solid red dot while
/// recording.
fn pixels(recording: bool) -> Vec<[u8; 4]> {
    let center = (SIZE as f32 - 1.) / 2.;
    let outer = SIZE as f32 * 0.42;
    let inner = SIZE as f32 * 0.18;
    let mut pixels = Vec::with_capacity(SIZE * SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let distance = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
            let coverage = (outer + 0.5 - distance).clamp(0., 1.);
            let (alpha, rgb) = match recording {
                true => (coverage, [0xE5, 0x3E, 0x3E]),
                false => {
                    let ring = coverage * (distance - (outer - 3.) + 0.5).clamp(0., 1.);
                    let dot = (inner + 0.5 - distance).clamp(0., 1.);
                    (ring.max(dot), [0xF2, 0xF2, 0xF2])
                }
            };
            pixels.push([rgb[0], rgb[1], rgb[2], (alpha * 255.) as u8]);
        }
    }
    pixels
}
