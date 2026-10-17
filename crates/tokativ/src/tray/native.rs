//! The menu bar item on macOS and the notification area icon on Windows, through tray-icon. Both
//! have to be made and changed on the main thread, which is where the daemon calls in from.

use anyhow::{Context as _, Result};
use futures::channel::mpsc::UnboundedSender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::{SIZE, Shown, TrayEvent, pixels};

const TOGGLE: &str = "toggle";
const SETTINGS: &str = "settings";
const UPDATES: &str = "updates";
const QUIT: &str = "quit";
/// Model entries are this followed by the model's full name.
const MODEL: &str = "model:";
/// Profile entries are this followed by the profile's name.
const PROFILE: &str = "profile:";

/// The running tray icon. Dropping it removes the icon.
pub struct Tray {
    icon: TrayIcon,
    shown: Option<(bool, Shown)>,
}

impl Tray {
    pub fn spawn(
        events: UnboundedSender<TrayEvent>,
        recording: bool,
        shown: Shown,
    ) -> Result<Self> {
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id.as_ref();
            let event = match id {
                TOGGLE => TrayEvent::Toggle,
                SETTINGS => TrayEvent::Settings,
                UPDATES => TrayEvent::Updates,
                QUIT => TrayEvent::Quit,
                _ => match (id.strip_prefix(MODEL), id.strip_prefix(PROFILE)) {
                    (Some(model), _) => TrayEvent::Model(model.to_owned()),
                    (_, Some(profile)) => TrayEvent::Profile(profile.to_owned()),
                    _ => return,
                },
            };
            events.unbounded_send(event).ok();
        }));
        let icon = TrayIconBuilder::new()
            .with_tooltip("Tokativ")
            .with_menu(Box::new(menu(recording, &shown)?))
            .with_icon(icon(recording)?)
            .build()
            .context("adding the tray icon")?;
        Ok(Self {
            icon,
            shown: Some((recording, shown)),
        })
    }

    /// Rebuilds the menu and icon when what they show has changed.
    pub fn show(&mut self, recording: bool, shown: Shown) {
        let next = Some((recording, shown));
        if self.shown == next {
            return;
        }
        let Some((recording, shown)) = &next else {
            return;
        };
        match menu(*recording, shown) {
            Ok(menu) => self.icon.set_menu(Some(Box::new(menu))),
            Err(error) => log::warn!("tray: {error:#}"),
        }
        if let Err(error) = icon(*recording).and_then(|icon| {
            self.icon
                .set_icon(Some(icon))
                .context("changing the tray icon")
        }) {
            log::warn!("tray: {error:#}");
        }
        self.shown = next;
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    }
}

/// The menu: start or stop, the installed models and the profiles with the ones in use checked,
/// settings, quit.
fn menu(recording: bool, shown: &Shown) -> Result<Menu> {
    let menu = Menu::new();
    let toggle = match recording {
        true => "Stop dictation",
        false => "Start dictation",
    };
    menu.append(&MenuItem::with_id(TOGGLE, toggle, true, None))?;
    if !shown.installed.is_empty() {
        menu.append(&PredefinedMenuItem::separator())?;
        for (model, name) in &shown.installed {
            menu.append(&CheckMenuItem::with_id(
                format!("{MODEL}{model}"),
                name,
                true,
                *model == shown.model,
                None,
            ))?;
        }
    }
    if !shown.profiles.is_empty() {
        menu.append(&PredefinedMenuItem::separator())?;
        for profile in &shown.profiles {
            menu.append(&CheckMenuItem::with_id(
                format!("{PROFILE}{profile}"),
                profile,
                true,
                *profile == shown.profile,
                None,
            ))?;
        }
    }
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(SETTINGS, "Settings", true, None))?;
    menu.append(&MenuItem::with_id(
        UPDATES,
        "Check for updates…",
        true,
        None,
    ))?;
    menu.append(&MenuItem::with_id(QUIT, "Quit", true, None))?;
    Ok(menu)
}

fn icon(recording: bool) -> Result<Icon> {
    let rgba = pixels(recording).into_iter().flatten().collect();
    Icon::from_rgba(rgba, SIZE as u32, SIZE as u32).context("drawing the tray icon")
}
