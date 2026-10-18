//! The windows: the pill and its caption and the settings window, which walks through the first
//! run before it shows any settings, all drawn with Sonora's `ui` crate.

pub mod caption;
mod chrome;
// Off Linux there is no compositor to give binding directions for, so most of this goes unused.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod desktop;
mod grab;
mod host;
mod hotkey;
mod markdown;
mod models;
mod overlay;
pub mod pill;
mod radius;
mod screen;
pub mod settings;
pub mod setup;
mod spring;
mod studio;
mod text_morph;
pub mod themes;
pub mod updates;
mod usage;

use gpui::App;

pub use caption::Caption;
pub use host::{Action, Files, Host, Library, ModelState, Status, Store, entities};
pub use pill::{Phase, Pill};
pub use radius::nested_radius;

/// The path the download icon is drawn from. Sonora's packs have none, so the app ships its own.
pub const DOWNLOAD_ICON: &str = "tokativ/download.svg";

/// Builds the theme again from the selected theme and the desktop's light or dark preference.
/// Called before each window opens and whenever the preference changes, since the portal
/// reports it some time after startup.
pub fn refresh_theme(cx: &mut App) {
    themes::refresh(cx);
}
