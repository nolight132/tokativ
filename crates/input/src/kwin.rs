//! KDE Plasma. KWin offers its input method protocol only to the one program it starts itself,
//! the one chosen under System Settings → Keyboard → Virtual Keyboard, and has no virtual
//! keyboard protocol at all. So `tokativ input-method` is that program: KWin starts it and keeps
//! it running, and the daemon's insertion worker drives it over a socket of its own. The daemon
//! can restart, crash or update without KWin ever noticing.
//!
//! Text the input method cannot place, in X11 apps and in apps without text input support, is
//! pasted with Ctrl+V pressed through KWin's fake input.

use std::fs;
use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use rustix::event::{PollFd, PollFlags, Timespec};
use serde::{Deserialize, Serialize};
use tokativ_config::APP_ID;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::input_method::zv1::client::zwp_input_method_context_v1::{
    self, ZwpInputMethodContextV1,
};
use wayland_protocols::wp::input_method::zv1::client::zwp_input_method_v1::{
    self, ZwpInputMethodV1,
};

use crate::Key;

/// The input method protocol rejects longer strings.
const MAX_STRING: usize = 4000;
/// Evdev codes the session answers to.
const KEY_ESC: u32 = 1;
const KEY_ENTER: u32 = 28;
const KEY_KPENTER: u32 = 96;
/// Shift, Ctrl, Alt and Super in KWin's modifier masks.
const SHORTCUT_MODS: u32 = 1 | 4 | 8 | 64;
const PRESSED: u32 = 1;

/// What the worker asks of the input method.
#[derive(Serialize, Deserialize)]
pub(crate) enum Order {
    /// A session started: take Escape and Enter whenever a text field is focused.
    Begin,
    Preedit(String),
    /// Answered with [`Report::Inserted`].
    Insert {
        text: String,
        preedit: String,
    },
    End,
}

/// What the input method tells the worker.
#[derive(Serialize, Deserialize)]
pub(crate) enum Report {
    /// Whether a text field is focused, so text can go in through the input method.
    Active(bool),
    /// The text left of the cursor, when the app reports it.
    Before(Option<String>),
    Key(SessionKey),
    /// Whether the last [`Order::Insert`] reached a text field.
    Inserted(bool),
}

#[derive(Serialize, Deserialize)]
pub(crate) enum SessionKey {
    Escape,
    Enter,
}

impl From<SessionKey> for Key {
    fn from(key: SessionKey) -> Self {
        match key {
            SessionKey::Escape => Key::Escape,
            SessionKey::Enter => Key::Enter,
        }
    }
}

/// The socket the input method listens on. It is not the sandbox's own, since KWin starts the
/// input method outside any sandbox.
fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("{APP_ID}-input-method.sock"))
}

/// The worker's end of the socket.
pub(crate) struct Bridge {
    stream: UnixStream,
    buffer: Vec<u8>,
    /// Whether a text field is focused, as last reported.
    pub active: bool,
}

impl Bridge {
    /// Connects to the running input method, or `None` when KWin has not started it.
    pub fn connect() -> Option<Self> {
        let stream = UnixStream::connect(socket_path()).ok()?;
        Some(Self {
            stream,
            buffer: Vec::new(),
            active: false,
        })
    }

    pub fn send(&mut self, order: &Order) -> Result<()> {
        let mut line = serde_json::to_vec(order)?;
        line.push(b'\n');
        self.stream
            .write_all(&line)
            .context("writing to the input method")
    }

    /// Reads what the input method has sent, waiting up to `timeout` for something to arrive.
    /// Fails once the input method is gone.
    pub fn receive(&mut self, timeout: Duration) -> Result<Vec<Report>> {
        let readable = {
            let mut fds = [PollFd::new(&self.stream, PollFlags::IN)];
            let timeout = Timespec::try_from(timeout).unwrap_or_default();
            rustix::event::poll(&mut fds, Some(&timeout)).context("polling the input method")? > 0
        };
        if readable {
            let mut chunk = [0; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => bail!("the input method stopped"),
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error).context("reading from the input method"),
            }
        }
        let reports = lines(&mut self.buffer)
            .filter_map(|line| serde_json::from_slice::<Report>(&line).ok())
            .collect::<Vec<_>>();
        for report in &reports {
            if let Report::Active(active) = report {
                self.active = *active;
            }
        }
        Ok(reports)
    }
}

/// Takes every complete line out of `buffer`.
fn lines(buffer: &mut Vec<u8>) -> impl Iterator<Item = Vec<u8>> {
    let end = buffer
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    let complete: Vec<u8> = buffer.drain(..end).collect();
    complete
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>()
        .into_iter()
}

/// Why KWin has not started Tokativ's input method, for the settings to show, or `None` when it
/// is chosen in System Settings and only has to come up.
pub(crate) fn absence() -> Option<String> {
    let chosen = chosen()?;
    if chosen == entry_path() {
        return None;
    }
    let name = chosen
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(format!(
        "System Settings uses {name} as the virtual keyboard"
    ))
}

/// The desktop entry KWin is set to start as the input method, if any.
fn chosen() -> Option<PathBuf> {
    let output = Command::new("kreadconfig6")
        .args([
            "--file",
            "kwinrc",
            "--group",
            "Wayland",
            "--key",
            "InputMethod",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let chosen = String::from_utf8(output.stdout).ok()?;
    let chosen = chosen.trim();
    (!chosen.is_empty()).then(|| PathBuf::from(chosen))
}

/// Where Tokativ's entry for System Settings' list of virtual keyboards lives.
fn entry_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join("applications")
        .join(format!("{APP_ID}-input-method.desktop"))
}

/// Keeps Tokativ's entry in System Settings' list of virtual keyboards pointing at the running
/// binary, restarts the input method when that path changes, and chooses it once when no other input method is chosen. A marker remembers that it
/// was, so a later choice of none or of another one in System Settings stays.
pub(crate) fn register() -> Result<()> {
    if tokativ_config::sandbox().is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("finding the running binary")?;
    let entry = entry_path();
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Tokativ\nComment=Dictation at the cursor\n\
         Icon={APP_ID}\nExec=\"{}\" input-method\nTerminal=false\nNoDisplay=true\n\
         X-KDE-Wayland-VirtualKeyboard=true\n",
        exe.display()
    );
    if fs::read_to_string(&entry).ok().as_deref() != Some(text.as_str()) {
        fs::create_dir_all(entry.parent().context("the entry path has no parent")?)?;
        fs::write(&entry, &text).with_context(|| format!("writing {}", entry.display()))?;
        // KWin keeps running the old command, or none when that binary is gone, as after a test
        // build in /tmp and a reboot. It reads the entry again only when the setting changes.
        if chosen().as_deref() == Some(entry.as_path()) {
            choose(Path::new(""))?;
            choose(&entry)?;
            log::info!("insert: asked KWin to restart Tokativ's input method from the new binary");
        }
    }

    let marker = dirs::state_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_ID)
        .join("chose-kwin-input-method");
    if marker.exists() || chosen().is_some() {
        return Ok(());
    }
    choose(&entry)?;
    log::info!("insert: chose Tokativ as KWin's input method");
    fs::create_dir_all(marker.parent().context("the marker path has no parent")?)?;
    fs::write(&marker, b"").with_context(|| format!("writing {}", marker.display()))
}

/// Sets `entry` as KWin's input method, or none for an empty path. `--notify` makes KWin start
/// it right away.
fn choose(entry: &Path) -> Result<()> {
    let status = Command::new("kwriteconfig6")
        .args([
            "--file",
            "kwinrc",
            "--group",
            "Wayland",
            "--key",
            "InputMethod",
            "--notify",
        ])
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .context("running kwriteconfig6")?;
    if !status.success() {
        bail!("kwriteconfig6 failed with {status}");
    }
    Ok(())
}

/// Runs as KWin's input method until KWin stops it. KWin hands over the connection in
/// `WAYLAND_SOCKET`, which is the only one that offers the input method protocol.
pub fn serve() -> Result<()> {
    let connection = Connection::connect_to_env().context("connecting to KWin")?;
    let (globals, mut queue) =
        registry_queue_init::<Server>(&connection).context("listing Wayland globals")?;
    let handle = queue.handle();
    let _input_method: ZwpInputMethodV1 = globals.bind(&handle, 1..=1, ()).context(
        "there is no input method protocol, so this is not KWin starting its input method",
    )?;

    let path = socket_path();
    match fs::remove_file(&path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(error).with_context(|| format!("removing {}", path.display()));
        }
        _ => {}
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("listening on {}", path.display()))?;
    listener.set_nonblocking(true)?;
    log::info!("input method: listening on {}", path.display());

    let mut server = Server {
        handle,
        context: None,
        serial: 0,
        before: None,
        grab: None,
        depressed: 0,
        swallowed: Vec::new(),
        session: false,
        preedit: false,
        worker: None,
    };
    loop {
        queue.dispatch_pending(&mut server)?;
        connection.flush()?;
        // Events already queued are dispatched first, then the loop sleeps until KWin, a new
        // worker or the current one has something.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let (wayland, incoming, worker) = {
            let wayland = guard.connection_fd();
            let mut fds = vec![
                PollFd::new(&wayland, PollFlags::IN),
                PollFd::new(&listener, PollFlags::IN),
            ];
            if let Some(worker) = &server.worker {
                fds.push(PollFd::new(&worker.stream, PollFlags::IN));
            }
            match rustix::event::poll(&mut fds, None) {
                Err(rustix::io::Errno::INTR) => continue,
                result => result.context("polling")?,
            };
            let ready = |index: usize| fds.get(index).is_some_and(|fd| !fd.revents().is_empty());
            (ready(0), ready(1), ready(2))
        };
        if wayland {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(error))
                    if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).context("reading from KWin"),
            }
        } else {
            drop(guard);
        }
        queue.dispatch_pending(&mut server)?;
        if incoming {
            server.accept(&listener);
        }
        if worker {
            server.read();
        }
        server.flush();
    }
}

/// The input method's state, owned by the loop in [`serve`].
struct Server {
    handle: QueueHandle<Server>,
    /// The focused text field, while there is one.
    context: Option<ZwpInputMethodContextV1>,
    /// The latest state serial KWin sent, which every request about the text carries.
    serial: u32,
    before: Option<String>,
    /// The keyboard, taken while a session runs and a text field is focused.
    grab: Option<WlKeyboard>,
    depressed: u32,
    /// Escape or Enter presses the session took, whose releases the app must not see either.
    swallowed: Vec<u32>,
    session: bool,
    /// Whether a draft is showing, so ending the session clears it.
    preedit: bool,
    worker: Option<Worker>,
}

/// The daemon's insertion worker, connected to the input method.
struct Worker {
    stream: UnixStream,
    buffer: Vec<u8>,
    outbox: Vec<Report>,
}

impl Server {
    /// Takes a new worker. A daemon that restarted connects again, so the new one replaces the
    /// old, along with any session the old one left behind.
    fn accept(&mut self, listener: &UnixListener) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => return,
            Err(error) => {
                log::warn!("input method: accepting the daemon: {error}");
                return;
            }
        };
        if let Err(error) = stream.set_write_timeout(Some(Duration::from_secs(1))) {
            log::warn!("input method: {error}");
            return;
        }
        self.end();
        self.worker = Some(Worker {
            stream,
            buffer: Vec::new(),
            outbox: Vec::new(),
        });
        self.report(Report::Active(self.context.is_some()));
        self.report(Report::Before(self.before.clone()));
    }

    fn read(&mut self) {
        let Some(worker) = &mut self.worker else {
            return;
        };
        // The socket polled readable, so one read does not block. Whatever does not fit is
        // read on the next round.
        let mut chunk = [0; 4096];
        let gone = match worker.stream.read(&mut chunk) {
            Ok(0) => true,
            Ok(read) => {
                worker.buffer.extend_from_slice(&chunk[..read]);
                false
            }
            Err(error) => error.kind() != ErrorKind::Interrupted,
        };
        let orders = lines(&mut worker.buffer)
            .filter_map(|line| serde_json::from_slice::<Order>(&line).ok())
            .collect::<Vec<_>>();
        for order in orders {
            self.order(order);
        }
        if gone {
            self.end();
            self.worker = None;
        }
    }

    fn order(&mut self, order: Order) {
        match order {
            Order::Begin => {
                self.session = true;
                self.swallowed.clear();
                self.grab_keys();
                // The daemon forgets the text before the cursor at every start.
                self.report(Report::Before(self.before.clone()));
            }
            Order::Preedit(text) => {
                if let Some(context) = &self.context {
                    let shown = chunks(&text).next().unwrap_or_default();
                    context.preedit_cursor(shown.len() as i32);
                    context.preedit_string(self.serial, shown.into(), String::new());
                    self.preedit = !shown.is_empty();
                }
            }
            Order::Insert { text, preedit } => {
                let inserted = match &self.context {
                    Some(context) => {
                        for chunk in chunks(&text) {
                            context.commit_string(self.serial, chunk.into());
                        }
                        // Committing clears the draft, so an empty one needs no request.
                        let shown = chunks(&preedit).next().unwrap_or_default();
                        if !shown.is_empty() {
                            context.preedit_cursor(shown.len() as i32);
                            context.preedit_string(self.serial, shown.into(), String::new());
                        }
                        self.preedit = !shown.is_empty();
                        true
                    }
                    None => false,
                };
                self.report(Report::Inserted(inserted));
            }
            Order::End => self.end(),
        }
    }

    fn end(&mut self) {
        self.session = false;
        self.release_keys();
        if let Some(context) = &self.context
            && self.preedit
        {
            context.preedit_string(self.serial, String::new(), String::new());
        }
        self.preedit = false;
    }

    /// Takes the keyboard for the session, so Escape and Enter reach it. Every other key goes
    /// back to the focused app through the context. KWin hands keys to the input method only
    /// while a text field is focused, so this waits for one.
    fn grab_keys(&mut self) {
        let Some(context) = &self.context else {
            return;
        };
        if !self.session || self.grab.is_some() {
            return;
        }
        self.depressed = 0;
        self.grab = Some(context.grab_keyboard(&self.handle, ()));
    }

    fn release_keys(&mut self) {
        if let Some(grab) = self.grab.take() {
            grab.release();
        }
    }

    /// Takes Escape and Enter, when no shortcut modifier is held, and passes every other key on.
    fn key(&mut self, serial: u32, time: u32, key: u32, state: u32) {
        let plain = self.depressed & SHORTCUT_MODS == 0;
        let ours = match key {
            KEY_ESC => Some(SessionKey::Escape),
            KEY_ENTER | KEY_KPENTER => Some(SessionKey::Enter),
            _ => None,
        };
        match ours {
            Some(ours) if state == PRESSED && plain => {
                self.swallowed.push(key);
                self.report(Report::Key(ours));
                return;
            }
            Some(_) if state != PRESSED && self.swallowed.contains(&key) => {
                // A held key repeats, and only its release ends the swallowing.
                if state == 0 {
                    self.swallowed.retain(|swallowed| *swallowed != key);
                }
                return;
            }
            _ => {}
        }
        if let Some(context) = &self.context {
            context.key(serial, time, key, state);
        }
    }

    fn report(&mut self, report: Report) {
        if let Some(worker) = &mut self.worker {
            worker.outbox.push(report);
        }
    }

    /// Sends what is waiting for the worker. A worker that stops reading is dropped.
    fn flush(&mut self) {
        let Some(worker) = &mut self.worker else {
            return;
        };
        let mut bytes = Vec::new();
        for report in worker.outbox.drain(..) {
            if let Ok(line) = serde_json::to_vec(&report) {
                bytes.extend_from_slice(&line);
                bytes.push(b'\n');
            }
        }
        if bytes.is_empty() {
            return;
        }
        if let Err(error) = worker.stream.write_all(&bytes) {
            log::warn!("input method: the daemon stopped listening: {error}");
            self.end();
            self.worker = None;
        }
    }
}

/// Splits `text` into pieces the protocol takes, on character boundaries.
fn chunks(text: &str) -> impl Iterator<Item = &str> {
    crate::wayland::chunks(text, MAX_STRING)
}

impl Dispatch<ZwpInputMethodV1, ()> for Server {
    fn event(
        server: &mut Self,
        _: &ZwpInputMethodV1,
        event: zwp_input_method_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v1::Event::Activate { id } => {
                server.release_keys();
                if let Some(old) = server.context.replace(id) {
                    old.destroy();
                }
                server.serial = 0;
                server.preedit = false;
                server.before = None;
                server.report(Report::Active(true));
                server.grab_keys();
            }
            zwp_input_method_v1::Event::Deactivate { context } => {
                if server.context.as_ref() != Some(&context) {
                    context.destroy();
                    return;
                }
                server.release_keys();
                context.destroy();
                server.context = None;
                server.preedit = false;
                server.before = None;
                server.report(Report::Active(false));
                server.report(Report::Before(None));
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Server, ZwpInputMethodV1, [
        zwp_input_method_v1::EVT_ACTIVATE_OPCODE => (ZwpInputMethodContextV1, ()),
    ]);
}

impl Dispatch<ZwpInputMethodContextV1, ()> for Server {
    fn event(
        server: &mut Self,
        context: &ZwpInputMethodContextV1,
        event: zwp_input_method_context_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if server.context.as_ref() != Some(context) {
            return;
        }
        match event {
            zwp_input_method_context_v1::Event::SurroundingText { text, cursor, .. } => {
                // KWin repeats the text after every state the app commits, mostly unchanged.
                let cursor = (cursor as usize).min(text.len());
                let before = text.get(..cursor).map(str::to_owned);
                if before != server.before {
                    server.before = before;
                    server.report(Report::Before(server.before.clone()));
                }
            }
            zwp_input_method_context_v1::Event::CommitState { serial } => server.serial = serial,
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Server {
    fn event(
        server: &mut Self,
        keyboard: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if server.grab.as_ref() != Some(keyboard) {
            return;
        }
        match event {
            // While the keyboard is grabbed KWin stops passing modifiers to the app and leaves
            // that to the input method, so Shift and Ctrl would otherwise never reach it.
            wl_keyboard::Event::Modifiers {
                serial,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                server.depressed = mods_depressed;
                if let Some(context) = &server.context {
                    context.modifiers(serial, mods_depressed, mods_latched, mods_locked, group);
                }
            }
            wl_keyboard::Event::Key {
                serial,
                time,
                key,
                state,
            } => {
                let state = match state {
                    WEnum::Value(state) => state.into(),
                    WEnum::Unknown(state) => state,
                };
                server.key(serial, time, key, state);
            }
            // The keymap is KWin's own, and keys go back to it as they came.
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Server {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_keep_the_unfinished_tail() {
        let mut buffer = b"\"End\"\n{\"Preedit\":\"hi\"}\n\"Beg".to_vec();
        let complete: Vec<Vec<u8>> = lines(&mut buffer).collect();
        assert_eq!(complete.len(), 2);
        assert!(matches!(
            serde_json::from_slice::<Order>(&complete[1]),
            Ok(Order::Preedit(text)) if text == "hi"
        ));
        assert_eq!(buffer, b"\"Beg");
    }
}
