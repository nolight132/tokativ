//! The insertion worker: one Wayland connection that drives the input method and the virtual
//! keyboard on behalf of [`crate::Inserter`]. On KWin it drives Tokativ's own input method
//! through [`Bridge`] instead and presses keys through KWin's fake input.

use std::collections::VecDeque;
use std::io::{ErrorKind, Write as _};
use std::os::fd::AsFd as _;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use rustix::event::{PollFd, PollFlags, Timespec};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_keyboard::{KeyState, KeymapFormat};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_keyboard_grab_v2::{
    self, ZwpInputMethodKeyboardGrabV2,
};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{
    self, ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

use crate::keymap::{self, KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V, MOD_CTRL, MOD_SHIFT};
use crate::kwin::{self, Bridge, Order, Report};
use crate::{Backend, Key, KeyHandler, Method, Request, Shared, Status, clipboard, focus};

/// The input method protocol rejects longer strings.
const MAX_STRING: usize = 4000;
/// How long the first insertion of a session waits for the focused field to activate the input
/// method before typing instead. Apps answer the compositor's enter late, often near a second.
const ACTIVATION: Duration = Duration::from_millis(1500);
/// The gap between virtual key events, as wtype leaves it. Some clients drop keys sent faster.
const KEY_GAP: Duration = Duration::from_millis(2);
/// The most distinct characters one virtual keymap carries before a fresh one is uploaded.
const KEYMAP_CHARS: usize = 64;
/// How long the app gets to read the clipboard after the paste chord before it is restored.
const PASTE_SETTLE: Duration = Duration::from_millis(400);
const TICK: Duration = Duration::from_millis(10);
/// Evdev codes the keyboard grab answers to.
const KEY_ESC: u32 = 1;
const KEY_ENTER: u32 = 28;
const KEY_KPENTER: u32 = 96;
/// Shift, Ctrl, Alt and Super in the masks a keymap built from `complete` uses.
const SHORTCUT_MODS: u32 = 1 | 4 | 8 | 64;
/// How long a forwarded key may take to come back through the grab before it counts as a new
/// press. Compositors that route virtual keys through the seat echo them within a millisecond or
/// two, and no one presses a key twice this fast.
const ECHO: Duration = Duration::from_millis(50);
/// How often the worker looks for KWin's input method while it is not connected, since KWin
/// starts it whenever it is chosen in System Settings.
const RETRY: Duration = Duration::from_secs(1);
/// How long the input method gets to say whether an insertion reached a text field.
const ANSWER: Duration = Duration::from_secs(1);

/// The protocol state the dispatch callbacks write into.
struct State {
    pending_active: bool,
    active: bool,
    /// How many `done` events the current input method has seen, which is the serial every
    /// commit must carry.
    serial: u32,
    unavailable: bool,
    before: Arc<Mutex<Option<String>>>,
    keys: KeyHandler,
    /// Passes every key the session does not answer to on to the focused app, with the seat's
    /// own keymap and modifiers. It exists only while the keyboard is grabbed.
    forward: Option<ZwpVirtualKeyboardV1>,
    /// Depressed modifiers as the grab last saw them.
    depressed: u32,
    /// Escape or Enter presses the session took, whose releases the app must not see either.
    swallowed: Vec<u32>,
    /// Recently forwarded keys and when, so one that comes back through the grab is not
    /// forwarded again.
    echoes: VecDeque<(u32, bool, Instant)>,
}

struct Worker {
    connection: Connection,
    queue: EventQueue<State>,
    handle: QueueHandle<State>,
    state: State,
    seat: WlSeat,
    input_methods: Option<ZwpInputMethodManagerV2>,
    keyboards: Option<ZwpVirtualKeyboardManagerV1>,
    method: Method,
    terminals: Vec<String>,
    input_method: Option<ZwpInputMethodV2>,
    grab: Option<ZwpInputMethodKeyboardGrabV2>,
    /// Whether this session already waited out [`ACTIVATION`] once.
    waited: bool,
    started: Instant,
    shared: Shared,
    /// Whether this is KWin and the input method is wanted, so the worker keeps looking for
    /// Tokativ's own.
    kwin: bool,
    bridge: Option<Bridge>,
    retried: Instant,
    /// KWin's way to press keys, for the paste chord where there is no virtual keyboard.
    fake_input: Option<OrgKdeKwinFakeInput>,
    session: bool,
}

pub(crate) fn run(
    preferred: Backend,
    terminals: Vec<String>,
    requests: Receiver<Request>,
    ready: SyncSender<Result<Status>>,
    shared: Shared,
    keys: KeyHandler,
) {
    let mut worker = match Worker::connect(preferred, terminals, shared, keys) {
        Ok((worker, status)) => {
            ready.send(Ok(status)).ok();
            worker
        }
        Err(error) => {
            ready.send(Err(error)).ok();
            return;
        }
    };
    loop {
        // While the keyboard is grabbed every key passes through here, so the worker waits on
        // the compositor rather than on requests, to forward keys without delay.
        let grabbed = worker.grab.is_some();
        let request = match grabbed {
            true => match requests.try_recv() {
                Ok(request) => Some(request),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            },
            false => match requests.recv_timeout(TICK) {
                Ok(request) => Some(request),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        let handled = match (request, grabbed) {
            (Some(request), _) => worker.handle(request),
            (None, true) => worker.pump(TICK),
            (None, false) => worker.pump(Duration::ZERO),
        };
        if let Err(error) = handled {
            log::error!("insert: {error:#}");
            if worker.connection.protocol_error().is_some() {
                return;
            }
        }
    }
}

impl Worker {
    fn connect(
        preferred: Backend,
        terminals: Vec<String>,
        shared: Shared,
        keys: KeyHandler,
    ) -> Result<(Self, Status)> {
        let connection = Connection::connect_to_env().context("connecting to Wayland")?;
        let (globals, queue) =
            registry_queue_init::<State>(&connection).context("listing Wayland globals")?;
        let handle = queue.handle();
        let seat: WlSeat = globals
            .bind(&handle, 1..=7, ())
            .context("the compositor has no seat")?;
        let input_methods = bind::<ZwpInputMethodManagerV2>(&globals, &handle);
        let keyboards = bind::<ZwpVirtualKeyboardManagerV1>(&globals, &handle);
        // Only KWin has fake input, and it is the one way to press keys there.
        let fake_input = globals
            .bind::<OrgKdeKwinFakeInput, State, ()>(&handle, 4..=5, ())
            .ok();
        if let Some(fake_input) = &fake_input {
            fake_input.authenticate("Tokativ".into(), "Pastes dictated text".into());
        }
        let kwin = input_methods.is_none()
            && fake_input.is_some()
            && matches!(preferred, Backend::Auto | Backend::InputMethod);
        if kwin && let Err(error) = kwin::register() {
            log::warn!("insert: cannot offer the input method to KWin: {error:#}");
        }
        let bridge = kwin.then(Bridge::connect).flatten();
        let status = match kwin {
            true => kwin_status(bridge.is_some()),
            false => choose(
                preferred,
                input_methods.is_some(),
                keyboards.is_some(),
                focus::other_input_method(),
            ),
        };
        shared.set_status(status.clone());
        let before = shared.before.clone();
        let worker = Self {
            connection,
            queue,
            handle,
            state: State {
                pending_active: false,
                active: false,
                serial: 0,
                unavailable: false,
                before,
                keys,
                forward: None,
                depressed: 0,
                swallowed: Vec::new(),
                echoes: VecDeque::new(),
            },
            seat,
            input_methods,
            keyboards,
            method: status.method,
            terminals,
            input_method: None,
            grab: None,
            waited: false,
            started: Instant::now(),
            shared,
            kwin,
            bridge,
            retried: Instant::now(),
            fake_input,
            session: false,
        };
        Ok((worker, status))
    }

    fn handle(&mut self, request: Request) -> Result<()> {
        match request {
            Request::Begin => self.begin(),
            Request::Preedit(text) => self.preedit(&text),
            Request::Insert { text, preedit } => self.insert(&text, &preedit),
            Request::End => self.end(),
            Request::Terminals(terminals) => {
                self.terminals = terminals;
                Ok(())
            }
        }
    }

    fn begin(&mut self) -> Result<()> {
        self.end()?;
        self.waited = false;
        self.session = true;
        if self.method != Method::InputMethod {
            return Ok(());
        }
        if self.bridge.is_some() {
            self.order(Order::Begin);
            return Ok(());
        }
        let Some(manager) = &self.input_methods else {
            return Ok(());
        };
        self.state.pending_active = false;
        self.state.active = false;
        self.state.serial = 0;
        self.state.unavailable = false;
        let input_method = manager.get_input_method(&self.seat, &self.handle, ());
        self.input_method = Some(input_method);
        self.grab_keys();
        self.pump(Duration::ZERO)
    }

    /// The input method, when it is bound, usable and active in a text field. The first call in
    /// a session waits for activation, since it usually arrives after the session starts.
    fn active_input_method(&mut self) -> Result<Option<ZwpInputMethodV2>> {
        if self.input_method.is_none() {
            return Ok(None);
        }
        if !self.state.active && !self.waited {
            self.waited = true;
            let deadline = Instant::now() + ACTIVATION;
            while !self.state.active && !self.state.unavailable && Instant::now() < deadline {
                self.pump(TICK)?;
            }
        }
        if self.state.unavailable {
            log::warn!("insert: another input method holds the seat, typing instead");
            if let Some(input_method) = self.input_method.take() {
                input_method.destroy();
            }
            return Ok(None);
        }
        Ok(self
            .state
            .active
            .then(|| self.input_method.clone())
            .flatten())
    }

    fn preedit(&mut self, text: &str) -> Result<()> {
        if self.bridge.is_some() {
            self.order(Order::Preedit(text.into()));
            return Ok(());
        }
        if !self.state.active {
            return Ok(());
        }
        if let Some(input_method) = &self.input_method {
            let shown = chunks(text, MAX_STRING).next().unwrap_or_default();
            input_method.set_preedit_string(shown.into(), -1, -1);
            input_method.commit(self.state.serial);
        }
        self.pump(Duration::ZERO)
    }

    /// Sends `text` to the focused window, which is looked up afresh each time. X11 apps never
    /// activate the input method and misread typed keys, so they always get the paste.
    fn insert(&mut self, text: &str, preedit: &str) -> Result<()> {
        let window = focus::focused_window();
        let x11 = window.as_ref().is_some_and(|window| window.x11);
        let terminal = window
            .and_then(|window| window.app_id)
            .is_some_and(|app| self.terminals.contains(&app));
        if x11 {
            log::debug!("insert: {} bytes into an X11 window", text.len());
        } else if self.bridge_insert(text, preedit) {
            log::debug!("insert: {} bytes through KWin's input method", text.len());
            return Ok(());
        } else if let Some(input_method) = self.active_input_method()? {
            log::debug!("insert: {} bytes through the input method", text.len());
            for chunk in chunks(text, MAX_STRING) {
                input_method.commit_string(chunk.into());
            }
            if let Some(preedit) = chunks(preedit, MAX_STRING).next() {
                input_method.set_preedit_string(preedit.into(), -1, -1);
            }
            input_method.commit(self.state.serial);
            return self.pump(Duration::ZERO);
        } else {
            log::debug!("insert: {} bytes, no active input method", text.len());
        }
        let presses = self.keyboards.is_some() || self.fake_input.is_some();
        // Only a virtual keyboard can type. KWin's fake input presses keys of the user's own
        // keymap, which is enough for the paste chord.
        let paste = self.method == Method::Clipboard || x11 || self.keyboards.is_none();
        match (presses, paste) {
            (true, true) => self.ungrabbed(|worker| worker.paste(text, terminal)),
            (true, false) => self.ungrabbed(|worker| worker.type_text(text)),
            (false, _) => {
                clipboard::offer(text)?;
                log::warn!("insert: no way to send keys, the text is on the clipboard");
                Ok(())
            }
        }
    }

    /// Commits `text` through KWin's input method when a text field is focused, and says
    /// whether it did.
    fn bridge_insert(&mut self, text: &str, preedit: &str) -> bool {
        // Whatever the input method reported meanwhile, such as a field gaining focus, counts.
        self.listen(Duration::ZERO);
        if !self.bridge.as_ref().is_some_and(|bridge| bridge.active) {
            return false;
        }
        self.order(Order::Insert {
            text: text.into(),
            preedit: preedit.into(),
        });
        let deadline = Instant::now() + ANSWER;
        while self.bridge.is_some() && Instant::now() < deadline {
            if let Some(inserted) = self.listen(TICK) {
                return inserted;
            }
        }
        log::warn!("insert: KWin's input method did not answer");
        false
    }

    /// Sends `order` to KWin's input method, and lets it go if it is gone.
    fn order(&mut self, order: Order) {
        let Some(bridge) = &mut self.bridge else {
            return;
        };
        if let Err(error) = bridge.send(&order) {
            self.lose_bridge(error);
        }
    }

    /// Handles what KWin's input method has sent, waiting up to `timeout` for something to
    /// arrive. Returns the answer to an insertion, if one came.
    fn listen(&mut self, timeout: Duration) -> Option<bool> {
        let bridge = self.bridge.as_mut()?;
        let reports = match bridge.receive(timeout) {
            Ok(reports) => reports,
            Err(error) => {
                self.lose_bridge(error);
                return None;
            }
        };
        let mut inserted = None;
        for report in reports {
            match report {
                Report::Active(_) => {}
                Report::Before(before) => self.shared.set_before(before),
                Report::Key(key) => (self.state.keys)(key.into()),
                Report::Inserted(answer) => inserted = Some(answer),
            }
        }
        inserted
    }

    /// Connects to KWin's input method once KWin has started it, at most every [`RETRY`].
    fn look_for_bridge(&mut self) {
        if !self.kwin || self.bridge.is_some() || self.retried.elapsed() < RETRY {
            return;
        }
        self.retried = Instant::now();
        let Some(bridge) = Bridge::connect() else {
            return;
        };
        log::info!("insert: connected to KWin's input method");
        self.bridge = Some(bridge);
        self.method = Method::InputMethod;
        self.shared.set_status(kwin_status(true));
        if self.session {
            self.order(Order::Begin);
        }
    }

    fn lose_bridge(&mut self, error: anyhow::Error) {
        log::warn!("insert: lost KWin's input method: {error:#}");
        self.bridge = None;
        self.retried = Instant::now();
        self.method = Method::Clipboard;
        self.shared.set_status(kwin_status(false));
    }

    fn end(&mut self) -> Result<()> {
        self.session = false;
        self.order(Order::End);
        self.ungrab_keys();
        let Some(input_method) = self.input_method.take() else {
            return Ok(());
        };
        if self.state.active {
            input_method.set_preedit_string(String::new(), 0, 0);
            input_method.commit(self.state.serial);
        }
        input_method.destroy();
        self.state.active = false;
        *self.state.before.lock().expect("before lock") = None;
        self.queue
            .roundtrip(&mut self.state)
            .context("ending the input method")?;
        Ok(())
    }

    /// Takes the keyboard for the session, so Escape and Enter reach it in any app. Every other
    /// key goes on to the focused app through a virtual keyboard. Without the virtual keyboard
    /// protocol nothing could go on, so the keyboard is left alone.
    fn grab_keys(&mut self) {
        let (Some(input_method), Some(keyboards)) = (&self.input_method, &self.keyboards) else {
            return;
        };
        if self.grab.is_some() {
            return;
        }
        self.state.forward = Some(keyboards.create_virtual_keyboard(&self.seat, &self.handle, ()));
        self.state.depressed = 0;
        self.state.swallowed.clear();
        self.state.echoes.clear();
        self.grab = Some(input_method.grab_keyboard(&self.handle, ()));
    }

    fn ungrab_keys(&mut self) {
        if let Some(grab) = self.grab.take() {
            grab.release();
        }
        if let Some(forward) = self.state.forward.take() {
            forward.destroy();
        }
    }

    /// Runs `send` with the keyboard handed back for its duration. Compositors that route virtual
    /// keys through the seat would otherwise deliver the typed text to the grab.
    fn ungrabbed(&mut self, send: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        let grabbed = self.grab.is_some();
        self.ungrab_keys();
        let result = send(self);
        if grabbed {
            self.grab_keys();
        }
        result
    }

    /// Types `text` through a fresh virtual keyboard whose keymap holds exactly the characters
    /// needed. The keyboard is destroyed afterwards so the compositor restores the real keymap.
    fn type_text(&mut self, text: &str) -> Result<()> {
        let keyboard = self.virtual_keyboard()?;
        let characters: Vec<char> = text.chars().collect();
        let mut start = 0;
        while start < characters.len() {
            let mut distinct: Vec<char> = Vec::new();
            let mut end = start;
            while end < characters.len() {
                let c = characters[end];
                if !distinct.contains(&c) {
                    if distinct.len() == KEYMAP_CHARS {
                        break;
                    }
                    distinct.push(c);
                }
                end += 1;
            }
            let chunk: String = characters[start..end].iter().collect();
            let typing = keymap::typing(&chunk);
            self.upload(&keyboard, &typing.text)?;
            for code in typing.codes {
                self.key(&keyboard, code, true)?;
                self.key(&keyboard, code, false)?;
            }
            start = end;
        }
        keyboard.destroy();
        self.queue
            .roundtrip(&mut self.state)
            .context("finishing typing")?;
        Ok(())
    }

    /// Offers `text` on the clipboard, sends the paste chord, and restores the clipboard. A
    /// `terminal` gets Ctrl+Shift+V instead of Ctrl+V.
    fn paste(&mut self, text: &str, terminal: bool) -> Result<()> {
        let previous = clipboard::save();
        clipboard::offer(text)?;
        match self.keyboards.is_some() {
            true => self.virtual_chord(terminal)?,
            false => self.fake_chord(terminal)?,
        }
        thread::sleep(PASTE_SETTLE);
        clipboard::restore(previous);
        Ok(())
    }

    /// Presses the paste chord through KWin's fake input. The keys go through KWin like real
    /// ones, with the user's keymap.
    fn fake_chord(&mut self, terminal: bool) -> Result<()> {
        let fake_input = self
            .fake_input
            .clone()
            .ok_or_else(|| anyhow!("the compositor has no way to press keys"))?;
        let mut chord = vec![KEY_LEFTCTRL];
        if terminal {
            chord.push(KEY_LEFTSHIFT);
        }
        chord.push(KEY_V);
        for (code, pressed) in chord
            .iter()
            .map(|code| (*code, true))
            .chain(chord.iter().rev().map(|code| (*code, false)))
        {
            fake_input.keyboard_key(code, pressed as u32);
            self.queue
                .roundtrip(&mut self.state)
                .context("pressing a key")?;
            thread::sleep(KEY_GAP);
        }
        Ok(())
    }

    /// Sends the paste chord through a virtual keyboard with a keymap of its own.
    fn virtual_chord(&mut self, terminal: bool) -> Result<()> {
        let keyboard = self.virtual_keyboard()?;
        self.upload(&keyboard, &keymap::paste())?;
        let mut mods = MOD_CTRL;
        self.key(&keyboard, KEY_LEFTCTRL, true)?;
        if terminal {
            mods |= MOD_SHIFT;
            self.key(&keyboard, KEY_LEFTSHIFT, true)?;
        }
        keyboard.modifiers(mods, 0, 0, 0);
        self.key(&keyboard, KEY_V, true)?;
        self.key(&keyboard, KEY_V, false)?;
        keyboard.modifiers(0, 0, 0, 0);
        if terminal {
            self.key(&keyboard, KEY_LEFTSHIFT, false)?;
        }
        self.key(&keyboard, KEY_LEFTCTRL, false)?;
        keyboard.destroy();
        self.queue
            .roundtrip(&mut self.state)
            .context("sending the paste chord")?;
        Ok(())
    }

    fn virtual_keyboard(&self) -> Result<ZwpVirtualKeyboardV1> {
        let manager = self
            .keyboards
            .as_ref()
            .ok_or_else(|| anyhow!("the compositor has no virtual keyboard"))?;
        Ok(manager.create_virtual_keyboard(&self.seat, &self.handle, ()))
    }

    fn upload(&mut self, keyboard: &ZwpVirtualKeyboardV1, keymap: &str) -> Result<()> {
        let fd = rustix::fs::memfd_create("tokativ-keymap", rustix::fs::MemfdFlags::CLOEXEC)
            .context("creating the keymap file")?;
        let mut file = std::fs::File::from(fd);
        file.write_all(keymap.as_bytes())?;
        file.write_all(&[0])?;
        keyboard.keymap(
            KeymapFormat::XkbV1.into(),
            file.as_fd(),
            keymap.len() as u32 + 1,
        );
        self.queue
            .roundtrip(&mut self.state)
            .context("uploading the keymap")?;
        Ok(())
    }

    fn key(&mut self, keyboard: &ZwpVirtualKeyboardV1, code: u32, pressed: bool) -> Result<()> {
        let time = self.started.elapsed().as_millis() as u32;
        keyboard.key(time, code, pressed as u32);
        self.queue
            .roundtrip(&mut self.state)
            .context("sending a key")?;
        thread::sleep(KEY_GAP);
        Ok(())
    }

    /// Sends what is queued and handles whatever the compositor has sent, waiting up to
    /// `timeout` for something to arrive.
    fn pump(&mut self, timeout: Duration) -> Result<()> {
        self.queue.dispatch_pending(&mut self.state)?;
        self.connection.flush()?;
        if let Some(guard) = self.queue.prepare_read() {
            let readable = {
                let fd = guard.connection_fd();
                let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                let timeout = Timespec::try_from(timeout).unwrap_or_default();
                rustix::event::poll(&mut fds, Some(&timeout)).context("polling Wayland")? > 0
            };
            if readable {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(error))
                        if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        // Keys forwarded by the dispatch callbacks go out now rather than at the next tick.
        self.connection.flush()?;
        self.look_for_bridge();
        self.listen(Duration::ZERO);
        Ok(())
    }
}

/// The method on KWin, where the input method is Tokativ's own once KWin has started it, and
/// the paste is the only other way.
fn kwin_status(connected: bool) -> Status {
    match connected {
        true => Status {
            method: Method::InputMethod,
            note: None,
        },
        false => {
            Status {
                method: Method::Clipboard,
                note: Some(kwin::absence().unwrap_or_else(|| {
                    "KWin has not started Tokativ's input method yet".to_string()
                })),
            }
        }
    }
}

/// Picks the method for `preferred`, stepping down the chain when a protocol is missing or the
/// input method belongs to someone else.
fn choose(
    preferred: Backend,
    input_method: bool,
    virtual_keyboard: bool,
    other: Option<String>,
) -> Status {
    let input_method_note = match (input_method, &other) {
        (false, _) => Some("the compositor has no input method protocol".to_string()),
        (true, Some(other)) => Some(format!("{other} already owns the input method")),
        (true, None) => None,
    };
    let keyboard_note =
        (!virtual_keyboard).then(|| "the compositor has no virtual keyboard protocol".to_string());
    let chain: &[Method] = match preferred {
        Backend::Auto | Backend::InputMethod => &[
            Method::InputMethod,
            Method::VirtualKeyboard,
            Method::Clipboard,
        ],
        Backend::VirtualKeyboard => &[Method::VirtualKeyboard, Method::Clipboard],
        Backend::Clipboard => &[Method::Clipboard],
    };
    let mut notes = Vec::new();
    for method in chain {
        let missing = match method {
            Method::InputMethod => &input_method_note,
            Method::VirtualKeyboard => &keyboard_note,
            Method::Clipboard => &None,
        };
        match missing {
            None => {
                return Status {
                    method: *method,
                    note: (!notes.is_empty()).then(|| notes.join(", ")),
                };
            }
            Some(note) => notes.push(note.clone()),
        }
    }
    unreachable!("the clipboard is always available")
}

/// Splits `text` into pieces of at most `limit` bytes on character boundaries.
pub(crate) fn chunks(text: &str, limit: usize) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(limit);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, tail) = rest.split_at(end);
        rest = tail;
        Some(chunk)
    })
}

fn bind<I>(globals: &GlobalList, handle: &QueueHandle<State>) -> Option<I>
where
    I: Proxy + 'static,
    State: Dispatch<I, ()>,
{
    globals.bind::<I, State, ()>(handle, 1..=1, ()).ok()
}

impl Dispatch<ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.pending_active = true,
            zwp_input_method_v2::Event::Deactivate => state.pending_active = false,
            zwp_input_method_v2::Event::SurroundingText { text, cursor, .. } => {
                let cursor = (cursor as usize).min(text.len());
                let before = text.get(..cursor).map(str::to_owned);
                *state.before.lock().expect("before lock") = before;
            }
            zwp_input_method_v2::Event::Done => {
                state.serial = state.serial.wrapping_add(1);
                state.active = state.pending_active;
            }
            zwp_input_method_v2::Event::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { fd, size, .. } => {
                if let Some(forward) = &state.forward {
                    forward.keymap(KeymapFormat::XkbV1.into(), fd.as_fd(), size);
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                state.depressed = mods_depressed;
                if let Some(forward) = &state.forward {
                    forward.modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                key,
                state: WEnum::Value(pressed),
                time,
                ..
            } => state.key(key, pressed == KeyState::Pressed, time),
            _ => {}
        }
    }
}

impl State {
    /// Takes Escape and Enter, when no shortcut modifier is held, and passes every other key on.
    fn key(&mut self, key: u32, pressed: bool, time: u32) {
        let now = Instant::now();
        while self
            .echoes
            .front()
            .is_some_and(|(_, _, at)| now.duration_since(*at) > ECHO)
        {
            self.echoes.pop_front();
        }
        if let Some(echo) = self
            .echoes
            .iter()
            .position(|(sent, down, _)| *sent == key && *down == pressed)
        {
            self.echoes.remove(echo);
            return;
        }

        let plain = self.depressed & SHORTCUT_MODS == 0;
        let ours = match key {
            KEY_ESC => Some(Key::Escape),
            KEY_ENTER | KEY_KPENTER => Some(Key::Enter),
            _ => None,
        };
        match (ours, pressed) {
            (Some(ours), true) if plain => {
                self.swallowed.push(key);
                (self.keys)(ours);
                return;
            }
            (Some(_), false) if self.swallowed.contains(&key) => {
                self.swallowed.retain(|swallowed| *swallowed != key);
                return;
            }
            _ => {}
        }
        if let Some(forward) = &self.forward {
            let state = match pressed {
                true => KeyState::Pressed,
                false => KeyState::Released,
            };
            forward.key(time, key, state.into());
            self.echoes.push_back((key, pressed, now));
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
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

wayland_client::delegate_noop!(State: ignore WlSeat);
wayland_client::delegate_noop!(State: ignore ZwpInputMethodManagerV2);
wayland_client::delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(State: ignore ZwpVirtualKeyboardV1);
wayland_client::delegate_noop!(State: ignore OrgKdeKwinFakeInput);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_respect_character_boundaries() {
        let text = "приветhello";
        let pieces: Vec<&str> = chunks(text, 5).collect();
        assert_eq!(pieces.concat(), text);
        assert!(pieces.iter().all(|piece| piece.len() <= 5));
    }

    #[test]
    fn chain_steps_down_with_reasons() {
        let status = choose(Backend::Auto, true, true, Some("fcitx5".into()));
        assert_eq!(status.method, Method::VirtualKeyboard);
        assert_eq!(
            status.note.as_deref(),
            Some("fcitx5 already owns the input method")
        );
        let status = choose(Backend::Auto, true, true, None);
        assert_eq!(
            status,
            Status {
                method: Method::InputMethod,
                note: None
            }
        );
        assert_eq!(
            choose(Backend::VirtualKeyboard, false, false, None).method,
            Method::Clipboard
        );
    }
}
