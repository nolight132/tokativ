//! Text insertion on macOS and Windows. Text is typed as keyboard events, or offered on the
//! clipboard and pasted with the platform's paste chord. Neither platform lets another program
//! show unconfirmed text at the cursor, so drafts are dropped, and Escape and Enter are taken by
//! the daemon's global hotkeys rather than here.

use std::sync::mpsc::{Receiver, SyncSender};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard as _, Settings};

use crate::{Backend, KeyHandler, Method, Request, Shared, Status};

/// How long the app gets to read the clipboard after the paste chord before the old contents
/// go back.
const PASTE_SETTLE: Duration = Duration::from_millis(150);

/// Serves requests until the daemon drops its handle. macOS asks for the Accessibility
/// permission the first time keys are sent, and events are dropped until it is granted.
pub(crate) fn run(
    preferred: Backend,
    _terminals: Vec<String>,
    requests: Receiver<Request>,
    ready: SyncSender<Result<Status>>,
    shared: Shared,
    _keys: KeyHandler,
) {
    let mut keyboard = match Enigo::new(&Settings::default()) {
        Ok(keyboard) => keyboard,
        Err(error) => {
            ready
                .send(Err(anyhow!("cannot send key events: {error}")))
                .ok();
            return;
        }
    };
    let status = choose(preferred);
    let method = status.method;
    shared.set_status(status.clone());
    ready.send(Ok(status)).ok();
    for request in requests {
        if let Request::Insert { text, .. } = request
            && let Err(error) = insert(&mut keyboard, method, &text)
        {
            log::error!("insert: {error:#}");
        }
    }
}

/// The method for a configured backend. Typing is the default since it leaves the clipboard
/// alone. The input method backend does not exist here and falls back to typing.
fn choose(preferred: Backend) -> Status {
    match preferred {
        Backend::Auto | Backend::VirtualKeyboard => Status {
            method: Method::VirtualKeyboard,
            note: None,
        },
        Backend::InputMethod => Status {
            method: Method::VirtualKeyboard,
            note: Some("this system has no input method protocol".into()),
        },
        Backend::Clipboard => Status {
            method: Method::Clipboard,
            note: None,
        },
    }
}

/// Inserts `text` with `method`, pasting instead when typing fails.
fn insert(keyboard: &mut Enigo, method: Method, text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    match method {
        Method::Clipboard => paste(keyboard, text),
        Method::VirtualKeyboard | Method::InputMethod => {
            type_text(keyboard, text).or_else(|error| {
                log::warn!("insert: typing failed, pasting instead: {error:#}");
                paste(keyboard, text)
            })
        }
    }
}

/// Types `text` as key events carrying it as Unicode, which works whatever the keyboard layout.
#[cfg(not(target_os = "macos"))]
fn type_text(keyboard: &mut Enigo, text: &str) -> Result<()> {
    keyboard.text(text).context("typing")
}

/// Types `text` as key events carrying it as Unicode. enigo sends only the key-down half of each
/// event, which many macOS apps drop, so this sends the key-up too.
#[cfg(target_os = "macos")]
fn type_text(_keyboard: &mut Enigo, text: &str) -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    /// The most UTF-16 units one event carries. macOS cuts longer strings off.
    const CHUNK: usize = 20;
    /// A pause between chunks, so a busy app does not drop or reorder them.
    const BETWEEN: Duration = Duration::from_millis(2);

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|()| anyhow!("cannot create a keyboard event source"))?;
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut start = 0;
    while start < units.len() {
        let mut end = (start + CHUNK).min(units.len());
        // A surrogate pair split across two events would arrive as two broken characters.
        if end < units.len() && (0xDC00..0xE000).contains(&units[end]) {
            end -= 1;
        }
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source.clone(), 0, down)
                .map_err(|()| anyhow!("cannot create a keyboard event"))?;
            event.set_string_from_utf16_unchecked(&units[start..end]);
            event.set_flags(CGEventFlags::CGEventFlagNull);
            event.post(CGEventTapLocation::HID);
        }
        start = end;
        thread::sleep(BETWEEN);
    }
    Ok(())
}

/// Offers `text` on the clipboard, presses the paste chord and then puts the old text back.
/// Clipboard contents that are not text are lost.
fn paste(keyboard: &mut Enigo, text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().context("opening the clipboard")?;
    let previous = clipboard.get_text().ok();
    clipboard
        .set_text(text.to_owned())
        .context("setting the clipboard")?;
    let modifier = match cfg!(target_os = "macos") {
        true => Key::Meta,
        false => Key::Control,
    };
    keyboard
        .key(modifier, Direction::Press)
        .and_then(|()| keyboard.key(Key::Unicode('v'), Direction::Click))
        .and_then(|()| keyboard.key(modifier, Direction::Release))
        .context("pressing the paste chord")?;
    thread::sleep(PASTE_SETTLE);
    let restored = match previous {
        Some(previous) => clipboard.set_text(previous),
        None => clipboard.clear(),
    };
    if let Err(error) = restored {
        log::warn!("insert: cannot restore the clipboard: {error}");
    }
    Ok(())
}

/// No compositor reports the focused app here.
pub fn focused_app_id() -> Option<String> {
    None
}

/// No compositor reports the focused output here. The pill goes on the primary display.
pub fn focused_output() -> Option<String> {
    None
}

/// No input method daemon competes for a Wayland seat here.
pub fn other_input_method() -> Option<String> {
    None
}
