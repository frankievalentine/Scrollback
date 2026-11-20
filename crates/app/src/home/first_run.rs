//! The first-run screen: which harnesses were found, the choice of whether
//! summaries are written unasked, and the most recent sessions as a first
//! look. Nothing is summarised until this screen has been answered.

use super::{facts, label, logo, Home};
use crate::theme::{text, Theme, mono};
use crate::ui::{self, OneLine, Reachable};
use gpui::{div, prelude::*, px, svg, Context, Div, FontWeight, KeyDownEvent};
use scrollback_core::{sources, Harness, Origin};

const RECENT_SHOWN: usize = 4;

/// What was found for one harness or app.
struct Found {
    /// Which logo and name to show.
    origin: Origin,
    /// Where it was looked for, with the home folder written as `~`.
    place: String,
    present: bool,
    sessions: usize,
}

impl Home {
    fn found(&self) -> Vec<Found> {
        let home = scrollback_core::home();
        let shown = |path: &std::path::Path| match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        };
        let mut found: Vec<Found> = Harness::ALL
            .into_iter()
            .map(|harness| {
                let location = sources::location(harness);
                Found {
                    origin: harness.origin(),
                    present: location.exists(),
                    place: shown(&location),
                    sessions: self.sessions.iter().filter(|s| s.harness == harness).count(),
                }
            })
            .collect();
        // T3 Code keeps no conversations of its own; it says which of the
        // other harnesses' sessions were started from it.
        found.push(Found {
            origin: Origin::T3Code,
            present: sources::t3code::present(),
            place: shown(&sources::t3code::root().join("userdata")),
            sessions: self.sessions.iter().filter(|s| s.origin == Origin::T3Code).count(),
        });
        found.push(Found {
            origin: Origin::Paseo,
            present: sources::paseo::present(),
            place: shown(&sources::paseo::root()),
            sessions: self.sessions.iter().filter(|s| s.origin == Origin::Paseo).count(),
        });
        found
    }

    /// Pick whether summaries are written unasked. Without the `claude`
    /// command there is nothing to write them with, so that stays off.
    fn choose_auto(&mut self, auto: bool, cx: &mut Context<Self>) {
        self.config.auto_summarize = auto && self.summaries_ready;
        cx.notify();
    }

    /// Save the choice and move on to the app proper.
    fn finish_first_run(&mut self, cx: &mut Context<Self>) {
        self.first_run = false;
        self.config.onboarded = true;
        // A first run forced for development is a rehearsal: it must not
        // overwrite the settings the real one saved.
        if std::env::var_os("SCROLLBACK_FIRST_RUN").is_none() {
            crate::settings::store(self.config.clone(), cx);
        }
        self.pump_summaries(cx);
        self.pump_digest(cx);
        cx.notify();
    }

    pub(super) fn first_run_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.finish_first_run(cx),
            "up" => self.choose_auto(true, cx),
            "down" => self.choose_auto(false, cx),
            _ => {}
        }
    }

    pub(super) fn first_run_view(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let found = self.found();
        let present = found.iter().filter(|f| f.present).count();
        let heading = match present {
            0 => "No sessions found on this computer".to_string(),
            1 => "Found sessions from 1 source".to_string(),
            n => format!("Found sessions from {n} sources"),
        };

        // A row for each one found; the rest are named in a single line, so
        // the list stays short however many are supported.
        let mut harnesses = div().flex().flex_col();
        let rows: Vec<&Found> = found.iter().filter(|f| f.present).collect();
        let last = rows.len().saturating_sub(1);
        for (i, f) in rows.iter().enumerate() {
            harnesses = harnesses.child(
                harness_row(f, t).when(i < last, |el| el.border_b_1().border_color(t.line)),
            );
        }
        let missing: Vec<&str> = found.iter().filter(|f| !f.present).map(|f| f.origin.name()).collect();
        if !missing.is_empty() {
            harnesses = harnesses.child(
                div()
                    .pt(px(10.))
                    .text_size(px(text::SMALL))
                    .text_color(t.ink3)
                    .child(format!("Also looked for, and did not find: {}.", missing.join(", "))),
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
        if !self.summaries_ready {
            choice = choice.child(
                div()
                    .pt(px(2.))
                    .text_size(px(text::SMALL))
                    .line_height(px(19.))
                    .text_color(t.ink2)
                    .child(match scrollback_core::provider::ready(&self.config) {
                        Err(why) => format!("{why} Summaries can be set up in Settings later."),
                        Ok(()) => String::new(),
                    }),
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
                    .reachable(t)
                    .h(px(40.))
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(t.accent)
                    .text_color(t.on_accent)
                    .text_size(px(text::SMALL))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.9))
                    .on_click(cx.listener(|this, _, _, cx| this.finish_first_run(cx)))
                    .child("Open Scrollback"),
            )
            .child(
                div()
                    .text_size(px(text::SMALL))
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
                            .text_size(px(text::DISPLAY))
                            .line_height(px(32.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(heading),
                    )
                    .child(
                        div()
                            .text_size(px(text::LEAD))
                            .line_height(px(22.))
                            .text_color(t.ink2)
                            .child(
                                "Scrollback reads the session files where they already are \
                                 and keeps its index on this computer. It never writes to them.",
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
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Scrollback")),
            )
            .child(
                div().relative().flex_1().min_h(px(0.)).child(
                div()
                    .id("first-run")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.first_run_scroll)
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
                    ))
                    .children(ui::scrollbar(&self.first_run_scroll, t)),
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
        let available = !auto || self.summaries_ready;
        let (accent, raise) = (t.accent, t.raise);
        div()
            .id(id)
                    .reachable(t)
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
                            .text_size(px(text::SMALL))
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
                    .child("Nothing yet. A session appears here once one of the sources has recorded it."),
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
                            .text_size(px(text::LEAD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_clamp(2).text_ellipsis()
                            .child(session.display_title().to_string()),
                    )
                    .when(!blurb.is_empty(), |el| {
                        el.child(div().text_color(t.ink2).line_clamp(2).text_ellipsis().child(blurb))
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
                .text_size(px(text::SMALL))
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
    let quiet = |text: &'static str| div().text_size(px(text::SMALL)).text_color(t.ink3).child(text);
    let status = match (f.present, f.sessions) {
        (false, _) => quiet("Not found"),
        (true, 0) => quiet("No sessions yet"),
        (true, n) => div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(text::SMALL))
            .text_color(t.ink2)
            .child(svg().path("check.svg").size(px(14.)).flex_shrink_0().text_color(t.ink))
            .child(if n == 1 { "1 session".to_string() } else { format!("{n} sessions") }),
    };
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .py(px(12.))
        .child(logo(f.origin, t, 20.))
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
                        .child(f.origin.name()),
                )
                .child(
                    div()
                        .one_line()
                        .font_family(mono())
                        .text_size(px(text::CAPTION))
                        .text_color(t.ink3)
                        .child(ui::middle(&f.place, 44)),
                ),
        )
        .child(status.flex_shrink_0())
}
