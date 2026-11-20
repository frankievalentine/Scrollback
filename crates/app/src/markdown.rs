//! Markdown for the transcript: agents write their replies in it, so it is
//! parsed into a small tree and drawn with headings, lists, code, tables and
//! links instead of being shown as raw text.

use crate::theme::{text, Theme, MONO_FONT, UI_FONT};
use gpui::{
    div, prelude::*, px, AnyElement, Div, Font, FontStyle, FontWeight, Hsla, InteractiveText,
    SharedString, StrikethroughStyle, StyledText, TextRun, UnderlineStyle,
};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::iter::Peekable;
use std::ops::Range;

/// A run of text with its inline styling.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    /// Consecutive stretches of `text`, covering all of it.
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    /// Length in bytes.
    pub len: usize,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Paragraph(Inline),
    Heading(u8, Inline),
    Code(String),
    /// `Some(n)` for a numbered list starting at `n`.
    List(Option<u64>, Vec<Vec<Node>>),
    Quote(Vec<Node>),
    Table(Vec<Inline>, Vec<Vec<Inline>>),
    Rule,
}

/// The style in force while inline text is collected.
#[derive(Default)]
struct Style {
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<String>,
}

impl Inline {
    fn push(&mut self, text: &str, style: &Style, code: bool) {
        if text.is_empty() {
            return;
        }
        let span = Span {
            len: text.len(),
            bold: style.bold > 0,
            italic: style.italic > 0,
            code,
            strike: style.strike > 0,
            link: style.links.last().cloned(),
        };
        self.text.push_str(text);
        match self.spans.last_mut() {
            // Neighbouring text in the same style is one span.
            Some(last) if Span { len: last.len, ..span.clone() } == *last => last.len += span.len,
            _ => self.spans.push(span),
        }
    }

    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

pub fn parse(source: &str) -> Vec<Node> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut events = Parser::new_ext(source, options).peekable();
    blocks(&mut events)
}

fn is_inline_start(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
    )
}

/// Blocks up to the end of the enclosing container, which is consumed.
fn blocks<'a>(events: &mut Peekable<Parser<'a>>) -> Vec<Node> {
    let mut out = Vec::new();
    loop {
        let inline_next = match events.peek() {
            None => return out,
            Some(Event::Start(tag)) => is_inline_start(tag),
            Some(Event::End(_) | Event::Rule) => false,
            // Text with no paragraph around it: an item of a tight list.
            Some(_) => true,
        };
        if inline_next {
            let text = inline(events, false);
            if !text.is_blank() {
                out.push(Node::Paragraph(text));
            }
            continue;
        }
        match events.next() {
            None | Some(Event::End(_)) => return out,
            Some(Event::Rule) => out.push(Node::Rule),
            Some(Event::Start(tag)) => match tag {
                Tag::Paragraph => {
                    let text = inline(events, true);
                    if !text.is_blank() {
                        out.push(Node::Paragraph(text));
                    }
                }
                Tag::Heading { level, .. } => out.push(Node::Heading(level as u8, inline(events, true))),
                Tag::CodeBlock(kind) => {
                    let mut code = String::new();
                    for event in events.by_ref() {
                        match event {
                            Event::Text(text) => code.push_str(&text),
                            Event::End(TagEnd::CodeBlock) => break,
                            _ => {}
                        }
                    }
                    // A fenced block's own trailing newline is not content.
                    if matches!(kind, CodeBlockKind::Fenced(_) | CodeBlockKind::Indented) {
                        while code.ends_with('\n') {
                            code.pop();
                        }
                    }
                    out.push(Node::Code(code));
                }
                Tag::List(start) => {
                    let mut items = Vec::new();
                    loop {
                        match events.next() {
                            Some(Event::Start(Tag::Item)) => items.push(blocks(events)),
                            None | Some(Event::End(_)) => break,
                            Some(_) => {}
                        }
                    }
                    out.push(Node::List(start, items));
                }
                Tag::BlockQuote(_) => out.push(Node::Quote(blocks(events))),
                Tag::Table(_) => out.push(table(events)),
                // Anything else (raw HTML, footnotes, metadata) is read for
                // whatever ordinary blocks it holds.
                _ => out.extend(blocks(events)),
            },
            Some(_) => {}
        }
    }
}

fn table<'a>(events: &mut Peekable<Parser<'a>>) -> Node {
    let mut head = Vec::new();
    let mut rows: Vec<Vec<Inline>> = Vec::new();
    let mut in_head = false;
    loop {
        match events.next() {
            None | Some(Event::End(TagEnd::Table)) => break,
            Some(Event::Start(Tag::TableHead)) => in_head = true,
            Some(Event::End(TagEnd::TableHead)) => in_head = false,
            Some(Event::Start(Tag::TableRow)) => rows.push(Vec::new()),
            Some(Event::Start(Tag::TableCell)) => {
                let cell = inline(events, true);
                match (in_head, rows.last_mut()) {
                    (true, _) | (false, None) => head.push(cell),
                    (false, Some(row)) => row.push(cell),
                }
            }
            Some(_) => {}
        }
    }
    Node::Table(head, rows)
}

/// Inline text up to the end of its block. With `enclosed`, that end tag is
/// consumed; without, the text has no tag of its own and reading stops in
/// front of whatever block-level event comes next.
fn inline<'a>(events: &mut Peekable<Parser<'a>>, enclosed: bool) -> Inline {
    let mut out = Inline::default();
    let mut style = Style::default();
    let mut depth = 0usize;
    loop {
        if depth == 0 && !enclosed {
            let stop = match events.peek() {
                None | Some(Event::End(_) | Event::Rule) => true,
                Some(Event::Start(tag)) => !is_inline_start(tag),
                Some(_) => false,
            };
            if stop {
                return out;
            }
        }
        let Some(event) = events.next() else { return out };
        match event {
            Event::Text(text) | Event::InlineHtml(text) | Event::Html(text) => {
                out.push(&text, &style, false)
            }
            Event::Code(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                out.push(&text, &style, true)
            }
            Event::SoftBreak => out.push(" ", &style, false),
            Event::HardBreak => out.push("\n", &style, false),
            Event::TaskListMarker(done) => out.push(if done { "☑ " } else { "☐ " }, &style, false),
            Event::FootnoteReference(name) => out.push(&format!("[{name}]"), &style, false),
            Event::Rule => {}
            Event::Start(tag) => {
                depth += 1;
                match tag {
                    Tag::Strong => style.bold += 1,
                    Tag::Emphasis => style.italic += 1,
                    Tag::Strikethrough => style.strike += 1,
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        style.links.push(dest_url.to_string())
                    }
                    _ => {}
                }
            }
            Event::End(end) => {
                if depth == 0 {
                    return out;
                }
                depth -= 1;
                match end {
                    TagEnd::Strong => style.bold = style.bold.saturating_sub(1),
                    TagEnd::Emphasis => style.italic = style.italic.saturating_sub(1),
                    TagEnd::Strikethrough => style.strike = style.strike.saturating_sub(1),
                    TagEnd::Link | TagEnd::Image => {
                        style.links.pop();
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Hands out element ids for linked text, unique within one rendered reply.
struct Ids {
    seed: usize,
    next: usize,
}

/// Draw parsed markdown as a column. `seed` must differ between replies shown
/// at once, so that links in each can be told apart.
pub fn render(nodes: &[Node], t: &Theme, color: Hsla, seed: usize) -> Div {
    let mut ids = Ids { seed, next: 0 };
    column(nodes, t, color, &mut ids)
}

fn column(nodes: &[Node], t: &Theme, color: Hsla, ids: &mut Ids) -> Div {
    let mut out = div().flex().flex_col().gap(px(10.)).min_w(px(0.));
    for node in nodes {
        out = out.child(match node {
            Node::Paragraph(text) => rich(text, t, color, FontWeight::NORMAL, ids),
            Node::Heading(level, text) => {
                let size = match level {
                    1 => text::TITLE,
                    2 => text::LEAD,
                    _ => text::BODY,
                };
                div()
                    .pt(px(6.))
                    .text_size(px(size))
                    .line_height(px(size + 8.))
                    .child(rich(text, t, t.ink, FontWeight::SEMIBOLD, ids))
                    .into_any_element()
            }
            Node::Code(code) => div()
                .px(px(12.))
                .py(px(10.))
                .rounded(px(8.))
                .bg(t.fill.opacity(0.6))
                .border_1()
                .border_color(t.line)
                .font_family(MONO_FONT)
                .text_size(px(text::CAPTION))
                .line_height(px(18.))
                .text_color(t.ink)
                .child(SharedString::from(code.clone()))
                .into_any_element(),
            Node::List(start, items) => {
                let mut list = div().flex().flex_col().gap(px(5.));
                for (i, item) in items.iter().enumerate() {
                    let marker = match start {
                        Some(first) => format!("{}.", first + i as u64),
                        None => "•".to_string(),
                    };
                    list = list.child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w(px(if start.is_some() { 22. } else { 12. }))
                                    .flex_shrink_0()
                                    .text_color(t.ink3)
                                    .child(marker),
                            )
                            .child(column(item, t, color, ids).flex_1().gap(px(5.))),
                    );
                }
                list.into_any_element()
            }
            Node::Quote(inner) => div()
                .pl(px(12.))
                .border_l_2()
                .border_color(t.line)
                .child(column(inner, t, t.ink3, ids))
                .into_any_element(),
            Node::Table(head, rows) => {
                let row = |cells: &[Inline], weight: FontWeight, ids: &mut Ids| {
                    let mut line = div().flex().gap(px(16.)).py(px(6.)).border_b_1().border_color(t.line);
                    for cell in cells {
                        line = line.child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(rich(cell, t, color, weight, ids)),
                        );
                    }
                    line
                };
                let mut table = div().flex().flex_col().text_size(px(text::SMALL)).line_height(px(19.));
                if !head.is_empty() {
                    table = table.child(row(head, FontWeight::SEMIBOLD, ids));
                }
                for cells in rows {
                    table = table.child(row(cells, FontWeight::NORMAL, ids));
                }
                table.into_any_element()
            }
            Node::Rule => div().h(px(1.)).my(px(4.)).bg(t.line).into_any_element(),
        });
    }
    out
}

/// One piece of styled text. Links open in the browser when clicked.
fn rich(text: &Inline, t: &Theme, color: Hsla, weight: FontWeight, ids: &mut Ids) -> AnyElement {
    if text.text.is_empty() {
        return div().into_any_element();
    }
    let mut runs = Vec::with_capacity(text.spans.len());
    let mut links: Vec<(Range<usize>, String)> = Vec::new();
    let mut at = 0;
    for span in &text.spans {
        runs.push(TextRun {
            len: span.len,
            font: Font {
                family: if span.code { MONO_FONT } else { UI_FONT }.into(),
                features: Default::default(),
                fallbacks: None,
                weight: if span.bold { FontWeight::BOLD } else { weight },
                style: if span.italic { FontStyle::Italic } else { FontStyle::Normal },
            },
            color: match (&span.link, span.code) {
                (Some(_), _) => t.accent,
                (None, true) => t.ink,
                (None, false) => color,
            },
            background_color: span.code.then_some(t.fill),
            underline: span.link.as_ref().map(|_| UnderlineStyle {
                thickness: px(1.),
                color: Some(t.accent.opacity(0.5)),
                wavy: false,
            }),
            strikethrough: span.strike.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        });
        if let Some(url) = &span.link {
            links.push((at..at + span.len, url.clone()));
        }
        at += span.len;
    }
    let styled = StyledText::new(SharedString::from(text.text.clone())).with_runs(runs);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let id = ids.seed * 4096 + ids.next;
    ids.next += 1;
    let (ranges, urls): (Vec<_>, Vec<_>) = links.into_iter().unzip();
    InteractiveText::new(("markdown-link", id), styled)
        .on_click(ranges, move |i, _, cx| {
            // Only web links are opened; a reply cannot launch anything else.
            if let Some(url) = urls.get(i).filter(|u| u.starts_with("https://") || u.starts_with("http://")) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Inline {
        Inline {
            text: text.to_string(),
            spans: vec![Span { len: text.len(), ..Default::default() }],
        }
    }

    /// Every span list must cover its text exactly, or drawing it panics.
    fn check(nodes: &[Node]) {
        let covered = |inline: &Inline| {
            assert_eq!(inline.spans.iter().map(|s| s.len).sum::<usize>(), inline.text.len());
            let mut at = 0;
            for span in &inline.spans {
                assert!(inline.text.is_char_boundary(at));
                at += span.len;
            }
        };
        for node in nodes {
            match node {
                Node::Paragraph(text) | Node::Heading(_, text) => covered(text),
                Node::List(_, items) => items.iter().for_each(|item| check(item)),
                Node::Quote(inner) => check(inner),
                Node::Table(head, rows) => {
                    head.iter().for_each(covered);
                    rows.iter().flatten().for_each(covered);
                }
                Node::Code(_) | Node::Rule => {}
            }
        }
    }

    #[test]
    fn inline_styles_become_spans() {
        let nodes = parse("Plain **bold** and *it* with `code` and [a link](https://example.com).");
        check(&nodes);
        let [Node::Paragraph(text)] = nodes.as_slice() else {
            panic!("expected one paragraph, got {nodes:?}");
        };
        assert_eq!(text.text, "Plain bold and it with code and a link.");
        let styled: Vec<(&str, bool, bool, bool, bool)> = {
            let mut at = 0;
            text.spans
                .iter()
                .map(|s| {
                    let piece = &text.text[at..at + s.len];
                    at += s.len;
                    (piece, s.bold, s.italic, s.code, s.link.is_some())
                })
                .collect()
        };
        assert_eq!(
            styled,
            [
                ("Plain ", false, false, false, false),
                ("bold", true, false, false, false),
                (" and ", false, false, false, false),
                ("it", false, true, false, false),
                (" with ", false, false, false, false),
                ("code", false, false, true, false),
                (" and ", false, false, false, false),
                ("a link", false, false, false, true),
                (".", false, false, false, false),
            ]
        );
    }

    #[test]
    fn headings_code_and_rules() {
        let nodes = parse("## Key changes\n\n```bash\ndocker restart hermes\n```\n\n---\n\nDone.");
        check(&nodes);
        assert_eq!(
            nodes,
            [
                Node::Heading(2, plain("Key changes")),
                Node::Code("docker restart hermes".to_string()),
                Node::Rule,
                Node::Paragraph(plain("Done.")),
            ]
        );
    }

    #[test]
    fn tight_loose_and_nested_lists() {
        let nodes = parse("1. first\n2. second\n   - inner a\n   - inner b\n\n- loose one\n\n- loose two");
        check(&nodes);
        assert_eq!(
            nodes,
            [
                Node::List(
                    Some(1),
                    vec![
                        vec![Node::Paragraph(plain("first"))],
                        vec![
                            Node::Paragraph(plain("second")),
                            Node::List(
                                None,
                                vec![
                                    vec![Node::Paragraph(plain("inner a"))],
                                    vec![Node::Paragraph(plain("inner b"))],
                                ]
                            ),
                        ],
                    ]
                ),
                Node::List(
                    None,
                    vec![
                        vec![Node::Paragraph(plain("loose one"))],
                        vec![Node::Paragraph(plain("loose two"))],
                    ]
                ),
            ]
        );
    }

    #[test]
    fn tables_quotes_and_task_lists() {
        let nodes = parse("| Tool | Count |\n|---|---|\n| read | 4 |\n\n> quoted\n\n- [x] done\n- [ ] todo");
        check(&nodes);
        assert_eq!(
            nodes,
            [
                Node::Table(
                    vec![plain("Tool"), plain("Count")],
                    vec![vec![plain("read"), plain("4")]]
                ),
                Node::Quote(vec![Node::Paragraph(plain("quoted"))]),
                Node::List(
                    None,
                    vec![
                        vec![Node::Paragraph(plain("☑ done"))],
                        vec![Node::Paragraph(plain("☐ todo"))],
                    ]
                ),
            ]
        );
    }

    #[test]
    fn odd_input_still_covers_its_text() {
        for source in [
            "",
            "**unclosed bold",
            "<details><summary>x</summary>\n\nbody\n\n</details>",
            "text with <b>inline html</b> and émojis 🎉 **ünïcode**",
            "```\nunclosed fence",
            "- item\n\n  ```rust\n  let x = 1;\n  ```\n\n  after",
            "line one  \nline two\\\nline three",
            "![alt text](image.png) and [^1]\n\n[^1]: a footnote",
        ] {
            check(&parse(source));
        }
    }
}
