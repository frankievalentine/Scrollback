//! The transcript view: one session read top to bottom, with each run of
//! tool calls folded into a single line that opens on click. When the
//! session's summary says where its steps begin, the transcript is cut into
//! those steps, each under a heading that opens and closes.

use super::{facts, outcome_mark, section, stats, Home, Marker};
use crate::markdown;
use crate::theme::{text, Theme, mono};
use crate::ui::{OneLine, Reachable};
use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Div, FontWeight, ListAlignment, ListOffset,
    ListState, Window,
};
use scrollback_core::transcript::{self, Block};
use scrollback_core::Summary;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

/// One stretch of the transcript under a heading.
struct Step {
    /// The step's number in the summary, or `None` for what came after the
    /// summary was written.
    number: Option<usize>,
    title: String,
    /// The blocks it covers.
    blocks: Range<usize>,
}

/// One row of the reader's list.
#[derive(Clone, Copy)]
enum Line {
    Head,
    /// Shown instead of blocks while the session is read, or if that failed.
    Status,
    Step(usize),
    Block(usize),
}

/// Finding text in the open transcript.
#[derive(Default)]
struct Find {
    query: String,
    /// Blocks holding the text, in order.
    matches: Vec<usize>,
    /// Which of them is showing.
    current: usize,
}

pub(super) struct Reading {
    /// Index into `Home::sessions`.
    pub index: usize,
    /// `None` while the session is being read from disk.
    blocks: Option<Vec<Block>>,
    error: Option<String>,
    /// The block a search result pointed at; shown first and marked.
    focus: Option<usize>,
    /// Tool blocks the reader has opened, by block index.
    expanded: HashSet<usize>,
    /// The transcript cut into the summary's steps. Empty when the summary
    /// does not say where its steps begin; the transcript is then one run.
    steps: Vec<Step>,
    /// Steps whose blocks are showing.
    open: HashSet<usize>,
    /// The step the bracket keys last moved to.
    cursor: Option<usize>,
    /// Replies already parsed as markdown, by block index, so scrolling
    /// does not parse them again on every frame.
    parsed: RefCell<HashMap<usize, Rc<Vec<markdown::Node>>>>,
    lines: Vec<Line>,
    list: ListState,
    /// The find field, while it is open.
    find: Option<Find>,
}

impl Reading {
    fn new(index: usize, focus: Option<usize>) -> Reading {
        Reading {
            index,
            blocks: None,
            error: None,
            focus,
            expanded: HashSet::new(),
            steps: Vec::new(),
            open: HashSet::new(),
            cursor: None,
            parsed: RefCell::new(HashMap::new()),
            lines: vec![Line::Head, Line::Status],
            list: ListState::new(2, ListAlignment::Top, px(1200.)),
            find: None,
        }
    }

    /// Cut the transcript into steps and lay the rows out again. The steps
    /// start closed, as an outline, because a first step can run to many
    /// screens. When a search led here, the step holding that message is open.
    fn group(&mut self, summary: Option<&Summary>) {
        let count = self.blocks.as_ref().map_or(0, Vec::len);
        self.steps = summary.map(|s| steps_of(s, count)).unwrap_or_default();
        self.open.clear();
        self.cursor = None;
        if !self.steps.is_empty() {
            let holding_focus = self
                .focus
                .and_then(|block| self.steps.iter().position(|step| step.blocks.contains(&block)));
            self.open.extend(holding_focus);
        }
        self.lay_out();
    }

    /// A reply's markdown, parsed the first time it is shown.
    fn markdown(&self, block: usize, text: &str) -> Rc<Vec<markdown::Node>> {
        self.parsed
            .borrow_mut()
            .entry(block)
            .or_insert_with(|| Rc::new(markdown::parse(text)))
            .clone()
    }

    fn lay_out(&mut self) {
        let mut lines = vec![Line::Head];
        match &self.blocks {
            None => lines.push(Line::Status),
            Some(blocks) if self.steps.is_empty() => {
                lines.extend((0..blocks.len()).map(Line::Block));
            }
            Some(_) => {
                for (i, step) in self.steps.iter().enumerate() {
                    lines.push(Line::Step(i));
                    if self.open.contains(&i) {
                        lines.extend(step.blocks.clone().map(Line::Block));
                    }
                }
            }
        }
        self.lines = lines;
        self.list.reset(self.lines.len());
    }

    fn line_of_step(&self, step: usize) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| matches!(line, Line::Step(i) if *i == step))
    }

    fn line_of_block(&self, block: usize) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| matches!(line, Line::Block(i) if *i == block))
    }

    /// The find field's state, for the development key-press log.
    pub(super) fn dev_state(&self) -> String {
        match &self.find {
            None => String::new(),
            Some(find) => format!(
                "find={:?} matches={} current={} focus={:?} ",
                find.query,
                find.matches.len(),
                find.current,
                self.focus
            ),
        }
    }

    /// Look for the find text again, and show the first match.
    fn search(&mut self) {
        let Some(find) = self.find.as_mut() else { return };
        let needle = find.query.trim().to_lowercase();
        find.matches.clear();
        find.current = 0;
        if !needle.is_empty() {
            if let Some(blocks) = &self.blocks {
                find.matches = blocks
                    .iter()
                    .enumerate()
                    .filter(|(_, block)| block_text(block).to_lowercase().contains(&needle))
                    .map(|(i, _)| i)
                    .collect();
            }
        }
        self.show_match();
    }

    /// Bring the current match into view, opening the step it is in, and
    /// mark it.
    fn show_match(&mut self) {
        let block = self.find.as_ref().and_then(|find| find.matches.get(find.current).copied());
        let previous = std::mem::replace(&mut self.focus, block);
        let Some(block) = block else {
            if let Some(line) = previous.and_then(|b| self.line_of_block(b)) {
                self.list.splice(line..line + 1, 1);
            }
            return;
        };
        if let Some(step) = self.steps.iter().position(|step| step.blocks.contains(&block)) {
            if !self.open.contains(&step) {
                self.toggle_step(step);
            }
        }
        for marked in [previous, Some(block)].into_iter().flatten() {
            if let Some(line) = self.line_of_block(marked) {
                self.list.splice(line..line + 1, 1);
            }
        }
        if let Some(line) = self.line_of_block(block) {
            self.list.scroll_to(ListOffset { item_ix: line, offset_in_item: px(0.) });
        }
    }

    /// Open or close one step, leaving the rest of the list where it is.
    fn toggle_step(&mut self, step: usize) {
        let (Some(at), Some(range)) = (
            self.line_of_step(step),
            self.steps.get(step).map(|s| s.blocks.clone()),
        ) else {
            return;
        };
        let first = at + 1;
        if self.open.remove(&step) {
            self.lines.drain(first..first + range.len());
            self.list.splice(first..first + range.len(), 0);
        } else {
            self.open.insert(step);
            let count = range.len();
            self.lines.splice(first..first, range.map(Line::Block));
            self.list.splice(first..first, count);
        }
        // The heading changes with it.
        self.list.splice(at..at + 1, 1);
    }
}

/// Where each of the summary's steps falls in a transcript of `count`
/// blocks. Blocks added since the summary was written get a step of their
/// own at the end. Empty when the summary does not place its steps.
fn steps_of(summary: &Summary, count: usize) -> Vec<Step> {
    let starts = &summary.step_starts;
    if count == 0 || starts.is_empty() || starts.len() != summary.happened.len() {
        return Vec::new();
    }
    // What the summary covered; everything, for one that did not record it.
    let covered = match summary.blocks_seen {
        0 => count,
        seen => seen.min(count),
    };
    let mut steps = Vec::new();
    let mut from = 0;
    for (i, title) in summary.happened.iter().enumerate() {
        let start = starts[i].min(covered).max(from);
        let end = starts
            .get(i + 1)
            .map_or(covered, |next| (*next).min(covered).max(start));
        steps.push(Step {
            number: Some(i + 1),
            title: title.clone(),
            blocks: start..end,
        });
        from = end;
    }
    if let Some(first) = steps.first_mut() {
        first.blocks.start = 0;
    }
    if covered < count {
        steps.push(Step {
            number: None,
            title: "Since this summary was written".to_string(),
            blocks: covered..count,
        });
    }
    steps
}

/// "3 prompts · 41 tool calls · 1 failed or interrupted"
fn step_facts(blocks: &[Block]) -> String {
    let (mut prompts, mut calls, mut notes) = (0, 0, 0);
    for block in blocks {
        match block {
            Block::Prompt(_) => prompts += 1,
            Block::Tools(list) => calls += list.len(),
            Block::Note(_) => notes += 1,
            Block::Reply(_) => {}
        }
    }
    let mut parts = Vec::new();
    match prompts {
        0 => {}
        1 => parts.push("1 prompt".to_string()),
        n => parts.push(format!("{n} prompts")),
    }
    match calls {
        0 => {}
        1 => parts.push("1 tool call".to_string()),
        n => parts.push(format!("{n} tool calls")),
    }
    if notes > 0 {
        parts.push(format!("{notes} failed or interrupted"));
    }
    if parts.is_empty() && !blocks.is_empty() {
        parts.push("replies only".to_string());
    }
    parts.join(" · ")
}

/// A block's words, for finding in.
fn block_text(block: &Block) -> String {
    match block {
        Block::Prompt(text) | Block::Reply(text) | Block::Note(text) => text.clone(),
        Block::Tools(calls) => calls
            .iter()
            .map(|(name, target)| format!("{name} {}", target.as_deref().unwrap_or("")))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn read_blocks(session: &scrollback_core::Session) -> anyhow::Result<Vec<Block>> {
    Ok(transcript::blocks(transcript::events(session)?))
}

impl Home {
    /// Open the selected session's transcript, reading it in the background.
    pub(super) fn open_transcript(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_session() else {
            return;
        };
        let session = self.sessions[index].clone();
        let focus = self.selected_message().map(|hit| hit.block);
        self.reading = Some(Reading::new(index, focus));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let work = cx
                .background_executor()
                .spawn(async move { read_blocks(&session) });
            let result = work.await;
            this.update(cx, |this, cx| this.transcript_arrived(index, result, cx))
                .ok();
        })
        .detach();
    }

    /// Open the transcript and read it before returning. Used only by the
    /// development launch option, so the first frame already shows it.
    pub(super) fn open_transcript_now(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_session() else {
            return;
        };
        let result = read_blocks(&self.sessions[index]);
        let focus = self.selected_message().map(|hit| hit.block);
        self.reading = Some(Reading::new(index, focus));
        self.transcript_arrived(index, result, cx);
    }

    fn transcript_arrived(
        &mut self,
        index: usize,
        result: anyhow::Result<Vec<Block>>,
        cx: &mut Context<Self>,
    ) {
        // The reader may have gone back, or opened another session, meanwhile.
        let Some(reading) = self.reading.as_mut().filter(|r| r.index == index) else {
            return;
        };
        match result {
            Ok(blocks) => {
                reading.blocks = Some(blocks);
                reading.group(self.sessions[index].summary.as_ref());
                // Start at the message a search result pointed to.
                if let Some(line) = reading.focus.and_then(|block| reading.line_of_block(block)) {
                    reading.list.scroll_to(ListOffset {
                        item_ix: line,
                        offset_in_item: px(0.),
                    });
                }
            }
            Err(err) => reading.error = Some(format!("{err:#}")),
        }
        cx.notify();
    }

    /// Cut the open transcript into steps again, after its summary changed.
    pub(super) fn regroup_transcript(&mut self, index: usize) {
        if let Some(reading) = self.reading.as_mut().filter(|r| r.index == index) {
            if reading.blocks.is_some() {
                reading.group(self.sessions[index].summary.as_ref());
            }
        }
    }

    /// Open the find field, or keep it open, in the transcript view.
    pub(super) fn open_find(&mut self, cx: &mut Context<Self>) {
        if let Some(reading) = self.reading.as_mut() {
            if reading.find.is_none() {
                reading.find = Some(Find::default());
            }
            cx.notify();
        }
    }

    /// Keys while the find field is open: typing, moving between matches,
    /// and closing it. Returns whether the key was the field's.
    pub(super) fn find_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(reading) = self.reading.as_mut() else { return false };
        let Some(find) = reading.find.as_mut() else { return false };
        let mods = keystroke.modifiers;
        match keystroke.key.as_str() {
            "escape" => {
                reading.find = None;
                reading.show_match();
            }
            "enter" | "down" | "up" => {
                let count = find.matches.len();
                if count > 0 {
                    let back = mods.shift || keystroke.key == "up";
                    find.current = match back {
                        true => (find.current + count - 1) % count,
                        false => (find.current + 1) % count,
                    };
                    reading.show_match();
                }
            }
            "backspace" => {
                if mods.secondary() || mods.alt {
                    find.query.clear();
                } else {
                    find.query.pop();
                }
                reading.search();
            }
            _ => {
                if mods.secondary() || mods.control || mods.function {
                    return false;
                }
                let Some(typed) = keystroke.key_char.as_deref() else { return true };
                if typed.is_empty() || typed.chars().any(char::is_control) {
                    return true;
                }
                find.query.push_str(typed);
                reading.search();
            }
        }
        cx.notify();
        true
    }

    pub(super) fn close_transcript(&mut self, cx: &mut Context<Self>) {
        self.reading = None;
        cx.notify();
    }

    fn toggle_tools(&mut self, block: usize, cx: &mut Context<Self>) {
        let Some(reading) = self.reading.as_mut() else {
            return;
        };
        if !reading.expanded.remove(&block) {
            reading.expanded.insert(block);
        }
        if let Some(line) = reading.line_of_block(block) {
            reading.list.splice(line..line + 1, 1);
        }
        cx.notify();
    }

    fn toggle_step(&mut self, step: usize, cx: &mut Context<Self>) {
        if let Some(reading) = self.reading.as_mut() {
            reading.cursor = Some(step);
            reading.toggle_step(step);
            cx.notify();
        }
    }

    /// Open or close the step the bracket keys are on; the first, if none.
    pub(super) fn toggle_current_step(&mut self, cx: &mut Context<Self>) {
        let Some(reading) = self.reading.as_ref().filter(|r| !r.steps.is_empty()) else {
            return;
        };
        self.toggle_step(reading.cursor.unwrap_or(0), cx);
    }

    /// Move to the next or previous step and bring its heading to the top.
    pub(super) fn move_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(reading) = self.reading.as_mut().filter(|r| !r.steps.is_empty()) else {
            return;
        };
        let last = reading.steps.len() as isize - 1;
        let next = match reading.cursor {
            Some(at) => (at as isize + delta).clamp(0, last),
            None if delta > 0 => 0,
            None => last,
        } as usize;
        let previous = reading.cursor.replace(next);
        // Both headings change, so the list has to draw them again.
        for step in [previous, Some(next)].into_iter().flatten() {
            if let Some(line) = reading.line_of_step(step) {
                reading.list.splice(line..line + 1, 1);
            }
        }
        if let Some(line) = reading.line_of_step(next) {
            reading.list.scroll_to(ListOffset {
                item_ix: line,
                offset_in_item: px(0.),
            });
        }
        cx.notify();
    }

    /// The whole transcript column: a back bar above the scrolling session.
    pub(super) fn reader(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let Some(reading) = &self.reading else {
            return div();
        };
        let fill = t.fill;
        let keys = if reading.steps.is_empty() {
            format!("esc back   {} find   ↵ copy resume", crate::ui::shortcut("F"))
        } else {
            format!("esc back   [ ] step   o open   {} find   ↵ copy resume", crate::ui::shortcut("F"))
        };
        let right: AnyElement = match &reading.find {
            None => div()
                .font_family(mono())
                .text_size(px(text::CAPTION))
                .text_color(t.ink3)
                .child(keys)
                .into_any_element(),
            Some(find) => {
                let lit = self.window_active && (self.caret_lit || self.typed_at.elapsed() < crate::ui::CARET_BLINK);
                let count = match (find.query.trim().is_empty(), find.matches.len()) {
                    (true, _) => String::new(),
                    (false, 0) => "no matches".to_string(),
                    (false, n) => format!("{} of {n}", find.current + 1),
                };
                div()
                    .w(px(420.))
                    .h(px(32.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(8.))
                    .bg(t.fill)
                    .text_size(px(text::SMALL))
                    .child(div().flex_1().min_w(px(0.)).child(crate::ui::search_field(
                        &find.query,
                        "Find in this session",
                        lit,
                        t,
                    )))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(mono())
                            .text_size(px(text::CAPTION))
                            .text_color(t.ink3)
                            .child(count),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(mono())
                            .text_size(px(text::CAPTION))
                            .text_color(t.ink3)
                            .child("↵ next  esc close"),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(
                centered(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .pt(px(14.))
                        .pb(px(8.))
                        .child(
                            div()
                                .id("back")
                    .reachable(t)
                                .h(px(34.))
                                .px(px(12.))
                                .flex()
                                .items_center()
                                .rounded(px(8.))
                                .text_size(px(text::SMALL))
                                .text_color(t.ink2)
                                .cursor_pointer()
                                .hover(move |style| style.bg(fill))
                                .on_click(cx.listener(|this, _, _, cx| this.close_transcript(cx)))
                                .child("‹  Sessions"),
                        )
                        .child(right),
                ),
            )
            .child(
                div().flex_1().min_h(px(0.)).child(
                    list(
                        reading.list.clone(),
                        cx.processor(|this, ix: usize, window, cx| {
                            this.render_block(ix, window, cx)
                        }),
                    )
                    .size_full(),
                ),
            )
    }

    fn render_block(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = Theme::for_appearance(window.appearance());
        let Some(reading) = &self.reading else {
            return div().into_any_element();
        };
        let session = &self.sessions[reading.index];
        let line = reading.lines.get(ix).copied();

        if let Some(Line::Head) = line {
            let mut head = div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .pt(px(10.))
                .pb(px(22.))
                .mb(px(10.))
                .border_b_1()
                .border_color(t.line)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .child(facts(session, &t, true))
                        .children(
                            session
                                .summary
                                .as_ref()
                                .map(|summary| outcome_mark(summary, true, &t)),
                        ),
                )
                .child(
                    div()
                        .text_size(px(text::HEADLINE))
                        .line_height(px(27.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(session.display_title().to_string()),
                );
            if let Some(summary) = &session.summary {
                head = head.child(
                    div()
                        .text_size(px(text::LEAD))
                        .line_height(px(24.))
                        .text_color(t.ink2)
                        .child(summary.summary.clone()),
                );
            }
            // The rest of the summary, which the card leaves out.
            if let Some(summary) = &session.summary {
                let mut details = div()
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .pt(px(14.))
                    .border_t_1()
                    .border_color(t.line)
                    .child(
                        div()
                            .flex()
                            .gap(px(40.))
                            .child(section("You asked", std::slice::from_ref(&summary.asked), Marker::None, &t))
                            .child(match reading.steps.is_empty() {
                                // Without steps to group by, the steps are listed here.
                                true => section("What happened", &summary.happened, Marker::Number, &t),
                                false => section("Open ends", &summary.open_ends, Marker::Dash, &t),
                            }),
                    );
                let mut more = div().flex().gap(px(40.));
                let mut any = false;
                if reading.steps.is_empty() && !summary.open_ends.is_empty() {
                    more = more.child(section("Open ends", &summary.open_ends, Marker::Dash, &t));
                    any = true;
                }
                if !summary.friction.is_empty() {
                    more = more.child(section("Friction", &summary.friction, Marker::Dash, &t));
                    any = true;
                }
                if !summary.files.is_empty() {
                    let mut files = div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(super::label("Files edited", &t));
                    for file in summary.files.iter().take(8) {
                        files = files.child(
                            div()
                                .font_family(mono())
                                .text_size(px(text::CAPTION))
                                .text_color(t.ink2)
                                .one_line()
                                .child(file.clone()),
                        );
                    }
                    if summary.files.len() > 8 {
                        files = files.child(
                            div()
                                .text_size(px(text::CAPTION))
                                .text_color(t.ink3)
                                .child(format!("and {} more", summary.files.len() - 8)),
                        );
                    }
                    more = more.child(files);
                    any = true;
                }
                if any {
                    details = details.child(more);
                }
                head = head.child(details);
            }
            head = head
                .child(stats(session, &t))
                .child(
                    div()
                        .id("reader-folder")
                        .font_family(mono())
                        .text_size(px(text::CAPTION))
                        .text_color(t.ink3)
                        .tooltip(crate::ui::tip(session.cwd.clone()))
                        .one_line()
                        .child(crate::ui::middle(&session.cwd, 96)),
                );
            if !reading.steps.is_empty() {
                head = head.child(
                    div()
                        .text_size(px(text::SMALL))
                        .text_color(t.ink3)
                        .child("Grouped by the steps in the summary. Click a step to read the turns behind it."),
                );
            }
            // With steps, each heading draws its own rule.
            if !reading.steps.is_empty() {
                head = head.mb(px(0.)).border_b_0();
            }
            return centered(head).into_any_element();
        }

        if let Some(Line::Step(i)) = line {
            let Some(step) = reading.steps.get(i) else {
                return div().into_any_element();
            };
            let open = reading.open.contains(&i);
            let at_cursor = reading.cursor == Some(i);
            let covered = reading
                .blocks
                .as_deref()
                .and_then(|blocks| blocks.get(step.blocks.clone()))
                .unwrap_or(&[]);
            let about = match covered.is_empty() {
                true => "nothing recorded".to_string(),
                false => step_facts(covered),
            };
            let fill = t.fill;
            return centered(
                div()
                    .id(("step", i))
                    .reachable(&t)
                    .flex()
                    .gap(px(14.))
                    .px(px(20.))
                    .py(px(15.))
                    .border_t_1()
                    .border_color(t.line)
                    .cursor_pointer()
                    .when(at_cursor, |el| el.bg(fill.opacity(0.6)))
                    .hover(move |style| style.bg(fill.opacity(0.6)))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_step(i, cx)))
                    .child(
                        div()
                            .w(px(44.))
                            .flex_shrink_0()
                            .pt(px(2.))
                            .font_family(mono())
                            .text_size(px(text::CAPTION))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if open { t.ink } else { t.ink3 })
                            .child(format!(
                                "{} {}",
                                if open { "▾" } else { "▸" },
                                step.number.map_or("+".to_string(), |n| n.to_string())
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_size(px(text::LEAD))
                                    .line_height(px(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(step.title.clone()),
                            )
                            .child(
                                div()
                                    .font_family(mono())
                                    .text_size(px(text::CAPTION))
                                    .text_color(t.ink3)
                                    .child(about),
                            ),
                    ),
            )
            .into_any_element();
        }

        let Some(Line::Block(at)) = line else {
            let status = match &reading.error {
                Some(error) => format!("The session could not be read: {error}"),
                None => "Reading the session…".to_string(),
            };
            return centered(div().py(px(16.)).text_color(t.ink3).child(status)).into_any_element();
        };
        let Some(block) = reading.blocks.as_ref().and_then(|blocks| blocks.get(at)) else {
            return div().into_any_element();
        };

        let body: AnyElement = match block {
            Block::Prompt(text) => turn(
                "you",
                div()
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(9.))
                    .bg(t.fill)
                    .child(text.clone()),
                &t,
            )
            .into_any_element(),
            Block::Reply(text) => {
                let nodes = reading.markdown(at, text);
                turn("agent", markdown::render(&nodes, &t, t.ink2, at), &t).into_any_element()
            }
            Block::Note(note) => turn(
                "",
                div().text_size(px(text::SMALL)).text_color(t.ink3).child(note.clone()),
                &t,
            )
            .into_any_element(),
            Block::Tools(calls) => {
                let open = reading.expanded.contains(&at);
                let fill = t.fill;
                let mut column = div().flex().flex_col().items_start().gap(px(8.)).child(
                    div()
                        .id(("tools", ix))
                    .reachable(&t)
                        .h(px(28.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(t.line)
                        .font_family(mono())
                        .text_size(px(text::CAPTION))
                        .text_color(t.ink3)
                        .cursor_pointer()
                        .hover(move |style| style.bg(fill))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_tools(at, cx)))
                        .child(if open { "▾" } else { "▸" })
                        .child(transcript::tools_line(calls)),
                );
                if open {
                    let mut details = div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .px(px(12.))
                        .py(px(10.))
                        .rounded(px(9.))
                        .border_1()
                        .border_color(t.line)
                        .font_family(mono())
                        .text_size(px(text::CAPTION))
                        .line_height(px(18.));
                    for (name, target) in calls {
                        details = details.child(
                            div()
                                .flex()
                                .gap(px(12.))
                                .child(div().w(px(120.)).flex_shrink_0().one_line().child(name.clone()))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .one_line()
                                        .text_color(t.ink3)
                                        .child(target.clone().unwrap_or_default()),
                                ),
                        );
                    }
                    column = column.child(details);
                }
                turn("tools", column, &t).into_any_element()
            }
        };
        if reading.focus == Some(at) {
            // The message the search led here for.
            return centered(
                div()
                    .rounded(px(10.))
                    .bg(t.accent.opacity(0.12))
                    .child(body),
            )
            .into_any_element();
        }
        centered(body).into_any_element()
    }
}

/// The same centred, width-limited column the feed uses.
pub(super) fn centered(content: impl IntoElement) -> Div {
    div()
        .w_full()
        .flex()
        .justify_center()
        .child(div().w_full().max_w(px(924.)).px(px(32.)).child(content))
}

/// One turn: a small role label on the left, the content on the right.
fn turn(role: &'static str, content: impl IntoElement, t: &Theme) -> Div {
    div()
        .flex()
        .gap(px(14.))
        .py(px(8.))
        .px(px(20.))
        .child(
            div()
                .w(px(44.))
                .flex_shrink_0()
                .pt(px(2.))
                .font_family(mono())
                .text_size(px(text::CAPTION))
                .text_color(t.ink3)
                .child(role),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(text::BODY))
                .line_height(px(22.))
                .child(content),
        )
}
