//! What niri has focused: the window, so insertion can pick a way the app reads and the paste
//! chord can follow terminal conventions, and the output, so the pill appears where the user is
//! looking. KWin tells the output too.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::OnceLock;
use std::time::Duration;

/// The window niri has focused, as far as insertion cares.
pub(crate) struct Window {
    pub app_id: Option<String>,
    /// Whether the window belongs to an X11 app behind Xwayland. Such apps read keys with the
    /// seat's own keymap and ignore the one a virtual keyboard uploads.
    pub x11: bool,
}

/// niri's focused window, or `None` off niri or when nothing has focus.
pub(crate) fn focused_window() -> Option<Window> {
    let reply = niri("\"FocusedWindow\"")?;
    let window = reply.pointer("/Ok/FocusedWindow")?;
    if window.is_null() {
        return None;
    }
    let app_id = window
        .get("app_id")
        .and_then(|id| id.as_str())
        .map(str::to_owned);
    let x11 = window
        .get("pid")
        .and_then(|pid| pid.as_u64())
        .is_some_and(xwayland);
    Some(Window { app_id, x11 })
}

/// The app id of niri's focused window, or `None` off niri or when nothing has focus.
pub fn focused_app_id() -> Option<String> {
    focused_window()?.app_id
}

/// Whether `pid` is Xwayland or xwayland-satellite, the process niri reports for every X11
/// window.
fn xwayland(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .is_ok_and(|name| name.to_lowercase().contains("xwayland"))
}

/// The first running input method daemon that would already hold the seat's input method, by
/// process name. Binding a second one on niri 26.04 detaches the first, so this is checked
/// before ever asking for the protocol.
pub fn other_input_method() -> Option<String> {
    const KNOWN: [&str; 5] = ["fcitx5", "fcitx", "ibus-daemon", "kime", "maliit-server"];
    let processes = std::fs::read_dir("/proc").ok()?;
    processes.flatten().find_map(|entry| {
        let name = std::fs::read_to_string(entry.path().join("comm")).ok()?;
        let name = name.trim();
        KNOWN.contains(&name).then(|| name.to_owned())
    })
}

/// The connector name of the focused output on niri or KWin, or `None` elsewhere.
pub fn focused_output() -> Option<String> {
    match std::env::var_os("NIRI_SOCKET") {
        Some(_) => niri_output(),
        None => kwin_output(),
    }
}

fn niri_output() -> Option<String> {
    let reply = niri("\"FocusedOutput\"")?;
    reply
        .pointer("/Ok/FocusedOutput/name")?
        .as_str()
        .map(str::to_owned)
}

/// KWin's active output, the one with the active window or else the pointer, by connector name
/// as niri gives it.
fn kwin_output() -> Option<String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !desktop.split(':').any(|name| name.eq_ignore_ascii_case("KDE")) {
        return None;
    }
    static BUS: OnceLock<Option<zbus::blocking::Connection>> = OnceLock::new();
    let bus = BUS
        .get_or_init(|| zbus::blocking::Connection::session().ok())
        .as_ref()?;
    let reply = bus
        .call_method(
            Some("org.kde.KWin"),
            "/KWin",
            Some("org.kde.KWin"),
            "activeOutputName",
            &(),
        )
        .ok()?;
    let name: String = reply.body().deserialize().ok()?;
    (!name.is_empty()).then_some(name)
}

/// Sends one request to niri's IPC socket and reads its reply.
fn niri(request: &str) -> Option<serde_json::Value> {
    let path = std::env::var_os("NIRI_SOCKET")?;
    let stream = UnixStream::connect(path).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    (&stream).write_all(request.as_bytes()).ok()?;
    (&stream).write_all(b"\n").ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}
