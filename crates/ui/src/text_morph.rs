//! Cached text shapes with spring motion between matching grapheme clusters. Painting applies
//! color and opacity directly to glyphs, without a blur/compositing pass for each word.

use std::{rc::Rc, sync::OnceLock, time::Duration};

use gpui::prelude::*;
use gpui::{
    App, Bounds, Hsla, Pixels, Point, ScrollHandle, SharedString, Size, SpringConfig, Style,
    TextAlign, TextRun, Window, WrappedLine, canvas, font, point, px,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::spring::Spring;

pub(crate) const FONT_SIZE: Pixels = px(16.);
pub(crate) const LINE_HEIGHT: Pixels = px(24.);
const RISE: f32 = 5.;
const MOVE: SpringConfig = SpringConfig::new(440., 38., 1.);
const FADE: SpringConfig = SpringConfig::new(520., 46., 1.);
const ALIGNMENT_LIMIT: usize = 16_384;
static EMOJI_FONT: OnceLock<Option<&'static str>> = OnceLock::new();

struct Glyph {
    text: SharedString,
    x: Spring,
    y: Spring,
    alpha: Spring,
    present: bool,
    placed: bool,
    shape: Option<Rc<WrappedLine>>,
}

impl Glyph {
    fn new(text: &str, scale: f32) -> Self {
        let mut alpha = Spring::new(FADE);
        alpha.aim(1., FADE);
        let mut y = Spring::new(MOVE);
        y.state.position = RISE * scale;
        Self {
            text: text.to_owned().into(),
            x: Spring::new(MOVE),
            y,
            alpha,
            present: true,
            placed: false,
            shape: None,
        }
    }
}

pub(crate) struct Morph {
    text: SharedString,
    glyphs: Vec<Glyph>,
    natural: Option<Rc<WrappedLine>>,
    shaped: Option<Rc<WrappedLine>>,
    size: Size<Pixels>,
    limit: Pixels,
    moving: bool,
    /// How large the text is drawn, 1 for [`FONT_SIZE`] on [`LINE_HEIGHT`].
    scale: f32,
}

pub(crate) struct TextViewport {
    pub scroll: ScrollHandle,
    pub bleed: Pixels,
    pub size: Size<Pixels>,
    pub translation: Pixels,
}

impl Morph {
    pub fn new(text: &str, scale: f32) -> Self {
        let mut this = Self {
            text: "".into(),
            glyphs: Vec::new(),
            natural: None,
            shaped: None,
            size: Size::default(),
            limit: Pixels::ZERO,
            moving: false,
            scale,
        };
        this.retarget(text);
        this
    }

    pub fn retarget(&mut self, text: &str) {
        if self.text.as_ref() == text {
            return;
        }
        let (mut previous, mut leaving): (Vec<_>, Vec<_>) = std::mem::take(&mut self.glyphs)
            .into_iter()
            .partition(|glyph| glyph.present);
        let next: Vec<_> = text.graphemes(true).collect();
        let old: Vec<_> = previous.iter().map(|glyph| glyph.text.as_ref()).collect();
        let matched = align(&old, &next);
        let mut previous: Vec<_> = previous.drain(..).map(Some).collect();
        for (text, matched) in next.into_iter().zip(matched) {
            self.glyphs.push(
                matched
                    .and_then(|index| previous[index].take())
                    .unwrap_or_else(|| Glyph::new(text, self.scale)),
            );
        }
        for mut glyph in previous.into_iter().flatten() {
            glyph.present = false;
            glyph.alpha.aim(0., FADE);
            glyph.y.aim(glyph.y.target - RISE * self.scale, MOVE);
            leaving.push(glyph);
        }
        self.glyphs.extend(leaving);
        self.text = text.to_owned().into();
        self.natural = None;
        self.shaped = None;
        self.moving = true;
    }

    pub fn natural_width(&mut self, window: &Window) -> Pixels {
        let line = self
            .natural
            .get_or_insert_with(|| shape(self.text.clone(), None, self.scale, window));
        line.unwrapped_layout.width.ceil()
    }

    pub fn layout(&mut self, limit: Pixels, window: &Window) -> Size<Pixels> {
        if self.shaped.is_some() && self.limit == limit {
            return self.size;
        }
        self.natural_width(window);
        let natural = self.natural.as_ref().unwrap();
        let line = if natural.width() <= limit {
            natural.clone()
        } else {
            shape(self.text.clone(), Some(limit), self.scale, window)
        };
        let line_height = LINE_HEIGHT * self.scale;
        self.size = line.size(line_height);
        self.size.width = self.size.width.ceil().min(limit);
        let mut byte = 0;
        for glyph in self.glyphs.iter_mut().filter(|glyph| glyph.present) {
            let destination = glyph_position(&line, byte, line_height);
            if !glyph.placed {
                glyph.x.state.position = destination.x.as_f32();
                glyph.y.state.position += destination.y.as_f32();
                glyph.placed = true;
            }
            glyph.x.aim(destination.x.as_f32(), MOVE);
            glyph.y.aim(destination.y.as_f32(), MOVE);
            self.moving |= !glyph.x.settled() || !glyph.y.settled();
            glyph
                .shape
                .get_or_insert_with(|| shape(glyph.text.clone(), None, self.scale, window));
            byte += glyph.text.len();
        }
        self.limit = limit;
        self.shaped = Some(line);
        self.size
    }

    pub fn step(&mut self, elapsed: Duration) -> bool {
        if !self.moving {
            return false;
        }
        self.moving = false;
        for glyph in &mut self.glyphs {
            for spring in [&mut glyph.x, &mut glyph.y, &mut glyph.alpha] {
                spring.step(elapsed);
                self.moving |= !spring.settled();
            }
        }
        self.glyphs
            .retain(|glyph| glyph.present || !glyph.alpha.settled());
        self.moving
    }

    pub fn finish(&mut self) {
        self.glyphs.retain(|glyph| glyph.present);
        for glyph in &mut self.glyphs {
            glyph.x.finish();
            glyph.y.finish();
            glyph.alpha.finish();
        }
        self.moving = false;
    }

    pub fn settled(&self) -> bool {
        !self.moving
    }

    pub fn element(
        &self,
        color: Hsla,
        viewport: &TextViewport,
        offset: gpui::Point<Pixels>,
    ) -> impl IntoElement + Styled {
        let line = self.shaped.clone();
        let moving = !self.settled()
            || line
                .as_ref()
                .is_some_and(|line| !line.wrap_boundaries().is_empty());
        let glyphs: Vec<_> = if moving {
            self.glyphs
                .iter()
                .filter(|glyph| glyph.alpha.at() > 0.)
                .filter_map(|glyph| {
                    Some((
                        glyph.shape.clone()?,
                        point(px(glyph.x.at()), px(glyph.y.at())),
                        glyph.alpha.at().clamp(0., 1.),
                        px(glyph.y.target + if glyph.present { 0. } else { RISE * self.scale }),
                    ))
                })
                .collect()
        } else {
            Vec::new()
        };
        let scroll = viewport.scroll.clone();
        let bleed = viewport.bleed;
        let viewport_size = viewport.size;
        let translation = viewport.translation;
        let scale = self.scale;
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                let viewport = Bounds {
                    origin: scroll.bounds().origin + point(bleed, bleed),
                    size: viewport_size,
                };
                let bounds = Bounds {
                    origin: bounds.origin + offset,
                    size: bounds.size,
                };
                if !bounds.intersects(&window.content_mask().bounds) {
                    return;
                }
                if moving {
                    for (line, offset, alpha, row) in glyphs {
                        let origin = bounds.origin + offset;
                        let alpha = alpha
                            * row_visibility(bounds.origin.y + row + translation, viewport, scale);
                        if alpha > 0. {
                            paint_line(&line, origin, color.opacity(alpha), scale, window, cx);
                        }
                    }
                } else if let Some(line) = line {
                    if line.wrap_boundaries().is_empty() {
                        let alpha = row_visibility(bounds.origin.y + translation, viewport, scale);
                        if alpha > 0. {
                            let color = color.opacity(alpha);
                            paint_line(&line, bounds.origin, color, scale, window, cx);
                        }
                    } else {
                        window.with_content_mask(
                            Some(gpui::ContentMask { bounds: viewport }),
                            |window| {
                                paint_line(&line, bounds.origin, color, scale, window, cx);
                            },
                        );
                    }
                }
            },
        )
        .w(self.size.width)
        .h(self.size.height)
        .flex_none()
    }
}

/// GPUI's tinted paint deliberately skips color emoji. Keep its text rendering and paint
/// those glyphs separately, preserving their colors and applying the same animated opacity.
fn paint_line(
    line: &WrappedLine,
    origin: Point<Pixels>,
    color: Hsla,
    scale: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let line_height = LINE_HEIGHT * scale;
    let _ = line.paint_tinted(
        origin,
        line_height,
        TextAlign::Left,
        None,
        color,
        window,
        cx,
    );
    let baseline = (line_height - line.ascent() - line.descent()) / 2. + line.ascent();
    let opacity = Style {
        opacity: Some(color.a),
        ..Style::default()
    };
    opacity.paint(
        Bounds::new(origin, line.size(line_height)),
        window,
        cx,
        |window, _| {
            for run in &line.unwrapped_layout.runs {
                for glyph in run.glyphs.iter().filter(|glyph| glyph.is_emoji) {
                    let position = glyph_position(line, glyph.index, line_height)
                        + point(
                            glyph.position.x - line.unwrapped_layout.x_for_index(glyph.index),
                            baseline + glyph.position.y,
                        );
                    let font_size = FONT_SIZE * scale;
                    let _ = window.paint_emoji(origin + position, run.font_id, glyph.id, font_size);
                }
            }
        },
    );
}

fn shape(
    text: SharedString,
    limit: Option<Pixels>,
    scale: f32,
    window: &Window,
) -> Rc<WrappedLine> {
    // Explicit emoji presentation must also override a monochrome glyph in the text font
    // (for example, Inter's heart). Use an installed color font for the complete cluster.
    let emoji_font = text
        .contains('\u{fe0f}')
        .then(|| {
            *EMOJI_FONT.get_or_init(|| {
                let names = window.text_system().all_font_names();
                [
                    "Apple Color Emoji",
                    "Segoe UI Emoji",
                    "Noto Color Emoji",
                    "Twemoji Mozilla",
                ]
                .into_iter()
                .find(|candidate| {
                    names
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(candidate))
                })
            })
        })
        .flatten();
    let runs = text_runs(&text, emoji_font);
    Rc::new(
        window
            .text_system()
            .shape_text(text, FONT_SIZE * scale, &runs, limit, None)
            .expect("caption text must shape")
            .into_iter()
            .next()
            .expect("a word has a line"),
    )
}

fn text_runs(text: &str, emoji_font: Option<&'static str>) -> Vec<TextRun> {
    let mut runs: Vec<TextRun> = Vec::new();
    for grapheme in text.graphemes(true) {
        let family = if grapheme.contains('\u{fe0f}') {
            emoji_font.unwrap_or("Inter")
        } else {
            "Inter"
        };
        if let Some(last) = runs
            .last_mut()
            .filter(|run| run.font.family.as_ref() == family)
        {
            last.len += grapheme.len();
        } else {
            runs.push(TextRun {
                len: grapheme.len(),
                font: font(family),
                color: gpui::white(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }
    runs
}

fn glyph_position(line: &WrappedLine, byte: usize, line_height: Pixels) -> gpui::Point<Pixels> {
    // At a soft wrap's exact byte boundary the glyph belongs to the following row.
    let mut start = 0;
    let mut row = 0;
    for boundary in line.wrap_boundaries() {
        let index = line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index;
        if index > byte {
            break;
        }
        start = index;
        row += 1;
    }
    point(
        line.unwrapped_layout.x_for_index(byte) - line.unwrapped_layout.x_for_index(start),
        line_height * row as f32,
    )
}

/// How much of a row of text drawn at `scale` with its top at `y` shows in `viewport`.
fn row_visibility(y: Pixels, viewport: Bounds<Pixels>, scale: f32) -> f32 {
    let line_height = LINE_HEIGHT * scale;
    let coverage = ((y + line_height).min(viewport.bottom()) - y.max(viewport.top())) / line_height;
    // Fade through half a line of travel, finishing inside the existing padding/bleed.
    // No visible ink reaches the card's clip, and old rows have exactly zero opacity at rest.
    let t = ((coverage - 0.5) * 2.).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// Longest common subsequence keeps unchanged graphemes, including punctuation edits. Bound
/// the work for unusually long unbroken tokens; their common prefix/suffix still stay put.
pub(crate) fn align(previous: &[&str], next: &[&str]) -> Vec<Option<usize>> {
    let mut result = vec![None; next.len()];
    if previous.len().saturating_mul(next.len()) > ALIGNMENT_LIMIT {
        let prefix = previous
            .iter()
            .zip(next)
            .take_while(|(a, b)| a == b)
            .count();
        for (index, item) in result.iter_mut().enumerate().take(prefix) {
            *item = Some(index);
        }
        let suffix = previous[prefix..]
            .iter()
            .rev()
            .zip(next[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        for offset in 0..suffix {
            result[next.len() - 1 - offset] = Some(previous.len() - 1 - offset);
        }
        return result;
    }
    let stride = next.len() + 1;
    let mut lengths = vec![0; (previous.len() + 1) * stride];
    for i in (0..previous.len()).rev() {
        for j in (0..next.len()).rev() {
            lengths[i * stride + j] = if previous[i] == next[j] {
                1 + lengths[(i + 1) * stride + j + 1]
            } else {
                lengths[(i + 1) * stride + j].max(lengths[i * stride + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < previous.len() && j < next.len() {
        if previous[i] == next[j] {
            result[j] = Some(i);
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * stride + j] >= lengths[i * stride + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_presentation_uses_the_color_font_without_tinting_punctuation_or_letters() {
        let runs = text_runs("Привет ❤️, 👩‍❤️‍👨!", Some("Noto Color Emoji"));
        let mut start = 0;
        let fragments: Vec<_> = runs
            .iter()
            .map(|run| {
                let text = &"Привет ❤️, 👩‍❤️‍👨!"[start..start + run.len];
                start += run.len;
                (text, run.font.family.as_ref())
            })
            .collect();
        assert_eq!(
            fragments,
            vec![
                ("Привет ", "Inter"),
                ("❤️", "Noto Color Emoji"),
                (", ", "Inter"),
                ("👩‍❤️‍👨", "Noto Color Emoji"),
                ("!", "Inter")
            ]
        );
        let fallback = text_runs("Привет ❤️!", None);
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].len, "Привет ❤️!".len());
    }

    #[test]
    fn punctuation_does_not_restart_existing_letters_or_their_velocity() {
        let mut morph = Morph::new("привет", 1.);
        morph.finish();
        morph.glyphs[0].x.state.velocity = 7.;
        morph.retarget("привет,");
        for glyph in morph.glyphs.iter().take(6) {
            assert_eq!(glyph.alpha.at(), 1.);
        }
        assert_eq!(morph.glyphs[0].x.state.velocity, 7.);
        assert_eq!(morph.glyphs[6].text.as_ref(), ",");
        assert_eq!(morph.glyphs[6].alpha.at(), 0.);
    }

    #[test]
    fn an_insertion_keeps_the_common_prefix_and_suffix() {
        assert_eq!(
            align(&["c", "a", "t"], &["c", "o", "a", "t"]),
            vec![Some(0), None, Some(1), Some(2)]
        );
        let mut morph = Morph::new("cat", 1.);
        morph.finish();
        morph.retarget("coat");
        assert_eq!(
            morph.glyphs.iter().filter(|g| g.alpha.at() == 1.).count(),
            3
        );
        morph.retarget("cat");
        assert!(
            morph
                .glyphs
                .iter()
                .any(|g| !g.present && g.text.as_ref() == "o")
        );
    }

    #[test]
    fn combining_marks_and_emoji_remain_whole_graphemes() {
        let morph = Morph::new("е\u{301}👩‍💻", 1.);
        assert_eq!(morph.glyphs.len(), 2);
        assert_eq!(morph.glyphs[0].text.as_ref(), "е\u{301}");
        assert_eq!(morph.glyphs[1].text.as_ref(), "👩‍💻");
    }

    #[test]
    fn rows_fade_smoothly_and_disappear_before_their_letters_can_be_clipped() {
        let viewport = Bounds {
            origin: point(px(0.), px(10.)),
            size: Size::new(px(100.), LINE_HEIGHT * 3.),
        };
        assert_eq!(row_visibility(px(10.) - LINE_HEIGHT, viewport, 1.), 0.);
        assert_eq!(
            row_visibility(px(10.) - LINE_HEIGHT + px(0.8), viewport, 1.),
            0.
        );
        assert_eq!(row_visibility(viewport.bottom(), viewport, 1.), 0.);
        assert_eq!(row_visibility(px(10.), viewport, 1.), 1.);
        assert_eq!(
            row_visibility(px(10.) - LINE_HEIGHT / 4., viewport, 1.),
            0.5
        );
        assert_eq!(row_visibility(px(10.) - LINE_HEIGHT / 2., viewport, 1.), 0.);
        for step in 1..=24 {
            let previous = row_visibility(px(10.) - px((step - 1) as f32 / 2.), viewport, 1.);
            let current = row_visibility(px(10.) - px(step as f32 / 2.), viewport, 1.);
            assert!(current <= previous);
            assert!(previous - current < 0.07);
        }
    }
}
