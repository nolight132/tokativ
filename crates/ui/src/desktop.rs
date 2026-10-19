//! Works out which desktop the user is in, how a keybinding is added there, and whether one for
//! Tokativ already exists, so setup can give directions that fit.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How soon a second start of System Settings exits once it handed its page to the window
/// already open. One that runs longer opened a window of its own.
const HANDOVER: Duration = Duration::from_secs(5);

/// Brings System Settings to the current desktop and in front, as a KWin script, which KWin
/// lets take focus where a program asking for it could not.
#[cfg(target_os = "linux")]
const RAISE_SYSTEM_SETTINGS: &str = r#"
for (const window of workspace.windowList()) {
    if (window.resourceClass !== "systemsettings") {
        continue;
    }
    if (window.desktops.length > 0 && !window.desktops.includes(workspace.currentDesktop)) {
        window.desktops = [workspace.currentDesktop];
    }
    window.minimized = false;
    workspace.activeWindow = window;
}
"#;

/// The compositor or desktop environment of this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    Niri,
    Hyprland,
    Sway,
    River,
    Plasma,
    Gnome,
    Unknown,
}

/// A keybinding for Tokativ found in the desktop's config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    /// The keys, as the config spells them.
    pub keys: String,
    /// The file the binding is in.
    pub file: PathBuf,
}

impl Desktop {
    /// Reads the session from the variables each compositor sets for its clients.
    pub fn detect() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        if set("NIRI_SOCKET") {
            return Self::Niri;
        }
        if set("HYPRLAND_INSTANCE_SIGNATURE") {
            return Self::Hyprland;
        }
        if set("SWAYSOCK") {
            return Self::Sway;
        }
        let current = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();
        if current.split(':').any(|part| part == "river") {
            Self::River
        } else if current.contains("kde") {
            Self::Plasma
        } else if current.contains("gnome") {
            Self::Gnome
        } else {
            Self::Unknown
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Niri => "niri",
            Self::Hyprland => "Hyprland",
            Self::Sway => "Sway",
            Self::River => "river",
            Self::Plasma => "KDE Plasma",
            Self::Gnome => "GNOME",
            Self::Unknown => "your desktop",
        }
    }

    /// Whether the pill can show here. GNOME has no layer shell.
    pub fn supported(self) -> bool {
        self != Self::Gnome
    }

    /// The config file a binding goes in, for desktops configured by file.
    pub fn config_file(self) -> Option<PathBuf> {
        let config = dirs::config_dir()?;
        match self {
            Self::Niri => Some(config.join("niri/config.kdl")),
            Self::Hyprland => Some(config.join("hypr/hyprland.conf")),
            Self::Sway => Some(config.join("sway/config")),
            Self::River => Some(config.join("river/init")),
            Self::Plasma | Self::Gnome | Self::Unknown => None,
        }
    }

    /// What to do, in a sentence.
    pub fn directions(self) -> &'static str {
        match self {
            Self::Niri => {
                "Add this line to the binds section of your niri config. niri picks the change up on save. Tap the key to start and finish dictating, or hold it while you talk."
            }
            Self::Hyprland => {
                "Add these lines to your Hyprland config. Hyprland reloads it on save. Tap the key to start and finish dictating, or hold it while you talk."
            }
            Self::Sway => {
                "Add these lines to your Sway config, then reload Sway with Mod+Shift+C. Tap the key to start and finish dictating, or hold it while you talk."
            }
            Self::River => {
                "Add these lines to your river init script and run them once, or restart river. Tap the key to start and finish dictating, or hold it while you talk."
            }
            Self::Plasma => {
                "Tokativ adds a Dictate shortcut to System Settings, on Meta+Shift+D unless another app has it. Tap the key to start and finish dictating, or hold it while you talk. To pick another key, open Shortcuts and find Tokativ. A modifier alone, such as Shift, only reports when it comes up, so it cannot be held."
            }
            Self::Gnome => {
                "GNOME has no layer shell, so the pill cannot show there. Tokativ needs niri, Hyprland, Sway, river or Plasma."
            }
            Self::Unknown => "Bind a key in your compositor to run this command.",
        }
    }

    /// The text to add, in the config's own syntax. Where the compositor runs a command when a key
    /// comes up, the key is bound twice so holding it works as push to talk. niri cannot, so its
    /// binding repeats while held and the repeats stand in for the key being down.
    pub fn snippet(self) -> &'static str {
        match self {
            Self::Niri => "Mod+D { spawn \"tokativ\" \"hold\"; }",
            Self::Hyprland => {
                "bind = SUPER, D, exec, tokativ press\nbindr = SUPER, D, exec, tokativ release"
            }
            Self::Sway => {
                "bindsym --no-repeat $mod+d exec tokativ press\nbindsym --release $mod+d exec tokativ release"
            }
            Self::River => {
                "riverctl map normal Super D spawn 'tokativ press'\nriverctl map -release normal Super D spawn 'tokativ release'"
            }
            Self::Plasma | Self::Gnome | Self::Unknown => "tokativ toggle",
        }
    }

    /// Whether Tokativ binds its own key here, so setup shows where to change it rather than a
    /// binding to copy.
    pub fn own_shortcut(self) -> bool {
        self == Self::Plasma
    }

    /// Opens the desktop's shortcut settings, where Tokativ's own key is changed.
    pub fn open_shortcuts(self) {
        if self != Self::Plasma {
            return;
        }
        let mut child = match std::process::Command::new("systemsettings")
            .arg("kcm_keys")
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                log::error!("setup: cannot open the shortcut settings: {error}");
                return;
            }
        };
        let started = Instant::now();
        // System Settings runs once. Starting it again hands the page to the window already open
        // and exits, and KWin leaves that window where it is, behind others or on another
        // desktop, since nothing gave it leave to take focus. A KWin script may raise it.
        std::thread::spawn(move || {
            let handed_over =
                child.wait().is_ok_and(|status| status.success()) && started.elapsed() < HANDOVER;
            #[cfg(target_os = "linux")]
            if handed_over && let Err(error) = raise_system_settings() {
                log::warn!("setup: cannot bring System Settings forward: {error:#}");
            }
            #[cfg(not(target_os = "linux"))]
            let _ = handed_over;
        });
    }

    /// Looks through the desktop's config files for a line that starts dictation: `tokativ toggle`,
    /// `press` or `hold`. On Plasma, the key Tokativ's own shortcut has.
    pub fn bound(self) -> Option<Bound> {
        if self == Self::Plasma {
            return plasma_bound();
        }
        let file = self.config_file()?;
        let folder = file.parent()?.to_path_buf();
        let mut files = vec![file];
        // niri and Sway configs often include sibling files, so those are read as well.
        if let Ok(entries) = fs::read_dir(&folder) {
            let mut siblings: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_file() && !files.contains(path))
                .collect();
            siblings.sort();
            files.extend(siblings);
        }
        files.into_iter().find_map(|file| {
            let text = fs::read_to_string(&file).ok()?;
            let line = text.lines().find(|line| {
                let line = line.trim_start();
                !line.starts_with("//") && !line.starts_with('#') && starts_dictation(line)
            })?;
            Some(Bound {
                keys: keys(self, line)?,
                file,
            })
        })
    }
}

/// Runs [`RAISE_SYSTEM_SETTINGS`] through KWin's scripting service.
#[cfg(target_os = "linux")]
fn raise_system_settings() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use zbus::blocking::{Connection, Proxy};

    const PLUGIN: &str = "tokativ-raise-system-settings";
    let file = dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("{PLUGIN}.js"));
    fs::write(&file, RAISE_SYSTEM_SETTINGS).context("writing the script")?;
    let bus = Connection::session().context("connecting to the session bus")?;
    let scripting = Proxy::new(&bus, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting")
        .context("reaching KWin's scripting")?;
    // A script left from an earlier run would keep the name taken.
    let _: bool = scripting
        .call("unloadScript", &(PLUGIN,))
        .context("unloading the last script")?;
    let id: i32 = scripting
        .call("loadScript", &(file.to_string_lossy().as_ref(), PLUGIN))
        .context("loading the script")?;
    let script = Proxy::new(
        &bus,
        "org.kde.KWin",
        format!("/Scripting/Script{id}"),
        "org.kde.kwin.Script",
    )
    .context("reaching the script")?;
    script
        .call_method("run", &())
        .context("running the script")?;
    script
        .call_method("stop", &())
        .context("stopping the script")?;
    fs::remove_file(&file).ok();
    Ok(())
}

/// The key of Tokativ's Dictate shortcut as kglobalaccel saved it, in a line such as
/// `dictate=Meta+Shift+D,Meta+Shift+D,Dictate` under `[tokativ]`: the keys in use, the default
/// and the name.
fn plasma_bound() -> Option<Bound> {
    let file = dirs::config_dir()?.join("kglobalshortcutsrc");
    let text = fs::read_to_string(&file).ok()?;
    let keys = plasma_keys(&text)?;
    Some(Bound { keys, file })
}

fn plasma_keys(text: &str) -> Option<String> {
    let mut ours = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            ours = line == "[tokativ]";
            continue;
        }
        let Some(value) = line.strip_prefix("dictate=").filter(|_| ours) else {
            continue;
        };
        // Several keys for one shortcut are separated by tabs, which the file writes escaped.
        let keys = value.split(',').next()?.trim();
        return (!keys.is_empty() && !keys.eq_ignore_ascii_case("none"))
            .then(|| keys.replace("\\t", "\t").replace('\t', " or "));
    }
    None
}

fn starts_dictation(line: &str) -> bool {
    line.contains("tokativ")
        && ["toggle", "press", "hold"]
            .iter()
            .any(|verb| line.contains(verb))
}

/// The key combination at the start of a binding line, in each config's syntax.
fn keys(desktop: Desktop, line: &str) -> Option<String> {
    let line = line.trim();
    match desktop {
        Desktop::Niri => line.split_whitespace().next().map(str::to_owned),
        Desktop::Sway => line
            .split_whitespace()
            .skip(1)
            .find(|word| !word.starts_with("--"))
            .map(str::to_owned),
        Desktop::Hyprland => {
            let (_, rest) = line.split_once('=')?;
            let mut fields = rest.split(',').map(str::trim);
            let modifiers = fields.next()?;
            let key = fields.next()?;
            Some(match modifiers.is_empty() {
                true => key.to_owned(),
                false => format!("{} + {key}", modifiers.replace(' ', " + ")),
            })
        }
        Desktop::River => {
            let words: Vec<&str> = line.split_whitespace().collect();
            let at = words.iter().position(|word| *word == "map")?;
            // Flags such as -release come before the mode, which comes before the keys.
            let mut rest = words[at + 1..].iter().filter(|word| !word.starts_with('-'));
            rest.next()?;
            Some(format!("{}+{}", rest.next()?, rest.next()?))
        }
        Desktop::Plasma | Desktop::Gnome | Desktop::Unknown => None,
    }
}

/// Opens a config file in the user's editor, creating its folder first if needed.
pub fn open(file: &Path) {
    if let Some(folder) = file.parent() {
        fs::create_dir_all(folder).ok();
    }
    if let Err(error) = std::process::Command::new("xdg-open").arg(file).spawn() {
        log::error!("setup: cannot open {}: {error}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_come_out_of_each_syntax() {
        assert_eq!(
            keys(
                Desktop::Niri,
                "\tMod+D hotkey-overlay-title=\"x\" { spawn \"tokativ\" \"toggle\"; }"
            )
            .as_deref(),
            Some("Mod+D")
        );
        assert_eq!(
            keys(
                Desktop::Hyprland,
                "bind = SUPER SHIFT, D, exec, tokativ toggle"
            )
            .as_deref(),
            Some("SUPER + SHIFT + D")
        );
        assert_eq!(
            keys(Desktop::Sway, "bindsym $mod+d exec tokativ toggle").as_deref(),
            Some("$mod+d")
        );
        assert_eq!(
            keys(
                Desktop::River,
                "riverctl map normal Super D spawn 'tokativ toggle'"
            )
            .as_deref(),
            Some("Super+D")
        );
    }

    #[test]
    fn plasma_shortcut_is_read_from_its_section() {
        let config = "[kwin]\ndictate=Meta+D,none,Other\n\n[tokativ]\n_k_friendly_name=Tokativ\ndictate=Meta+Shift+D,Meta+Shift+D,Dictate\n";
        assert_eq!(plasma_keys(config).as_deref(), Some("Meta+Shift+D"));
        assert_eq!(
            plasma_keys("[tokativ]\ndictate=none,Meta+Shift+D,Dictate\n"),
            None
        );
        assert_eq!(plasma_keys("[kwin]\ndictate=Meta+D,none,Other\n"), None);
        assert_eq!(
            plasma_keys("[tokativ]\ndictate=F13\\tMeta+Shift+D,Meta+Shift+D,Dictate\n").as_deref(),
            Some("F13 or Meta+Shift+D")
        );
    }

    #[test]
    fn snippets_are_found_by_their_first_line() {
        for (desktop, expected) in [
            (Desktop::Niri, "Mod+D"),
            (Desktop::Hyprland, "SUPER + D"),
            (Desktop::Sway, "$mod+d"),
            (Desktop::River, "Super+D"),
        ] {
            let mut lines = desktop.snippet().lines();
            let first = lines.next().unwrap();
            assert!(starts_dictation(first), "{first}");
            assert_eq!(keys(desktop, first).as_deref(), Some(expected));
            for line in lines {
                assert!(!starts_dictation(line), "{line}");
                assert_eq!(keys(desktop, line).as_deref(), Some(expected));
            }
        }
    }
}
