//! Push to talk on Plasma. A command bound under System Settings → Shortcuts only runs when its
//! key goes down, so holding the key could not finish dictation. Tokativ registers a shortcut of
//! its own with KDE's global shortcut service instead, which reports the key going up as well. It
//! shows up under System Settings → Keyboard → Shortcuts → Tokativ, where its key can be changed.

use std::sync::OnceLock;
use std::thread;

use anyhow::{Context as _, Result};
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;
use zbus::zvariant::OwnedObjectPath;

const COMPONENT: &str = "tokativ";
const ACTION: &str = "dictate";
/// The component and the action, each by id and by the name System Settings shows.
const ACTION_ID: [&str; 4] = [COMPONENT, ACTION, "Tokativ", "Dictate"];
/// Meta+Shift+D in Qt's key encoding, the modifier bits over the key code. It is the key until
/// the user picks another.
const DEFAULT: i32 = 0x1000_0000 | 0x0200_0000 | 0x44;
/// How kglobalaccel's setter takes the keys: as the default, and as the keys in use. The keys in
/// use come back as the user last chose them, when they did.
const IS_DEFAULT: u32 = 8;
const SET_PRESENT: u32 = 2;

/// Shortcuts as kglobalaccel passes them: up to four key combinations per sequence.
type Keys = Vec<(Vec<i32>,)>;

/// The session bus connection the shortcut was registered on, to give the key back on quit.
static BUS: OnceLock<Connection> = OnceLock::new();

/// Whether this is a Plasma session, where the shortcut service runs.
pub fn plasma() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|name| name.eq_ignore_ascii_case("KDE"))
}

/// Registers the shortcut and, on a thread of its own, calls `pressed(true)` when its key goes
/// down and `pressed(false)` when it comes up.
pub fn listen(pressed: impl Fn(bool) + Send + 'static) {
    let spawned = thread::Builder::new()
        .name("plasma-shortcut".into())
        .spawn(move || {
            if let Err(error) = run(&pressed) {
                log::warn!("hotkey: no Plasma shortcut: {error:#}");
            }
        });
    if let Err(error) = spawned {
        log::warn!("hotkey: cannot start the Plasma shortcut: {error}");
    }
}

/// Lets go of the key, so it reaches other apps while Tokativ is not running.
pub fn stop() {
    let Some(bus) = BUS.get() else {
        return;
    };
    let released = accel(bus).and_then(|accel| {
        accel
            .call_method("setInactive", &(ACTION_ID.as_slice(),))
            .context("releasing the shortcut")
    });
    if let Err(error) = released {
        log::warn!("hotkey: {error:#}");
    }
}

fn accel(bus: &Connection) -> Result<Proxy<'static>> {
    Proxy::new(
        bus,
        "org.kde.kglobalaccel",
        "/kglobalaccel",
        "org.kde.KGlobalAccel",
    )
    .context("reaching kglobalaccel")
}

fn run(pressed: &dyn Fn(bool)) -> Result<()> {
    let bus = Connection::session().context("connecting to the session bus")?;
    let accel = accel(&bus)?;
    accel
        .call_method("doRegister", &(ACTION_ID.as_slice(),))
        .context("registering the shortcut")?;
    // kglobalaccel leaves a key another app already has with that app, so the default only
    // takes when it is free.
    let defaults: Keys = vec![(vec![DEFAULT, 0, 0, 0],)];
    // The keys in use first, as KGlobalAccel itself sets them, then the default, which System
    // Settings offers to reset to.
    let keys: Keys = accel
        .call(
            "setShortcutKeys",
            &(ACTION_ID.as_slice(), &defaults, SET_PRESENT),
        )
        .context("setting the key")?;
    let _: Keys = accel
        .call(
            "setShortcutKeys",
            &(ACTION_ID.as_slice(), &defaults, IS_DEFAULT),
        )
        .context("setting the default key")?;
    match keys
        .iter()
        .any(|(sequence,)| sequence.iter().any(|key| *key != 0))
    {
        true => log::info!("hotkey: Plasma's Dictate shortcut is ready"),
        false => log::info!(
            "hotkey: Plasma's Dictate shortcut has no key yet; pick one under System Settings → Shortcuts → Tokativ"
        ),
    }

    let path: OwnedObjectPath = accel
        .call("getComponent", &(COMPONENT,))
        .context("finding the shortcut's component")?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("org.kde.kglobalaccel.Component")?
        .path(path)?
        .build();
    let signals =
        MessageIterator::for_match_rule(rule, &bus, None).context("listening for the shortcut")?;
    BUS.set(bus.clone()).ok();
    for message in signals {
        let message = message.context("reading a shortcut signal")?;
        let header = message.header();
        let down = match header.member().map(|member| member.as_str()) {
            Some("globalShortcutPressed") => true,
            Some("globalShortcutReleased") => false,
            // Repeats while the key is held add nothing, since the release ends the hold.
            _ => continue,
        };
        let Ok((_, shortcut, _)) = message.body().deserialize::<(String, String, i64)>() else {
            continue;
        };
        if shortcut == ACTION {
            pressed(down);
        }
    }
    Ok(())
}
