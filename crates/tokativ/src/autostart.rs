//! Launch at login: an XDG autostart entry on Linux, a LaunchAgent on macOS, and a value under
//! the user's `Run` key on Windows.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use tokativ_config::{APP_ID, autostart_dir};

/// Writes or removes the autostart entry so it matches `enabled`.
pub fn apply(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe().context("finding the running binary")?;
    platform::apply(enabled, &launcher(exe))
}

/// The path the entry should start. A binary in the Nix store is replaced by the link to it on
/// `PATH`, because the store path stops existing or goes stale after the next upgrade.
fn launcher(exe: PathBuf) -> PathBuf {
    let package: PathBuf = exe.components().take(4).collect();
    if !exe.starts_with("/nix/store") || package.components().count() < 4 {
        return exe;
    }
    let Some(path) = std::env::var_os("PATH") else {
        return exe;
    };
    std::env::split_paths(&path)
        .map(|folder| folder.join("tokativ"))
        .find(|link| {
            !link.starts_with("/nix/store")
                && fs::canonicalize(link).is_ok_and(|target| target.starts_with(&package))
        })
        .unwrap_or(exe)
}

/// Writes `text` to `path`, or removes the file when `text` is `None`.
fn write_or_remove(path: &Path, text: Option<String>) -> Result<()> {
    let Some(text) = text else {
        return match fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        };
    };
    fs::create_dir_all(path.parent().context("autostart path has no parent")?)?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub fn apply(enabled: bool, exe: &Path) -> Result<()> {
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=Tokativ\nComment=Local dictation\n\
             Exec={} daemon\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        let entry: PathBuf = autostart_dir().join(format!("{APP_ID}.desktop"));
        write_or_remove(&entry, enabled.then_some(text))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    /// launchd reads the folder at login, so the agent starts from the next one on.
    pub fn apply(enabled: bool, exe: &Path) -> Result<()> {
        let program = escape(&exe.display().to_string());
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\
             \t<key>Label</key>\n\t<string>{APP_ID}</string>\n\
             \t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>{program}</string>\n\t\t<string>daemon</string>\n\t</array>\n\
             \t<key>RunAtLoad</key>\n\t<true/>\n\
             \t<key>ProcessType</key>\n\t<string>Interactive</string>\n\
             </dict>\n</plist>\n"
        );
        let agent: PathBuf = autostart_dir().join(format!("{APP_ID}.plist"));
        write_or_remove(&agent, enabled.then_some(text))
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
}

#[cfg(windows)]
mod platform {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};

    use super::*;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Tokativ";

    /// A sandbox writes the command to a file in its own folder instead, so trying the app
    /// never changes what the real install starts at login.
    pub fn apply(enabled: bool, exe: &Path) -> Result<()> {
        let command = format!("\"{}\" daemon", exe.display());
        if tokativ_config::sandbox().is_some() {
            let entry: PathBuf = autostart_dir().join(format!("{APP_ID}.txt"));
            return write_or_remove(&entry, enabled.then_some(command));
        }
        let run = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN, KEY_SET_VALUE)
            .context("opening the Run key")?;
        match enabled {
            true => run
                .set_value(VALUE, &command)
                .context("adding Tokativ to the Run key"),
            false => match run.delete_value(VALUE) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(error).context("removing Tokativ from the Run key")
                }
                _ => Ok(()),
            },
        }
    }
}
