//! The color themes: Sonora's built-in ones and the custom ones in the themes folder, which are
//! JSON files in the format Sonora reads, so a theme written for one works in the other.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use gpui::{App, Global};
use serde::Deserialize;
use ui::{Look, Rounding, Theme, ThemeKind, ThemeOverrides};

/// The only theme file format there is so far.
const FORMAT_VERSION: u32 = 1;

/// The themes that come with the app. Sonora's other palettes are left out on purpose.
pub const BUILT_IN: [ThemeKind; 3] = [ThemeKind::System, ThemeKind::Dark, ThemeKind::Light];

/// A theme from the themes folder, laid over the dark theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Custom {
    /// The file name without `.json`, which the config names the theme by.
    pub id: String,
    pub name: String,
    pub overrides: ThemeOverrides,
}

#[derive(Deserialize)]
struct ThemeFile {
    name: String,
    author: String,
    version: u32,
    theme: ThemeOverrides,
}

/// The theme the config picks and the custom themes on disk.
#[derive(Default)]
struct Themes {
    selected: String,
    custom: Vec<Custom>,
}

impl Global for Themes {}

/// The folder custom themes are read from.
pub fn folder() -> PathBuf {
    tokativ_config::config_dir().join("themes")
}

/// Reads the themes folder and applies the theme with the id `selected` without a transition.
/// Call once before the first window opens.
pub fn init(selected: &str, cx: &mut App) {
    cx.set_global(Themes {
        selected: selected.to_owned(),
        custom: load(&folder(), &[]),
    });
    Theme::init(look(cx), &overrides(cx), cx);
}

/// Switches to the theme with the id `selected`, fading the open windows over.
pub fn select(selected: &str, cx: &mut App) {
    if cx.default_global::<Themes>().selected == selected {
        return;
    }
    cx.global_mut::<Themes>().selected = selected.to_owned();
    Theme::fade(look(cx), &overrides(cx), cx);
}

/// Reads the themes folder again and applies any change to the selected theme.
pub fn reload(cx: &mut App) {
    let themes = cx.default_global::<Themes>();
    let custom = load(&folder(), &themes.custom);
    if themes.custom == custom {
        return;
    }
    themes.custom = custom;
    Theme::set(look(cx), &overrides(cx), cx);
}

/// Builds the theme again, for when the desktop's light or dark preference may have changed.
pub fn refresh(cx: &mut App) {
    cx.default_global::<Themes>();
    Theme::set(look(cx), &overrides(cx), cx);
}

/// The custom themes, sorted by name.
pub fn custom(cx: &App) -> &[Custom] {
    cx.try_global::<Themes>()
        .map_or(&[], |themes| themes.custom.as_slice())
}

/// The name of the theme with `id`, if there is one.
pub fn name(id: &str, cx: &App) -> Option<String> {
    if let Some(kind) = BUILT_IN.into_iter().find(|kind| kind.id() == id) {
        return Some(kind.label().to_string());
    }
    custom(cx)
        .iter()
        .find(|theme| theme.id == id)
        .map(|theme| theme.name.clone())
}

/// A theme as the appearance page shows it, with the palettes it paints in: two for the system
/// theme, which follows the desktop between light and dark.
pub struct Swatch {
    pub id: String,
    pub name: String,
    pub palettes: Vec<Theme>,
}

/// Every theme there is to pick, built-in ones first.
pub fn swatches(cx: &App) -> Vec<Swatch> {
    let built = |kind| Theme::for_look(look_of(kind), &ThemeOverrides::default());
    let built_in = BUILT_IN.into_iter().map(|kind| Swatch {
        id: kind.id().to_owned(),
        name: kind.label().to_string(),
        palettes: match kind {
            ThemeKind::System => vec![built(ThemeKind::Light), built(ThemeKind::Dark)],
            kind => vec![built(kind)],
        },
    });
    let custom = custom(cx).iter().map(|theme| Swatch {
        id: theme.id.clone(),
        name: theme.name.clone(),
        palettes: vec![Theme::for_look(look_of(ThemeKind::Dark), &theme.overrides)],
    });
    built_in.chain(custom).collect()
}

fn look(cx: &App) -> Look {
    let selected = cx
        .try_global::<Themes>()
        .map_or("system", |themes| themes.selected.as_str());
    // A custom theme is laid over the dark one.
    look_of(
        BUILT_IN
            .into_iter()
            .find(|kind| kind.id() == selected)
            .unwrap_or(ThemeKind::Dark),
    )
}

fn look_of(kind: ThemeKind) -> Look {
    Look {
        kind,
        rounding: Rounding::Rounded,
        font: 14.,
        transparent: false,
        transparency: 0.,
        blur: false,
        blur_window: false,
        tint: None,
        tint_secondary: None,
    }
}

fn overrides(cx: &App) -> ThemeOverrides {
    let Some(themes) = cx.try_global::<Themes>() else {
        return ThemeOverrides::default();
    };
    themes
        .custom
        .iter()
        .find(|theme| theme.id == themes.selected)
        .map(|theme| theme.overrides.clone())
        .unwrap_or_default()
}

/// Every valid theme file directly in `folder`, sorted by name. A file that does not parse keeps
/// its version in `previous`, if it had one, so a save in progress does not flash the theme away.
/// An empty file is one being written and is skipped quietly. A file named like a built-in theme
/// is left out.
fn load(folder: &Path, previous: &[Custom]) -> Vec<Custom> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut themes: Vec<Custom> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let id = path.file_stem()?.to_str()?.to_owned();
            if BUILT_IN.into_iter().any(|kind| kind.id() == id) {
                log::warn!("themes: {} has a built-in theme's name", path.display());
                return None;
            }
            let kept = || previous.iter().find(|theme| theme.id == id).cloned();
            let bytes = match std::fs::read(&path) {
                Ok(bytes) if bytes.is_empty() => return kept(),
                Ok(bytes) => bytes,
                Err(error) => {
                    log::warn!("themes: cannot read {}: {error}", path.display());
                    return kept();
                }
            };
            match parse(&bytes, id.clone()) {
                Ok(theme) => Some(theme),
                Err(error) => {
                    log::warn!("themes: {}: {error:#}", path.display());
                    kept()
                }
            }
        })
        .collect();
    themes.sort_by_key(|theme| theme.name.to_lowercase());
    themes
}

/// Parses one theme file, refusing anything Sonora would refuse.
fn parse(bytes: &[u8], id: String) -> Result<Custom> {
    // Notepad and PowerShell can put a byte order mark in front, which serde_json rejects.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: serde_json::Value = serde_json::from_slice(bytes).context("parsing the JSON")?;
    let object = value.as_object().context("the file must hold an object")?;
    for field in object.keys() {
        anyhow::ensure!(
            matches!(field.as_str(), "name" | "author" | "version" | "theme"),
            "unknown field `{field}`"
        );
    }
    let theme = object
        .get("theme")
        .and_then(serde_json::Value::as_object)
        .context("`theme` must hold an object")?;
    for field in theme.keys() {
        anyhow::ensure!(
            ThemeOverrides::is_color(field),
            "`{field}` is not a theme color"
        );
    }
    let file: ThemeFile = serde_json::from_value(value).context("decoding the theme")?;
    anyhow::ensure!(
        file.version == FORMAT_VERSION,
        "unsupported format version {}",
        file.version
    );
    let name = file.name.trim();
    anyhow::ensure!(!name.is_empty(), "the name is empty");
    anyhow::ensure!(!file.author.trim().is_empty(), "the author is empty");
    if let Some(field) = file.theme.invalid_color() {
        anyhow::bail!("theme.{field} is not a valid color");
    }
    Ok(Custom {
        id,
        name: name.to_owned(),
        overrides: file.theme,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_sonora_theme() {
        let theme = parse(
            br##"{"name": "Nord", "author": "Someone", "version": 1,
                 "theme": {"background": "#2e3440", "primary": "#88c0d0"}}"##,
            "nord".into(),
        )
        .unwrap();
        assert_eq!(theme.name, "Nord");
        assert_eq!(theme.overrides.background.as_deref(), Some("#2e3440"));
    }

    #[test]
    fn refuses_what_sonora_refuses() {
        let parsed = |json: &str| parse(json.as_bytes(), "x".into());
        assert!(parsed(r#"{"name": "A", "author": "B", "version": 2, "theme": {}}"#).is_err());
        assert!(
            parsed(r#"{"name": "A", "author": "B", "version": 1, "theme": {"radius": 4}}"#)
                .is_err()
        );
        assert!(
            parsed(
                r#"{"name": "A", "author": "B", "version": 1, "theme": {"background": "nope"}}"#
            )
            .is_err()
        );
        assert!(parsed(r#"{"name": " ", "author": "B", "version": 1, "theme": {}}"#).is_err());
        assert!(
            parsed(r#"{"name": "A", "author": "B", "version": 1, "theme": {}, "x": 1}"#).is_err()
        );
    }
}
