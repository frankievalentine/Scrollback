//! The transcript view: one session read top to bottom, with each run of
//! tool calls folded into a single line that opens on click.

use super::{facts, outcome_mark, Home};
use crate::theme::{Theme, MONO_FONT};
use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Div, FontWeight, ListAlignment, ListState,
    Window,
};
use scrollback_core::transcript::{self, Block};
use std::collections::HashSet;

pub(super) struct Reading {
    /// Index into `Home::sessions`.
    pub index: usize,
    /// `None` while the session is being read from disk.
    blocks: Option<Vec<Block>>,
    error: Option<String>,
    /// Tool blocks the reader has opened.
    expanded: HashSet<usize>,
    list: ListState,
}

fn read_blocks(session: &scrollback_core::Session) -> anyhow::Result<Vec<Block>> {
    Ok(transcript::blocks(transcript::events(session)?))
}

impl Home {
    /// Open the selected session's transcript, reading it in the background.
    pub(super) fn open_transcript(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.selected else { return };
        let index = self.hits[hit].index;
        let session = self.sessions[index].clone();
        self.reading = Some(Reading {
            index,
            blocks: None,
            error: None,
            expanded: HashSet::new(),
            // Row 0 is the header, row 1 the loading line.
            list: ListState::new(2, ListAlignment::Top, px(1200.)),
        });
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
        let Some(hit) = self.selected else { return };
        let index = self.hits[hit].index;
        let result = read_blocks(&self.sessions[index]);
        self.reading = Some(Reading {
            index,
            blocks: None,
            error: None,
            expanded: HashSet::new(),
            list: ListState::new(2, ListAlignment::Top, px(1200.)),
        });
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
                reading.list.reset(blocks.len() + 1);
                reading.blocks = Some(blocks);
            }
            Err(err) => reading.error = Some(format!("{err:#}")),
        }
        cx.notify();
    }

    pub(super) fn close_transcript(&mut self, cx: &mut Context<Self>) {
        self.reading = None;
        cx.notify();
    }

    fn toggle_tools(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(reading) = self.reading.as_mut() else {
            return;
        };
        if !reading.expanded.remove(&row) {
            reading.expanded.insert(row);
        }
        reading.list.splice(row..row + 1, 1);
        cx.notify();
    }

    /// The whole transcript column: a back bar above the scrolling session.
    pub(super) fn reader(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let Some(reading) = &self.reading else {
            return div();
        };
        let fill = t.fill;
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
                                .h(px(34.))
                                .px(px(12.))
                                .flex()
                                .items_center()
                                .rounded(px(8.))
                                .text_size(px(13.))
                                .text_color(t.ink2)
                                .cursor_pointer()
                                .hover(move |style| style.bg(fill))
                                .on_click(cx.listener(|this, _, _, cx| this.close_transcript(cx)))
                                .child("‹  Sessions"),
                        )
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .text_size(px(11.))
                                .text_color(t.ink3)
                                .child("esc back   ↵ copy resume"),
                        ),
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

        if ix == 0 {
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
                        .text_size(px(21.))
                        .line_height(px(27.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(session.display_title().to_string()),
                );
            if let Some(summary) = &session.summary {
                head = head.child(
                    div()
                        .text_size(px(15.))
                        .line_height(px(24.))
                        .text_color(t.ink2)
                        .child(summary.summary.clone()),
                );
            }
            return centered(head).into_any_element();
        }

        let Some(blocks) = &reading.blocks else {
            let status = match &reading.error {
                Some(error) => format!("The session could not be read: {error}"),
                None => "Reading the session…".to_string(),
            };
            return centered(div().py(px(16.)).text_color(t.ink3).child(status)).into_any_element();
        };
        let Some(block) = blocks.get(ix - 1) else {
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
                turn("agent", div().text_color(t.ink2).child(text.clone()), &t).into_any_element()
            }
            Block::Note(note) => turn(
                "",
                div().text_size(px(13.)).text_color(t.ink3).child(note.clone()),
                &t,
            )
            .into_any_element(),
            Block::Tools(calls) => {
                let open = reading.expanded.contains(&ix);
                let fill = t.fill;
                let mut column = div().flex().flex_col().items_start().gap(px(8.)).child(
                    div()
                        .id(("tools", ix))
                        .h(px(28.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(t.line)
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .text_color(t.ink3)
                        .cursor_pointer()
                        .hover(move |style| style.bg(fill))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_tools(ix, cx)))
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
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .line_height(px(18.));
                    for (name, target) in calls {
                        details = details.child(
                            div()
                                .flex()
                                .gap(px(12.))
                                .child(div().w(px(120.)).flex_shrink_0().truncate().child(name.clone()))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
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
        centered(body).into_any_element()
    }
}

/// The same centred, width-limited column the feed uses.
fn centered(content: impl IntoElement) -> Div {
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
                .font_family(MONO_FONT)
                .text_size(px(11.))
                .text_color(t.ink3)
                .child(role),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(14.))
                .line_height(px(22.))
                .child(content),
        )
}
