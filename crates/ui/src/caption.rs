//! The caption beside the pill: what the session has said so far and a draft of the phrase being
//! spoken, on any platform. It sits away from the screen edge, lets clicks through, and every
//! new or changed word morphs into place on a spring.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Context, DisplayId, Hsla, Pixels, ScrollHandle, Size, SpringConfig, Window, WindowHandle, div,
    point, px,
};
use tokativ_config::PillEdge;
use ui::ActiveTheme as _;

use crate::overlay;
use crate::pill;
use crate::spring::Spring;
use crate::text_morph::{LINE_HEIGHT, Morph, TextViewport, align};

/// The caption's width, wider than the pill so a phrase has room.
pub(crate) const WIDTH: Pixels = px(520.);
/// Three readable lines in a card larger than the controls. Older lines move up out of view.
pub(crate) const TEXT_HEIGHT: Pixels = px(72.);
pub(crate) const PADDING_X: Pixels = px(20.);
pub(crate) const PADDING_Y: Pixels = px(14.);
pub(crate) const BORDER: Pixels = px(1.);
/// Room for word animations inside the original padding. Rows fade out completely
/// before their letters reach the card's clip.
const TEXT_BLEED: Pixels = px(10.);
pub(crate) const RADIUS: Pixels = px(20.);
/// The space between the pill and the caption.
pub(crate) const GAP: Pixels = px(10.);
const WORD_GAP: Pixels = px(5.);
const WORD_STAGGER: Duration = Duration::from_millis(16);
const MAX_STAGGER: Duration = Duration::from_millis(64);
/// Keep about this many words, removing only complete rows that have already scrolled away.
const MAX_WORDS: usize = 128;
/// The caption itself unfolding towards the pill and folding back.
pub(crate) const FOLDED_SCALE: f32 = 0.9;
pub(crate) const FOLDED_SHIFT: Pixels = px(10.);
pub(crate) const FOLDED_BLUR: Pixels = px(8.);
const INK: SpringConfig = SpringConfig::new(160., 27., 1.);
const REVEAL: SpringConfig = SpringConfig::new(300., 35., 1.);
const NEW_WORD_BLUR: Pixels = px(6.);
/// Damped enough to resize without overshooting the text's required bounds.
const RESIZE: SpringConfig = SpringConfig::new(210., 30., 1.);
/// Scroll the existing lines together, with no bounce past the last line.
const SCROLL: SpringConfig = SpringConfig::new(520., 46., 1.);
const REFLOW: SpringConfig = SpringConfig::new(520., 46., 1.);
pub(crate) const OPEN: SpringConfig = SpringConfig::new(320., 22., 1.);
pub(crate) const CLOSE: SpringConfig = SpringConfig::new(520., 44., 1.);
/// The longest a close may take before the window goes regardless.
const CLOSE_LIMIT: Duration = Duration::from_millis(600);

/// One word on show, with how far it has come in.
struct Word {
    index: usize,
    text: String,
    morph: Morph,
    /// Runs once when a word first enters the caption; revisions only retarget its morph.
    reveal: Spring,
    x: Spring,
    y: Spring,
    placed: bool,
    /// 0 for the draft's muted color, 1 for committed text. Carries its velocity across updates.
    ink: Spring,
    delay: Duration,
}

pub struct Caption {
    edge: PillEdge,
    words: Vec<Word>,
    body: Spring,
    width: Spring,
    height: Spring,
    scroll: Spring,
    scroll_handle: ScrollHandle,
    /// Text always wraps at its final width, independently of the animated background.
    content_size: Option<Size<Pixels>>,
    layout_dirty: bool,
    layout_width: Option<Pixels>,
    closing: Option<Instant>,
    beat: Option<Instant>,
}

/// Opens the caption beside a pill that sits `margin` from the `edge`, folded until the first
/// words arrive.
pub fn open(
    edge: PillEdge,
    margin: f32,
    display: Option<DisplayId>,
    cx: &mut gpui::App,
) -> anyhow::Result<WindowHandle<Caption>> {
    let width = display
        .and_then(|id| cx.find_display(id))
        .map(|display| (display.visible_bounds().size.width - pill::SHADOW * 2.).min(WIDTH))
        .unwrap_or(WIDTH);
    let size = Size::new(
        width + pill::SHADOW * 2.,
        TEXT_HEIGHT + PADDING_Y * 2. + BORDER * 2. + pill::SHADOW * 2.,
    );
    // The caption starts where the pill ends, and its window starts a shadow's width earlier.
    let offset = px(margin) - pill::SHADOW + pill::HEIGHT + GAP;
    let options = overlay::options(edge, offset, size, display, cx);
    cx.open_window(options, move |window, cx| {
        overlay::prepare(window, true);
        cx.new(|_| Caption::new(edge))
    })
}

impl Caption {
    fn new(edge: PillEdge) -> Self {
        Self {
            edge,
            words: Vec::new(),
            body: Spring::new(OPEN),
            width: Spring::new(RESIZE),
            height: Spring::new(RESIZE),
            scroll: Spring::new(SCROLL),
            scroll_handle: ScrollHandle::new(),
            content_size: None,
            layout_dirty: true,
            layout_width: None,
            closing: None,
            beat: None,
        }
    }

    /// Shows `written`, the text the session has inserted, followed by `draft`, the phrase still
    /// being spoken. Words that stay as they were keep still, and each new or changed word comes
    /// in afresh.
    pub fn show(&mut self, written: &str, draft: &str, cx: &mut Context<Self>) {
        self.words = reconcile(std::mem::take(&mut self.words), written, draft);
        self.layout_dirty = true;
        if !self.words.is_empty() && self.closing.is_none() {
            self.body.aim(1., OPEN);
        }
        cx.notify();
    }

    /// Folds the caption away and closes its window once it has.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.closing.is_none() {
            self.closing = Some(Instant::now());
            self.body.aim(0., CLOSE);
            cx.notify();
        }
    }

    fn resize(&mut self, mut content: Size<Pixels>) {
        let first = self.content_size.is_none();
        self.scroll.aim(
            (content.height - TEXT_HEIGHT).max(Pixels::ZERO).as_f32(),
            SCROLL,
        );
        content.height = content.height.min(TEXT_HEIGHT);
        self.width.aim(
            (content.width + PADDING_X * 2. + BORDER * 2.).as_f32(),
            RESIZE,
        );
        self.height.aim(
            (content.height + PADDING_Y * 2. + BORDER * 2.).as_f32(),
            RESIZE,
        );
        // The opening animation already unfolds the first frame. Later changes keep the
        // current dimensions and velocity, even if more text arrives midway through a resize.
        if first {
            self.width.finish();
            self.height.finish();
            self.scroll.finish();
        }
        self.content_size = Some(content);
        self.layout_dirty = false;
    }

    fn trim_history(&mut self, metrics: &[Size<Pixels>], width: Pixels) -> usize {
        let excess = self.words.len().saturating_sub(MAX_WORDS);
        let (count, removed_height) = history_prefix(metrics, width, px(self.scroll.at()), excess);
        if count > 0 {
            self.words.drain(..count);
            // The retained words keep the same wrapping and screen coordinates. Rebase both
            // ends of the spring without changing its velocity as offscreen rows are removed.
            self.scroll.state.position -= removed_height.as_f32();
            self.scroll.target -= removed_height.as_f32();
            for word in &mut self.words {
                if word.placed {
                    word.y.state.position -= removed_height.as_f32();
                    word.y.target -= removed_height.as_f32();
                }
            }
        }
        count
    }

    /// Shape only changed words, and only when text or available width changes. The same
    /// cached shapes supply layout and painting; animation frames do no text measurement.
    fn layout(&mut self, window: &Window) {
        let available =
            (window.viewport_size().width - pill::SHADOW * 2. - PADDING_X * 2. - BORDER * 2.)
                .max(px(1.));
        if self.words.is_empty() || (!self.layout_dirty && self.layout_width == Some(available)) {
            return;
        }
        let natural = self
            .words
            .iter_mut()
            .map(|word| word.morph.natural_width(window))
            .sum::<Pixels>()
            + WORD_GAP * self.words.len().saturating_sub(1) as f32;
        let width = (natural + px(1.)).min(available);
        let mut metrics: Vec<_> = self
            .words
            .iter_mut()
            .map(|word| word.morph.layout(width, window))
            .collect();
        let removed = self.trim_history(&metrics, width);
        metrics.drain(..removed);
        let (positions, height) = flow_layout(&metrics, width);
        for (word, position) in self.words.iter_mut().zip(positions) {
            word.x.aim(position.x.as_f32(), REFLOW);
            word.y.aim(position.y.as_f32(), REFLOW);
            if !word.placed {
                word.x.finish();
                word.y.finish();
                word.placed = true;
            }
        }
        self.resize(Size::new(width, height));
        self.layout_width = Some(available);
    }

    /// Moves every spring on, or lands them all at once when motion is reduced. Returns whether
    /// anything still moves.
    fn step(&mut self, still: bool) -> bool {
        let now = Instant::now();
        let elapsed = self.beat.replace(now).map_or(Duration::ZERO, |beat| {
            (now - beat).min(Duration::from_millis(50))
        });
        let mut moving = false;
        for spring in [
            &mut self.body,
            &mut self.width,
            &mut self.height,
            &mut self.scroll,
        ] {
            if still {
                spring.finish();
            } else {
                spring.step(elapsed);
            }
            moving |= !spring.settled();
        }
        for word in &mut self.words {
            for spring in [&mut word.x, &mut word.y] {
                if still {
                    spring.finish();
                } else {
                    spring.step(elapsed);
                }
                moving |= !spring.settled();
            }
            let morphing = if still {
                word.delay = Duration::ZERO;
                word.morph.finish();
                word.reveal.finish();
                word.ink.finish();
                false
            } else {
                let delay = word.delay.min(elapsed);
                word.delay -= delay;
                word.ink.step(elapsed);
                word.reveal.step(elapsed - delay);
                word.morph.step(elapsed - delay)
            };
            moving |= morphing || !word.ink.settled() || !word.reveal.settled();
        }
        moving
    }
}

impl Render for Caption {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.layout(window);
        let moving = self.step(cx.reduce_motion());
        if let Some(closing) = self.closing
            && (!moving || closing.elapsed() >= CLOSE_LIMIT)
        {
            window.remove_window();
            return div().size_full();
        }
        if moving {
            window.request_animation_frame();
        } else {
            self.beat = None;
        }

        let theme = cx.theme();
        let content = self.content_size.unwrap_or(Size::new(px(1.), LINE_HEIGHT));
        let body = self.body.at().clamp(0., 1.2);
        let shown = body.min(1.);
        // The caption grows out of the pill's side, so it moves in from there.
        let towards_pill = match self.edge {
            PillEdge::Bottom => FOLDED_SHIFT,
            PillEdge::Top => -FOLDED_SHIFT,
        };
        let unfolding = body != 1.;
        // Font rasterization snaps vertical glyph origins to device pixels. Scroll by whole
        // device pixels first, then composite only the fractional remainder for smooth motion.
        // The bleed covers this subpixel translation, including incoming rows.
        let (scroll, translation) = scroll_position(px(self.scroll.at()), window.scale_factor());
        self.scroll_handle.set_offset(point(px(0.), -scroll));

        let bubble = bubble(1., cx)
            .w(px(self.width.at()).max(PADDING_X * 2. + BORDER * 2.))
            .h(px(self.height.at()).max(PADDING_Y * 2. + BORDER * 2.))
            .when(unfolding, |this| {
                this.opacity((shown * 1.6).min(1.))
                    .layer_scale((FOLDED_SCALE + (1. - FOLDED_SCALE) * body).max(0.01))
                    .layer_scale_origin(point(
                        0.5,
                        match self.edge {
                            PillEdge::Bottom => 1.,
                            PillEdge::Top => 0.,
                        },
                    ))
                    .layer_translate(point(px(0.), towards_pill * (1. - shown)))
            })
            .when(shown < 1., |this| this.blur(FOLDED_BLUR * (1. - shown)))
            .child(
                // Scroll before painting/clipping, so incoming lines are rendered throughout
                // the motion. A composited translation would only move already visible pixels.
                div()
                    .id("caption-text")
                    .relative()
                    .left(-TEXT_BLEED)
                    .top(-TEXT_BLEED)
                    .w(content.width + TEXT_BLEED * 2.)
                    .h(content.height + TEXT_BLEED * 2.)
                    .p(TEXT_BLEED)
                    .flex_none()
                    .overflow_hidden()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .when(!self.scroll.settled(), |this| {
                        this.layer_translate(point(px(0.), translation))
                    })
                    .flex()
                    .flex_col()
                    .child(text_flow(
                        self,
                        content,
                        theme.foreground,
                        theme.muted_foreground,
                    )),
            );

        // The caption hugs the pill: above it when the pill sits at the bottom, below it when the
        // pill sits at the top.
        div()
            .size_full()
            .p(pill::SHADOW)
            .flex()
            .flex_col()
            .items_center()
            .font_family("Inter")
            .map(|this| match self.edge {
                PillEdge::Bottom => this.justify_end(),
                PillEdge::Top => this.justify_start(),
            })
            .child(bubble)
    }
}

/// The caption's card without its size, for the text to go in, drawn at `scale`. The caption
/// grows it to fit the words, and the settings show it with a sample.
pub(crate) fn bubble(scale: f32, cx: &gpui::App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .flex_none()
        .px(PADDING_X * scale)
        .py(PADDING_Y * scale)
        .rounded(RADIUS * scale)
        .squircle()
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .shadow(pill::shadow(scale))
        .overflow_hidden()
}

fn text_flow(caption: &Caption, content: Size<Pixels>, settled: Hsla, draft: Hsla) -> gpui::Div {
    let viewport = TextViewport {
        scroll: caption.scroll_handle.clone(),
        bleed: TEXT_BLEED,
        size: content,
        translation: -px(caption.scroll.at()) - caption.scroll_handle.offset().y,
    };
    div()
        .flex_none()
        .relative()
        .w(content.width)
        .h(content.height + px(caption.scroll.target))
        .children(caption.words.iter().map(|word| {
            let blur = NEW_WORD_BLUR * (1. - word.reveal.at().clamp(0., 1.));
            word.morph
                .element(
                    blend(draft, settled, word.ink.at().clamp(0., 1.)),
                    &viewport,
                    point(
                        px(word.x.at() - word.x.target),
                        px(word.y.at() - word.y.target),
                    ),
                )
                .absolute()
                .left(px(word.x.target))
                .top(px(word.y.target))
                .when(blur > px(0.), |this| this.blur(blur))
        }))
}

fn scroll_position(position: Pixels, scale: f32) -> (Pixels, Pixels) {
    let snapped = px((position.as_f32() * scale).round() / scale);
    (snapped, snapped - position)
}

fn flow_layout(metrics: &[Size<Pixels>], width: Pixels) -> (Vec<gpui::Point<Pixels>>, Pixels) {
    let mut used = Pixels::ZERO;
    let mut row_height = Pixels::ZERO;
    let mut total = Pixels::ZERO;
    let mut positions = Vec::with_capacity(metrics.len());
    for word in metrics {
        if used > Pixels::ZERO && used + WORD_GAP + word.width > width {
            total += row_height;
            used = Pixels::ZERO;
            row_height = Pixels::ZERO;
        }
        if used > Pixels::ZERO {
            used += WORD_GAP;
        }
        positions.push(point(used, total));
        used += word.width;
        row_height = row_height.max(word.height);
    }
    (positions, total + row_height)
}

/// Find complete rows within both the history budget and the area already scrolled offscreen.
fn history_prefix(
    metrics: &[Size<Pixels>],
    width: Pixels,
    scrolled: Pixels,
    excess: usize,
) -> (usize, Pixels) {
    let mut used = Pixels::ZERO;
    let mut row_height = Pixels::ZERO;
    let mut removed_height = Pixels::ZERO;
    let mut prefix = (0, Pixels::ZERO);
    for (index, word) in metrics.iter().enumerate() {
        if used > Pixels::ZERO && used + WORD_GAP + word.width > width {
            removed_height += row_height;
            if index > excess || removed_height > scrolled {
                break;
            }
            prefix = (index, removed_height);
            used = Pixels::ZERO;
            row_height = Pixels::ZERO;
        }
        if used > Pixels::ZERO {
            used += WORD_GAP;
        }
        used += word.width;
        row_height = row_height.max(word.height);
    }
    prefix
}

/// Keep springs for words whose text and position survive a draft revision or a final commit.
/// Absolute indices also preserve them when old history is dropped from the front.
fn reconcile(previous: Vec<Word>, written: &str, draft: &str) -> Vec<Word> {
    let next: Vec<_> = written
        .split_whitespace()
        .map(|word| (word, false))
        .chain(draft.split_whitespace().map(|word| (word, true)))
        .collect();
    // Keep the previous row boundary. Arbitrarily cutting to the last N words would rewrap
    // every remaining line and make visible text jump during a long session.
    let skip = previous.first().map_or(0, |word| word.index);
    let skip = if skip >= next.len() { 0 } else { skip };
    let next: Vec<_> = next.into_iter().enumerate().skip(skip).collect();
    let old_keys: Vec<_> = previous.iter().map(|word| word_key(&word.text)).collect();
    let next_keys: Vec<_> = next.iter().map(|(_, (text, _))| word_key(text)).collect();
    let mut matched = align(&old_keys, &next_keys);
    // Match unchanged words first, then pair replacements inside each gap. An insertion
    // gets its own reveal; existing words retain their morph, color and reveal springs.
    let anchors: Vec<_> = matched
        .iter()
        .enumerate()
        .filter_map(|(new, old)| old.map(|old| (new, old)))
        .chain(std::iter::once((next.len(), previous.len())))
        .collect();
    let (mut old_end, mut next_end) = (0, 0);
    for (new, old) in anchors {
        for (old_index, new_index) in (old_end..old).zip(next_end..new) {
            matched[new_index] = Some(old_index);
        }
        old_end = old + 1;
        next_end = new + 1;
    }
    let mut previous: Vec<_> = previous.into_iter().map(Some).collect();
    let mut delay = Duration::ZERO;
    next.into_iter()
        .zip(matched)
        .map(|((index, (text, draft)), matched)| {
            if let Some(mut word) = matched.and_then(|index| previous[index].take()) {
                word.index = index;
                if word.text != text {
                    word.text = text.to_owned();
                    word.morph.retarget(text);
                }
                word.ink.aim(if draft { 0. } else { 1. }, INK);
                return word;
            }
            let mut ink = Spring::new(INK);
            ink.aim(if draft { 0. } else { 1. }, INK);
            let mut reveal = Spring::new(REVEAL);
            reveal.aim(1., REVEAL);
            let word = Word {
                index,
                text: text.to_owned(),
                morph: Morph::new(text),
                reveal,
                x: Spring::new(REFLOW),
                y: Spring::new(REFLOW),
                placed: false,
                ink,
                delay,
            };
            delay = (delay + WORD_STAGGER).min(MAX_STAGGER);
            word
        })
        .collect()
}

fn word_key(text: &str) -> &str {
    let key = text.trim_end_matches(|ch: char| {
        ch.is_ascii_punctuation()
            || matches!(
                ch,
                '…' | '。'
                    | '、'
                    | '，'
                    | '！'
                    | '？'
                    | '：'
                    | '；'
                    | '»'
                    | '”'
                    | '’'
                    | '）'
                    | '】'
                    | '」'
                    | '』'
            )
    });
    if key.is_empty() { text } else { key }
}

fn blend(from: Hsla, to: Hsla, progress: f32) -> Hsla {
    let from = from.to_rgb();
    let to = to.to_rgb();
    gpui::Rgba {
        r: from.r + (to.r - from.r) * progress,
        g: from.g + (to.g - from.g) * progress,
        b: from.b + (to.b - from.b) * progress,
        a: from.a + (to.a - from.a) * progress,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_keeps_the_fraction_that_font_rasterization_would_discard() {
        for scale in [1., 1.25, 1.5, 2.] {
            for offset in [0., 0.1, 0.5, 1.2, 23.9, 24.] {
                let (snapped, translation) = scroll_position(px(offset), scale);
                assert!(((-snapped + translation).as_f32() + offset).abs() < 0.00001);
                assert!(translation.as_f32().abs() <= 0.5 / scale + 0.00001);
            }
        }
    }

    #[test]
    fn only_new_words_start_blurred_and_punctuation_keeps_the_existing_reveal() {
        let mut words = reconcile(Vec::new(), "", "Привет");
        words[0].reveal.finish();
        words[0].morph.finish();
        let words = reconcile(words, "Привет,", "мир");
        assert_eq!(words[0].reveal.at(), 1.);
        assert!(words[0].reveal.settled());
        assert_eq!(words[1].reveal.at(), 0.);
        assert!(!words[1].reveal.settled());
    }

    #[test]
    fn inserting_a_word_does_not_restart_the_words_after_it() {
        let mut words = reconcile(Vec::new(), "", "Мы любим Rust");
        for word in &mut words {
            word.reveal.finish();
            word.morph.finish();
        }
        let words = reconcile(words, "", "Мы, очень любим Rust.");
        assert_eq!(words[1].text, "очень");
        assert_eq!(words[1].reveal.at(), 0.);
        for index in [0, 2, 3] {
            assert!(words[index].reveal.settled());
            assert_eq!(words[index].reveal.at(), 1.);
        }
        assert!(words[2].morph.settled());
        assert_eq!(words[2].index, 2);
    }

    #[test]
    fn wrapping_uses_the_full_available_width_before_adding_a_line() {
        let metrics = [
            Size::new(px(100.), LINE_HEIGHT),
            Size::new(px(140.), LINE_HEIGHT),
        ];
        let (positions, height) = flow_layout(&metrics, px(245.));
        assert_eq!(height, LINE_HEIGHT);
        assert_eq!(positions[1], point(px(105.), px(0.)));
        let (positions, height) = flow_layout(&metrics, px(244.));
        assert_eq!(height, LINE_HEIGHT * 2.);
        assert_eq!(positions[1], point(px(0.), LINE_HEIGHT));
    }

    #[test]
    fn overflowing_lines_scroll_without_jumping_or_restarting_the_motion() {
        let mut caption = Caption::new(PillEdge::Bottom);
        caption.resize(Size::new(px(478.), TEXT_HEIGHT));
        caption.resize(Size::new(px(478.), TEXT_HEIGHT + LINE_HEIGHT));
        assert_eq!(caption.scroll.at(), 0.);
        assert_eq!(caption.scroll.target, LINE_HEIGHT.as_f32());
        caption.beat = Some(Instant::now() - Duration::from_millis(50));
        assert!(caption.step(false));
        assert!(caption.scroll.at() > 0. && caption.scroll.at() < caption.scroll.target);

        let position = caption.scroll.at();
        let velocity = caption.scroll.state.velocity;
        caption.resize(Size::new(px(478.), TEXT_HEIGHT + LINE_HEIGHT * 2.));
        assert_eq!(caption.scroll.at(), position);
        assert_eq!(caption.scroll.state.velocity, velocity);
        for _ in 0..180 {
            let previous = caption.scroll.at();
            caption.beat = Some(Instant::now() - Duration::from_millis(16));
            caption.step(false);
            assert!(
                caption.scroll.at() >= previous && caption.scroll.at() <= caption.scroll.target
            );
        }
        assert_eq!(caption.scroll.at(), LINE_HEIGHT.as_f32() * 2.);
        assert!(!caption.step(false));
        caption.resize(Size::new(px(478.), TEXT_HEIGHT + LINE_HEIGHT * 3.));
        assert!(!caption.step(true));
        assert_eq!(caption.scroll.at(), caption.scroll.target);
        assert_eq!(caption.scroll.state.velocity, 0.);
    }

    #[test]
    fn removing_old_rows_preserves_visible_positions_and_scroll_velocity() {
        let text = (0..137)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut caption = Caption::new(PillEdge::Top);
        caption.words = reconcile(Vec::new(), &text, "");
        // Three words per row at this width: 46 rows, with the last three visible.
        caption.resize(Size::new(px(40.), LINE_HEIGHT * 46.));
        caption.scroll.state.velocity = 10.;
        let visible_y = LINE_HEIGHT * 44. - px(caption.scroll.at());
        let metrics = vec![Size::new(px(10.), LINE_HEIGHT); 137];
        caption.trim_history(&metrics, px(40.));
        assert_eq!(caption.words.len(), MAX_WORDS);
        assert_eq!(caption.words[0].index, 9);
        assert_eq!(LINE_HEIGHT * 41. - px(caption.scroll.at()), visible_y);
        assert_eq!(caption.scroll.state.velocity, 10.);
        assert_eq!(caption.scroll.at(), caption.scroll.target);
    }

    #[test]
    fn history_trimming_never_splits_a_row_or_removes_visible_content() {
        let metrics = vec![Size::new(px(10.), LINE_HEIGHT); 137];
        assert_eq!(
            history_prefix(&metrics, px(40.), px(100.), 7),
            (6, LINE_HEIGHT * 2.)
        );
        assert_eq!(
            history_prefix(&metrics, px(40.), px(36.), 9),
            (3, LINE_HEIGHT)
        );
        // A single long word can span multiple text lines.
        let metrics = [
            Size::new(px(40.), LINE_HEIGHT * 2.),
            Size::new(px(10.), LINE_HEIGHT),
        ];
        assert_eq!(
            history_prefix(&metrics, px(40.), LINE_HEIGHT, 1),
            (0, px(0.))
        );
        assert_eq!(
            history_prefix(&metrics, px(40.), LINE_HEIGHT * 2., 1),
            (1, LINE_HEIGHT * 2.)
        );
    }

    #[test]
    fn resizing_preserves_dimensions_and_velocity_when_new_text_arrives() {
        let mut caption = Caption::new(PillEdge::Bottom);
        caption.resize(Size::new(px(100.), LINE_HEIGHT));
        let width = caption.width.at();
        let height = caption.height.at();
        caption.resize(Size::new(px(470.), TEXT_HEIGHT));
        assert_eq!(caption.width.at(), width);
        assert_eq!(caption.height.at(), height);
        caption.beat = Some(Instant::now() - Duration::from_millis(50));
        assert!(caption.step(false));
        assert!(caption.width.at() > width && caption.width.at() < caption.width.target);
        assert!(caption.height.at() > height && caption.height.at() < caption.height.target);

        let width = caption.width.at();
        let velocity = caption.width.state.velocity;
        caption.resize(Size::new(px(300.), LINE_HEIGHT * 2.));
        assert_eq!(caption.width.at(), width);
        assert_eq!(caption.width.state.velocity, velocity);
        for _ in 0..180 {
            caption.beat = Some(Instant::now() - Duration::from_millis(16));
            caption.step(false);
        }
        assert_eq!(caption.width.at(), caption.width.target);
        assert_eq!(caption.height.at(), caption.height.target);
        assert!(!caption.step(false));
    }

    #[test]
    fn reduced_motion_finishes_a_resize_without_remaining_velocity() {
        let mut caption = Caption::new(PillEdge::Top);
        caption.resize(Size::new(px(100.), LINE_HEIGHT));
        caption.resize(Size::new(px(470.), TEXT_HEIGHT));
        caption.beat = Some(Instant::now() - Duration::from_millis(50));
        caption.step(false);
        assert!(!caption.step(true));
        assert_eq!(caption.width.at(), caption.width.target);
        assert_eq!(caption.height.at(), caption.height.target);
        assert_eq!(caption.width.state.velocity, 0.);
        assert_eq!(caption.height.state.velocity, 0.);
    }

    #[test]
    fn committing_a_draft_changes_only_its_color_target() {
        let mut words = reconcile(Vec::new(), "", "Привет мир");
        for word in &mut words {
            word.morph.finish();
        }
        let mut words = reconcile(words, "Привет мир", "");
        for word in &mut words {
            assert!(word.morph.settled());
            assert_eq!(word.ink.at(), 0.);
            assert_eq!(word.ink.target, 1.);
            word.ink.step(Duration::from_millis(100));
            assert!(word.ink.at() > 0. && word.ink.at() < 1.);
        }
    }

    #[test]
    fn revising_a_word_keeps_the_other_words_and_their_color_in_motion() {
        let mut words = reconcile(Vec::new(), "One", "small step");
        for word in &mut words {
            word.morph.finish();
        }
        words[0].ink.step(Duration::from_millis(100));
        let color = words[0].ink.at();
        let velocity = words[0].ink.state.velocity;
        let words = reconcile(words, "One", "giant step");
        assert_eq!(words[0].ink.at(), color);
        assert_eq!(words[0].ink.state.velocity, velocity);
        assert!(!words[1].morph.settled());
        assert!(words[2].morph.settled());
    }

    #[test]
    fn color_and_scroll_approach_their_endpoints_without_reversing() {
        for config in [SCROLL, INK] {
            for hz in [30, 60, 120, 144] {
                let mut spring = Spring::new(config);
                spring.aim(1., config);
                for _ in 0..hz * 3 {
                    let before = spring.at();
                    spring.step(Duration::from_secs_f32(1. / hz as f32));
                    assert!(spring.at() >= before && spring.at() <= 1.);
                }
                assert_eq!(spring.at(), 1.);
            }
        }
    }

    #[test]
    fn history_is_kept_until_complete_offscreen_rows_can_be_removed() {
        let text = (0..MAX_WORDS)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut words = reconcile(Vec::new(), &text, "");
        for word in &mut words {
            word.morph.finish();
        }
        let words = reconcile(words, &text, "new");
        assert_eq!(words.len(), MAX_WORDS + 1);
        assert_eq!(words[0].index, 0);
        assert!(words[0].morph.settled());
        assert_eq!(words.last().unwrap().text, "new");
        assert!(!words.last().unwrap().morph.settled());
    }
}
