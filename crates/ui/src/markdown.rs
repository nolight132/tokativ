//! A model card in Markdown, drawn with the app's text styles. It covers what model cards use:
//! headings, paragraphs, lists, block quotes, code, tables and rules, with bold, italic, code and
//! links inline. Images, raw HTML and the YAML front matter are left out.

use std::ops::Range;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, FontStyle, FontWeight, HighlightStyle, InteractiveText, SharedString,
    StyledText, div, px,
};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ui::{ActiveTheme as _, Text};

/// One block of a card, in reading order. Lists come out flat, each item knowing how deep it
/// sits.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(u8, Inline),
    Paragraph(Inline),
    Item {
        depth: usize,
        marker: String,
        text: Inline,
    },
    Quote(Inline),
    Code(String),
    Table {
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
}

/// A run of text with the ranges that are bold, italic, code or a link.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    marks: Vec<(Range<usize>, Mark)>,
    links: Vec<(Range<usize>, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Strong,
    Emphasis,
    Code,
}

impl Inline {
    fn push(&mut self, text: &str, strong: bool, emphasis: bool, code: bool) {
        let start = self.text.len();
        self.text.push_str(text);
        let range = start..self.text.len();
        for (on, mark) in [
            (strong, Mark::Strong),
            (emphasis, Mark::Emphasis),
            (code, Mark::Code),
        ] {
            if on {
                self.marks.push((range.clone(), mark));
            }
        }
    }

    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A table as it is read: the header, the rows done, and the cells of the row being read.
#[derive(Default)]
struct Table {
    head: Vec<Inline>,
    rows: Vec<Vec<Inline>>,
    cells: Vec<Inline>,
}

/// Where the parser is inside the card.
#[derive(Default)]
struct Reader {
    blocks: Vec<Block>,
    inline: Option<Inline>,
    heading: Option<u8>,
    /// Each open list, with the number its next item gets when it is ordered.
    lists: Vec<Option<u64>>,
    /// The marker of the item whose text is being read, and how deep it sits.
    item: Option<(usize, String)>,
    quotes: usize,
    code: Option<String>,
    table: Option<Table>,
    strong: usize,
    emphasis: usize,
    link: Option<(usize, String)>,
    /// Inside an image or the front matter, whose text is not shown.
    hidden: usize,
}

impl Reader {
    fn inline(&mut self) -> &mut Inline {
        self.inline.get_or_insert_with(Inline::default)
    }

    fn text(&mut self, text: &str, code: bool) {
        if self.hidden > 0 {
            return;
        }
        if let Some(block) = &mut self.code {
            block.push_str(text);
            return;
        }
        let (strong, emphasis) = (self.strong > 0, self.emphasis > 0);
        self.inline().push(text, strong, emphasis, code);
    }

    /// Ends the text being read as the block it belongs to.
    fn flush(&mut self) {
        let Some(inline) = self.inline.take().filter(|inline| !inline.is_blank()) else {
            return;
        };
        let block = match (&self.item, self.heading, self.quotes) {
            (_, Some(level), _) => Block::Heading(level, inline),
            (Some((depth, marker)), _, _) => Block::Item {
                depth: *depth,
                marker: marker.clone(),
                text: inline,
            },
            (None, None, 0) => Block::Paragraph(inline),
            (None, None, _) => Block::Quote(inline),
        };
        self.blocks.push(block);
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text, false),
            Event::Code(text) => self.text(&text, true),
            Event::SoftBreak => self.text(" ", false),
            Event::HardBreak => self.text("\n", false),
            Event::Rule => {
                self.flush();
                self.blocks.push(Block::Rule);
            }
            Event::TaskListMarker(done) => self.text(if done { "☑ " } else { "☐ " }, false),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    _ => 4,
                });
            }
            Tag::Paragraph => {
                // A loose list item wraps its text in paragraphs, which stay one item.
                if self.item.is_none() {
                    self.flush();
                } else if self
                    .inline
                    .as_ref()
                    .is_some_and(|inline| !inline.is_blank())
                {
                    self.text(" ", false);
                }
            }
            Tag::List(start) => {
                self.flush();
                self.item = None;
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}.");
                        *number += 1;
                        marker
                    }
                    _ => "•".to_owned(),
                };
                self.item = Some((depth, marker));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quotes += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(Table::default());
            }
            Tag::TableCell => self.inline = Some(Inline::default()),
            Tag::Strong => self.strong += 1,
            Tag::Emphasis => self.emphasis += 1,
            Tag::Link { dest_url, .. } => {
                let start = self.inline().text.len();
                self.link = Some((start, dest_url.into_string()));
            }
            Tag::Image { .. } | Tag::MetadataBlock(_) | Tag::HtmlBlock => self.hidden += 1,
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::Paragraph => {
                if self.item.is_none() {
                    self.flush();
                }
            }
            TagEnd::Item => {
                self.flush();
                self.item = None;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.item = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    self.blocks.push(Block::Code(code.trim_end().to_owned()));
                }
            }
            TagEnd::TableCell => {
                let cell = self.inline.take().unwrap_or_default();
                if let Some(table) = &mut self.table {
                    table.cells.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.head = std::mem::take(&mut table.cells);
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.cells);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(Table { head, rows, .. }) = self.table.take() {
                    self.blocks.push(Block::Table { head, rows });
                }
            }
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Link => {
                if let Some((start, url)) = self.link.take() {
                    let inline = self.inline();
                    let end = inline.text.len();
                    if end > start {
                        inline.links.push((start..end, url));
                    }
                }
            }
            TagEnd::Image | TagEnd::MetadataBlock(_) | TagEnd::HtmlBlock => {
                self.hidden = self.hidden.saturating_sub(1)
            }
            _ => {}
        }
    }
}

/// Reads a model card into blocks.
pub fn parse(markdown: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut reader = Reader::default();
    for event in Parser::new_ext(markdown, options) {
        reader.event(event);
    }
    reader.flush();
    reader.blocks
}

/// Draws `blocks`. Links open in the browser, relative ones against `base`, the repository's
/// page on Hugging Face.
pub fn render(blocks: &[Block], base: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let small = theme.text(Text::Small);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .text_size(small)
        .children(blocks.iter().enumerate().map(|(index, block)| {
            let id = |part: usize| SharedString::from(format!("card-{index}-{part}"));
            match block {
                Block::Heading(level, text) => div()
                    .pt(match level {
                        1 | 2 => px(10.),
                        _ => px(6.),
                    })
                    .text_size(match level {
                        1 => theme.text(Text::Large),
                        2 => theme.text(Text::Body),
                        _ => small,
                    })
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(inline(id(0), text, base, cx))
                    .into_any_element(),
                Block::Paragraph(text) => div()
                    .text_color(theme.foreground.opacity(0.85))
                    .child(inline(id(0), text, base, cx))
                    .into_any_element(),
                Block::Item {
                    depth,
                    marker,
                    text,
                } => div()
                    .flex()
                    .gap_2()
                    .pl(px(14. * *depth as f32))
                    .text_color(theme.foreground.opacity(0.85))
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(14.))
                            .text_color(theme.muted_foreground)
                            .child(marker.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(inline(id(0), text, base, cx)),
                    )
                    .into_any_element(),
                Block::Quote(text) => div()
                    .pl_3()
                    .border_l_2()
                    .border_color(theme.border)
                    .text_color(theme.muted_foreground)
                    .child(inline(id(0), text, base, cx))
                    .into_any_element(),
                Block::Code(code) => div()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .squircle()
                    .bg(theme.secondary.opacity(0.6))
                    .font_family("monospace")
                    .text_size(theme.text(Text::Tiny))
                    .overflow_hidden()
                    .child(code.clone())
                    .into_any_element(),
                Block::Table { head, rows } => {
                    let row = |cells: &[Inline], header: bool, at: usize| {
                        div()
                            .flex()
                            .gap_3()
                            .py_1()
                            .border_b_1()
                            .border_color(theme.border.opacity(0.5))
                            .when(header, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .when(!header, |this| {
                                this.text_color(theme.foreground.opacity(0.85))
                            })
                            .children(cells.iter().enumerate().map(|(column, cell)| {
                                div().flex_1().min_w_0().child(inline(
                                    id(at * 64 + column),
                                    cell,
                                    base,
                                    cx,
                                ))
                            }))
                    };
                    div()
                        .flex()
                        .flex_col()
                        .when(!head.is_empty(), |this| this.child(row(head, true, 0)))
                        .children(
                            rows.iter()
                                .enumerate()
                                .map(|(at, cells)| row(cells, false, at + 1)),
                        )
                        .into_any_element()
                }
                Block::Rule => div()
                    .h(px(1.))
                    .my_1()
                    .bg(theme.border.opacity(0.6))
                    .into_any_element(),
            }
        }))
        .into_any_element()
}

/// One run of text with its marks, its links underlined and clickable.
fn inline(id: SharedString, text: &Inline, base: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = text
        .marks
        .iter()
        .map(|(range, mark)| {
            let style = match mark {
                Mark::Strong => HighlightStyle {
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..Default::default()
                },
                Mark::Emphasis => HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
                Mark::Code => HighlightStyle {
                    background_color: Some(theme.secondary),
                    ..Default::default()
                },
            };
            (range.clone(), style)
        })
        .collect();
    highlights.extend(text.links.iter().map(|(range, _)| {
        (
            range.clone(),
            HighlightStyle {
                color: Some(theme.foreground),
                underline: Some(gpui::UnderlineStyle {
                    thickness: px(1.),
                    color: Some(theme.muted_foreground),
                    wavy: false,
                }),
                ..Default::default()
            },
        )
    }));
    highlights = flatten(highlights);
    let styled = StyledText::new(text.text.clone()).with_highlights(highlights);
    if text.links.is_empty() {
        return styled.into_any_element();
    }
    let urls: Vec<String> = text
        .links
        .iter()
        .map(|(_, url)| absolute(url, base))
        .collect();
    let ranges = text.links.iter().map(|(range, _)| range.clone()).collect();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |index, _, cx| {
            if let Some(url) = urls.get(index) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

/// Highlights that do not overlap, each overlap carrying the styles of every highlight over it,
/// since text runs cannot overlap.
fn flatten(highlights: Vec<(Range<usize>, HighlightStyle)>) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut edges: Vec<usize> = highlights
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges
        .windows(2)
        .filter_map(|pair| {
            let piece = pair[0]..pair[1];
            let mut style: Option<HighlightStyle> = None;
            for (range, each) in &highlights {
                if range.start <= piece.start && piece.end <= range.end {
                    style = Some(match style {
                        Some(style) => style.highlight(*each),
                        None => *each,
                    });
                }
            }
            style.map(|style| (piece, style))
        })
        .collect()
}

/// A link as a full URL: anchors and relative paths resolve against the repository's page.
fn absolute(url: &str, base: &str) -> String {
    if url.contains("://") || url.starts_with("mailto:") {
        return url.to_owned();
    }
    let base = base.trim_end_matches('/');
    match url.strip_prefix('/') {
        Some(rooted) => format!("https://huggingface.co/{rooted}"),
        None if url.starts_with('#') => format!("{base}{url}"),
        None => format!("{base}/blob/main/{url}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_reads_into_blocks() {
        let card = "---\nlicense: mit\n---\n# Title\n\nSome **bold** and `code`, see [docs](README.md).\n\n- one\n- two\n  1. nested\n\n| Q | Size |\n| --- | --- |\n| Q8_0 | 506 MB |\n\n![logo](logo.png)\n";
        let blocks = parse(card);
        let texts: Vec<String> = blocks
            .iter()
            .map(|block| match block {
                Block::Heading(level, text) => format!("h{level} {}", text.text),
                Block::Paragraph(text) => format!("p {}", text.text),
                Block::Item {
                    depth,
                    marker,
                    text,
                } => format!("{depth}{marker} {}", text.text),
                Block::Table { head, rows } => format!(
                    "table {} / {}",
                    head.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    rows.iter()
                        .map(|row| row
                            .iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<Vec<_>>()
                            .join(","))
                        .collect::<Vec<_>>()
                        .join(";")
                ),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "h1 Title",
                "p Some bold and code, see docs.",
                "0• one",
                "0• two",
                "11. nested",
                "table Q,Size / Q8_0,506 MB",
            ]
        );
        let Block::Paragraph(text) = &blocks[1] else {
            panic!("a paragraph");
        };
        assert_eq!(text.links, [(24..28, "README.md".to_owned())]);
    }

    #[test]
    fn links_resolve_against_the_repository() {
        let base = "https://huggingface.co/handy-computer/x";
        assert_eq!(
            absolute("LICENSE", base),
            format!("{base}/blob/main/LICENSE")
        );
        assert_eq!(absolute("#usage", base), format!("{base}#usage"));
        assert_eq!(
            absolute("/moondream/parakeet-ultra", base),
            "https://huggingface.co/moondream/parakeet-ultra"
        );
        assert_eq!(absolute("https://a.b/c", base), "https://a.b/c");
    }
}
