//! The model list and the Hugging Face search, shared by the settings and the first-run setup.

use gpui::prelude::*;
use gpui::{AnyElement, App, ClickEvent, Entity, Pixels, SharedString, Window, div, px};
use tokativ_engine::ModelId;
use tokativ_engine::models::{self, RemoteFile, Repository};
use ui::{ActiveTheme as _, Button, Text};

use crate::chrome::{self, meter};
use crate::host::{Library, ModelState};

type Choose = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// What a model row says about the model, beyond its state on disk.
pub(crate) struct Described {
    pub id: ModelId,
    pub name: String,
    pub maker: Option<String>,
    pub summary: Option<String>,
    /// The recommendation shown beside the name, such as "Recommended for GPU".
    pub recommended: Option<&'static str>,
}

impl Described {
    /// The description for a model: the curated one for a suggested model, the repository and
    /// file for anything else.
    pub fn of(id: ModelId) -> Self {
        match models::suggested(&id) {
            Some(suggested) => Self {
                name: suggested.name.to_owned(),
                maker: Some(suggested.maker.to_owned()),
                summary: Some(suggested.summary.to_owned()),
                recommended: suggested.recommended,
                id,
            },
            None => Self {
                name: id.name(),
                maker: None,
                summary: Some(id.repo.clone()),
                recommended: None,
                id,
            },
        }
    }
}

/// The models to list before any search. With `offer` on, as in the setup, only the recommended
/// models are listed, with their recommendations. With it off the installed models and the ones
/// downloading are, without recommendations. `also` adds models either way, such as the one
/// chosen.
pub(crate) fn listed(library: &Library, also: &[ModelId], offer: bool) -> Vec<Described> {
    let mut ids: Vec<ModelId> = match offer {
        true => models::SUGGESTED
            .iter()
            .filter(|suggested| suggested.recommended.is_some())
            .filter_map(|suggested| ModelId::parse(suggested.id))
            .collect(),
        false => Vec::new(),
    };
    let fetching = library.fetching();
    let installed = library
        .installed_models()
        .iter()
        .map(|model| &model.id)
        .chain(&fetching)
        .filter(|_| !offer);
    for id in installed.chain(also) {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids.into_iter()
        .map(|id| {
            let mut described = Described::of(id);
            if !offer {
                described.recommended = None;
            }
            described
        })
        .collect()
}

/// One model's row: its name and maker, what it is good at, where it stands on disk, and the
/// button that changes that. `leading` goes in front, such as a radio. `choose` runs when an
/// installed model's row is clicked, and such a row shades under the pointer with `radius`
/// corners. `inset` is the room on either side of the content.
pub(crate) fn model_row(
    model: Described,
    library: &Entity<Library>,
    leading: Option<AnyElement>,
    choose: Option<Choose>,
    inset: Pixels,
    radius: Pixels,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let state = library.read(cx).state(&model.id);
    let (status, progress) = match &state {
        ModelState::Missing => ("Not downloaded".to_string(), None),
        ModelState::Downloading { done, total } => {
            let total = (*total).max(1);
            (
                format!(
                    "Downloading, {} of {}",
                    chrome::size(*done),
                    chrome::size(total)
                ),
                Some(*done as f32 / total as f32),
            )
        }
        ModelState::Installed { bytes } => (format!("{} on disk", chrome::size(*bytes)), None),
        ModelState::Failed(error) => (format!("Download failed: {error}"), None),
    };
    let key = model.id.key();
    let id = model.id.clone();
    let action = {
        let library = library.clone();
        match state {
            ModelState::Downloading { .. } => {
                Button::new(SharedString::from(format!("cancel-{key}")))
                    .label("Cancel")
                    .small()
                    .outline()
                    .on_click(move |_, _, cx| library.update(cx, |library, _| library.cancel(&id)))
            }
            ModelState::Installed { .. } => {
                Button::new(SharedString::from(format!("delete-{key}")))
                    .label("Delete")
                    .small()
                    .outline()
                    .on_click(move |_, _, cx| {
                        library.update(cx, |library, cx| library.delete(&id, cx))
                    })
            }
            ModelState::Missing | ModelState::Failed(_) => {
                Button::new(SharedString::from(format!("download-{key}")))
                    .label("Download")
                    .small()
                    .primary()
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        library.update(cx, |library, cx| library.download(id, cx))
                    })
            }
        }
    };
    let installed = matches!(state, ModelState::Installed { .. });
    let small = theme.text(Text::Small);
    div()
        .id(SharedString::from(format!("model-{key}")))
        .flex()
        .items_start()
        .gap_3()
        .px(inset)
        .py_3()
        .rounded(radius)
        .squircle()
        .when_some(choose.filter(|_| installed), |this, choose| {
            this.cursor_pointer()
                .hover(|style| style.bg(theme.secondary.opacity(0.6)))
                .on_click(choose)
        })
        .when_some(leading, |this, leading| {
            this.child(div().flex_none().pt(px(2.)).child(leading))
        })
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(model.name)
                        .when_some(model.maker, |this, maker| {
                            this.child(
                                div()
                                    .text_size(small)
                                    .text_color(theme.muted_foreground)
                                    .child(maker),
                            )
                        })
                        .when_some(model.recommended, |this, text| this.child(tag(text, cx))),
                )
                .when_some(model.summary, |this, summary| {
                    this.child(
                        div()
                            .text_size(small)
                            .text_color(theme.muted_foreground)
                            .child(summary),
                    )
                })
                .when_some(progress, |this, progress| {
                    this.child(div().pt_1().child(meter(progress, px(6.), cx)))
                })
                .child(
                    div()
                        .text_size(small)
                        .text_color(theme.muted_foreground.opacity(0.75))
                        .child(status),
                ),
        )
        .child(div().flex_none().child(action))
        .into_any_element()
}

/// A small label beside a model's name.
fn tag(text: &'static str, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .px_1p5()
        .rounded(theme.radius)
        .squircle()
        .bg(theme.secondary)
        .text_size(theme.text(Text::Tiny))
        .text_color(theme.foreground.opacity(0.8))
        .child(text)
}

/// The height of every row in the virtualized search lists. A uniform list only draws the rows
/// in view, which needs them all the same height.
pub(crate) const LIST_ROW: Pixels = px(60.);

/// One repository found by search, which opens its files when clicked.
pub(crate) fn result_row(repository: &Repository, open: Choose, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let small = theme.text(Text::Small);
    let (owner, name) = repository
        .id
        .split_once('/')
        .unwrap_or(("", repository.id.as_str()));
    div()
        .id(SharedString::from(format!("repo-{}", repository.id)))
        .w_full()
        .h(LIST_ROW)
        .flex()
        .items_center()
        .gap_3()
        .px(chrome::GROUP_PADDING)
        .rounded(crate::nested_radius(
            chrome::GROUP_RADIUS,
            chrome::LISTED_INSET + px(1.),
        ))
        .squircle()
        .cursor_pointer()
        .hover(|style| style.bg(theme.secondary.opacity(0.6)))
        .on_click(open)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(name.to_owned()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(small)
                                .text_color(theme.muted_foreground)
                                .child(owner.to_owned()),
                        ),
                )
                .child(facts(repository, cx)),
        )
        .child(
            gpui::svg()
                .path(icons::path("icons/chevron-right.svg"))
                .size(px(16.))
                .flex_none()
                .text_color(theme.muted_foreground),
        )
        .into_any_element()
}

/// A repository's architecture and how often it was downloaded, in one muted line.
pub(crate) fn facts(repository: &Repository, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_size(theme.text(Text::Small))
        .text_color(theme.muted_foreground)
        .child(
            repository
                .architecture
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
        )
        .child("·")
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    gpui::svg()
                        .path(crate::DOWNLOAD_ICON)
                        .size(px(12.))
                        .flex_none()
                        .text_color(theme.muted_foreground),
                )
                .child(count(repository.downloads)),
        )
        .into_any_element()
}

/// One GGUF file of a repository: its quantization and size, where it stands, and the button
/// that downloads, cancels or deletes it.
pub(crate) fn file_row(
    repo: &str,
    file: &RemoteFile,
    library: &Entity<Library>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let small = theme.text(Text::Small);
    let id = ModelId {
        repo: repo.to_owned(),
        file: file.name.clone(),
    };
    let state = library.read(cx).state(&id);
    let status = match &state {
        ModelState::Missing => chrome::size(file.size),
        ModelState::Downloading { done, total } => format!(
            "Downloading, {:.0}%",
            *done as f64 / (*total).max(1) as f64 * 100.
        ),
        ModelState::Installed { .. } => format!("{}, installed", chrome::size(file.size)),
        ModelState::Failed(error) => format!("Failed: {error}"),
    };
    let key = id.key();
    let action = {
        let library = library.clone();
        let id = id.clone();
        match state {
            ModelState::Downloading { .. } => {
                Button::new(SharedString::from(format!("cancel-{key}")))
                    .label("Cancel")
                    .small()
                    .outline()
                    .on_click(move |_, _, cx| library.update(cx, |library, _| library.cancel(&id)))
            }
            ModelState::Installed { .. } => {
                Button::new(SharedString::from(format!("delete-{key}")))
                    .label("Delete")
                    .small()
                    .outline()
                    .on_click(move |_, _, cx| {
                        library.update(cx, |library, cx| library.delete(&id, cx))
                    })
            }
            ModelState::Missing | ModelState::Failed(_) => {
                Button::new(SharedString::from(format!("download-{key}")))
                    .label("Download")
                    .small()
                    .primary()
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        library.update(cx, |library, cx| library.download(id, cx))
                    })
            }
        }
    };
    div()
        .w_full()
        .h(LIST_ROW)
        .flex()
        .items_center()
        .gap_3()
        .px(chrome::GROUP_PADDING)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    file.quantization()
                        .map(str::to_owned)
                        .unwrap_or_else(|| file.name.clone()),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(small)
                        .text_color(theme.muted_foreground)
                        .child(status),
                ),
        )
        .child(div().flex_none().child(action))
        .into_any_element()
}

/// A count as people read one: 1.9M, 640K, 812.
fn count(value: u64) -> String {
    match value {
        0..1_000 => value.to_string(),
        1_000..1_000_000 => format!("{:.0}K", value as f64 / 1_000.),
        _ => format!("{:.1}M", value as f64 / 1_000_000.),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_short() {
        assert_eq!(count(812), "812");
        assert_eq!(count(640_400), "640K");
        assert_eq!(count(1_951_595), "2.0M");
    }
}
