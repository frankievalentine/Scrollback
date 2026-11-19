//! The first-run screen: which harnesses were found, the choice of whether
//! summaries are written unasked, and the most recent sessions as a first
//! look. Nothing is summarised until this screen has been answered.

use super::{facts, label, logo, Home};
use crate::theme::{Theme, MONO_FONT};
use gpui::{div, prelude::*, px, svg, Context, Div, FontWeight, KeyDownEvent, Window};
use scrollback_core::{sources, Harness};

const RECENT_SHOWN: usize = 4;

/// What was found for one harness.
struct Found {
    harness: Harness,
    /// Where it was looked for, with the home folder written as `~`.
    place: String,
    present: bool,
    sessions: usize,
}

impl Home {
    fn found(&self) -> Vec<Found> {
        let home = scrollback_core::home();
        Harness::ALL
            .into_iter()
            .map(|harness| {
                let location = sources::location(harness);
                Found {
                    harness,
                    present: location.exists(),
                    place: match location.strip_prefix(&home) {
                        Ok(rest) => format!("~/{}", rest.display()),
                        Err(_) => location.display().to_string(),
                    },
                    sessions: self.sessions.iter().filter(|s| s.harness == harness).count(),
                }
            })
            .collect()
    }

    /// Pick whether summaries are written unasked. Without the `claude`
    /// command there is nothing to write them with, so that stays off.
    fn choose_auto(&mut self, auto: bool, cx: &mut Context<Self>) {
        self.config.auto_summarize = auto && self.claude_found;
        cx.notify();
    }

    /// Save the choice and move on to the app proper.
    fn finish_first_run(&mut self, cx: &mut Context<Self>) {
        self.first_run = false;
        self.config.onboarded = true;
        if let Err(err) = self.config.save() {
            eprintln!("could not save settings: {err:#}");
        }
        self.pump_summaries(cx);
        self.pump_digest(cx);
        cx.notify();
    }

    pub(super) fn first_run_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mods = event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "enter" => self.finish_first_run(cx),
            "up" => self.choose_auto(true, cx),
            "down" => self.choose_auto(false, cx),
            "q" if mods.platform => cx.quit(),
            "w" if mods.platform => window.remove_window(),
            _ => {}
        }
    }

    pub(super) fn first_run_view(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let found = self.found();
        let present = found.iter().filter(|f| f.present).count();
        let heading = match present {
            0 => "No harnesses found on this Mac".to_string(),
            1 => "Found 1 harness on this Mac".to_string(),
            n => format!("Found {n} harnesses on this Mac"),
        };

        let mut harnesses = div().flex().flex_col();
        let last = found.len().saturating_sub(1);
        for (i, f) in found.iter().enumerate() {
            harnesses = harnesses.child(
                harness_row(f, t).when(i < last, |el| el.border_b_1().border_color(t.line)),
            );
        }

        let mut choice = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().pb(px(4.)).child(label("How summaries are written", t)))
            .child(self.option(
                "summaries-auto",
                true,
                "Automatically",
                "Claude Code writes one once a session has been quiet for ten minutes. \
                 It is sent your prompts and the agent's replies, never tool output, \
                 and it counts towards your Claude plan.",
                t,
                cx,
            ))
            .child(self.option(
                "summaries-asked",
                false,
                "Only when I ask",
                "Nothing is sent until you press Write summary on a session.",
                t,
                cx,
            ));
        if !self.claude_found {
            choice = choice.child(
                div()
                    .pt(px(2.))
                    .text_size(px(13.))
                    .line_height(px(19.))
                    .text_color(t.ink2)
                    .child("The claude command was not found, so summaries cannot be written yet."),
            );
        }

        let open = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(14.))
            .child(
                div()
                    .id("open-scrollback")
                    .h(px(40.))
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(t.accent)
                    .text_color(t.on_accent)
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.9))
                    .on_click(cx.listener(|this, _, _, cx| this.finish_first_run(cx)))
                    .child("Open scrollback"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(t.ink3)
                    .child("You can change this later from the sidebar."),
            );

        let left = div()
            .flex_1()
            .min_w(px(400.))
            .flex()
            .flex_col()
            .gap(px(26.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(26.))
                            .line_height(px(32.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(heading),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .line_height(px(22.))
                            .text_color(t.ink2)
                            .child(
                                "scrollback reads the session files where they already are \
                                 and keeps its index on this Mac. It never writes to them.",
                            ),
                    ),
            )
            .child(harnesses)
            .child(choice)
            .child(open);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                // Clear of the window's close, minimise and zoom buttons.
                div()
                    .h(px(52.))
                    .flex_shrink_0()
                    .pl(px(88.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .child(svg().path("mark.svg").size(px(18.)).flex_shrink_0().text_color(t.accent))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("scrollback")),
            )
            .child(
                div()
                    .id("first-run")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(
                        div().w_full().flex().justify_center().child(
                            div()
                                .w_full()
                                .max_w(px(1120.))
                                .px(px(32.))
                                .pt(px(20.))
                                .pb(px(40.))
                                .flex()
                                .flex_wrap()
                                .items_start()
                                .gap_x(px(72.))
                                .gap_y(px(48.))
                                .child(left)
                                .child(self.recent_card(t)),
                        ),
                    ),
            )
    }

    /// One of the two answers, drawn as a radio button with an explanation.
    fn option(
        &self,
        id: &'static str,
        auto: bool,
        title: &'static str,
        text: &'static str,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.config.auto_summarize == auto;
        let available = !auto || self.claude_found;
        let (accent, raise) = (t.accent, t.raise);
        div()
            .id(id)
            .flex()
            .gap(px(12.))
            .px(px(16.))
            .py(px(14.))
            .rounded(px(10.))
            .border_2()
            .border_color(if selected { t.accent } else { t.line })
            .when(selected, |el| el.bg(raise))
            .when(!available, |el| el.opacity(0.55))
            .when(available, |el| {
                el.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_auto(auto, cx)))
            })
            .child(
                div()
                    .mt(px(2.))
                    .size(px(16.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(if selected { t.accent } else { t.ink3 })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(selected, |el| {
                        el.child(div().size(px(8.)).rounded_full().bg(accent))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(19.))
                            .text_color(t.ink2)
                            .child(text),
                    ),
            )
    }

    /// The newest few sessions, so the screen already shows something real.
    fn recent_card(&self, t: &Theme) -> Div {
        let mut card = div()
            .flex_1()
            .min_w(px(400.))
            .flex()
            .flex_col()
            .px(px(28.))
            .pt(px(8.))
            .pb(px(22.))
            .rounded(px(14.))
            .bg(t.raise)
            .shadow(t.card_shadow.clone())
            .child(div().pt(px(18.)).child(label("Most recent", t)));
        if self.sessions.is_empty() {
            return card.child(
                div()
                    .py(px(16.))
                    .text_color(t.ink3)
                    .child("Nothing yet. A session appears here once a harness has recorded one."),
            );
        }
        for session in self.sessions.iter().take(RECENT_SHOWN) {
            let blurb = match &session.summary {
                Some(summary) => summary.summary.clone(),
                None if session.first_prompt != session.title => session.first_prompt.clone(),
                None => String::new(),
            };
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .py(px(16.))
                    .border_b_1()
                    .border_color(t.line)
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_clamp(2)
                            .child(session.display_title().to_string()),
                    )
                    .when(!blurb.is_empty(), |el| {
                        el.child(div().text_color(t.ink2).line_clamp(2).child(blurb))
                    })
                    .child(facts(session, t, true)),
            );
        }
        let total = self.sessions.len();
        let summarised = self.sessions.iter().filter(|s| s.summary.is_some()).count();
        let counts = match (total, summarised) {
            (1, 0) => "1 session indexed, without a summary yet.".to_string(),
            (1, _) => "1 session indexed, with a summary.".to_string(),
            (n, 0) => format!("{n} sessions indexed, none with a summary yet."),
            (n, s) if s == n => format!("{n} sessions indexed, all with a summary."),
            (n, s) => format!("{n} sessions indexed, {s} with a summary."),
        };
        card.child(
            div()
                .pt(px(16.))
                .text_size(px(13.))
                .line_height(px(19.))
                .text_color(t.ink3)
                .child(format!(
                    "{counts} Until one is written, a session shows your first prompt as its \
                     title. Search works either way."
                )),
        )
    }
}

fn harness_row(f: &Found, t: &Theme) -> Div {
    let quiet = |text: &'static str| div().text_size(px(13.)).text_color(t.ink3).child(text);
    let status = match (f.present, f.sessions) {
        (false, _) => quiet("Not found"),
        (true, 0) => quiet("No sessions yet"),
        (true, n) => div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(13.))
            .text_color(t.ink2)
            .child(svg().path("check.svg").size(px(14.)).flex_shrink_0().text_color(t.ink))
            .child(if n == 1 { "1 session".to_string() } else { format!("{n} sessions") }),
    };
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .py(px(12.))
        .child(logo(f.harness, t, 20.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if f.present { t.ink } else { t.ink2 })
                        .child(f.harness.name()),
                )
                .child(
                    div()
                        .truncate()
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .text_color(t.ink3)
                        .child(f.place.clone()),
                ),
        )
        .child(status.flex_shrink_0())
}
