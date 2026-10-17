//! The StatusNotifierItem in the panel, through ksni.

use anyhow::{Context as _, Result};
use futures::channel::mpsc::UnboundedSender;
use ksni::blocking::{Handle, TrayMethods as _};
use ksni::menu::{MenuItem, RadioGroup, RadioItem, StandardItem};
use tokativ_config::APP_ID;

use super::{SIZE, Shown, TrayEvent, pixels};

/// The running tray item. Dropping it removes the item from the panel.
pub struct Tray {
    handle: Handle<Item>,
}

impl Tray {
    pub fn spawn(
        events: UnboundedSender<TrayEvent>,
        recording: bool,
        shown: Shown,
    ) -> Result<Self> {
        let item = Item {
            events,
            recording,
            shown,
        };
        let handle = item
            .spawn()
            .context("cannot reach the status notifier host")?;
        Ok(Self { handle })
    }

    pub fn show(&self, recording: bool, shown: Shown) {
        self.handle.update(|item| {
            item.recording = recording;
            item.shown = shown;
        });
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        drop(self.handle.shutdown());
    }
}

struct Item {
    events: UnboundedSender<TrayEvent>,
    recording: bool,
    shown: Shown,
}

impl Item {
    fn send(&self, event: TrayEvent) {
        self.events.unbounded_send(event).ok();
    }

    fn entry(label: &str, event: TrayEvent) -> MenuItem<Self> {
        StandardItem {
            label: label.into(),
            activate: Box::new(move |item: &mut Self| item.send(event.clone())),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "Tokativ".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        match self.recording {
            true => "media-record-symbolic".into(),
            false => "audio-input-microphone-symbolic".into(),
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![pixmap(self.recording)]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Tokativ".into(),
            description: match self.recording {
                true => "Dictating".into(),
                false => "Ready".to_string(),
            },
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayEvent::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let toggle = match self.recording {
            true => "Stop dictation",
            false => "Start dictation",
        };
        vec![
            Self::entry(toggle, TrayEvent::Toggle),
            MenuItem::Separator,
            RadioGroup {
                selected: self
                    .shown
                    .installed
                    .iter()
                    .position(|(model, _)| *model == self.shown.model)
                    .unwrap_or(usize::MAX),
                select: Box::new(|item: &mut Self, index| {
                    if let Some((model, _)) = item.shown.installed.get(index).cloned() {
                        item.shown.model = model.clone();
                        item.send(TrayEvent::Model(model));
                    }
                }),
                options: self
                    .shown
                    .installed
                    .iter()
                    .map(|(_, name)| RadioItem {
                        label: name.clone(),
                        ..Default::default()
                    })
                    .collect(),
            }
            .into(),
            MenuItem::Separator,
            RadioGroup {
                selected: self
                    .shown
                    .profiles
                    .iter()
                    .position(|profile| *profile == self.shown.profile)
                    .unwrap_or(usize::MAX),
                select: Box::new(|item: &mut Self, index| {
                    if let Some(profile) = item.shown.profiles.get(index).cloned() {
                        item.shown.profile = profile.clone();
                        item.send(TrayEvent::Profile(profile));
                    }
                }),
                options: self
                    .shown
                    .profiles
                    .iter()
                    .map(|profile| RadioItem {
                        label: profile.clone(),
                        ..Default::default()
                    })
                    .collect(),
            }
            .into(),
            MenuItem::Separator,
            Self::entry("Settings", TrayEvent::Settings),
            Self::entry("Quit", TrayEvent::Quit),
        ]
    }
}

/// A plain icon for hosts without the themed names, in ARGB network byte order as the protocol
/// wants.
fn pixmap(recording: bool) -> ksni::Icon {
    let data = pixels(recording)
        .into_iter()
        .flat_map(|[red, green, blue, alpha]| [alpha, red, green, blue])
        .collect();
    ksni::Icon {
        width: SIZE as i32,
        height: SIZE as i32,
        data,
    }
}
