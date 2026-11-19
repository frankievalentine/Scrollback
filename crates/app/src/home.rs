//! The home view: a sidebar of filters and a feed of sessions, newest first,
//! with type-to-search over everything.

mod reader;

use crate::theme::{claude_orange, Theme, MONO_FONT, UI_FONT};
use reader::Reading;
use chrono::{DateTime, Local, TimeZone};
use gpui::{
    div, list, prelude::*, px, svg, AnyElement, App, ClipboardItem, Context, Div, FocusHandle,
    FontWeight, HighlightStyle, Hsla, KeyDownEvent, ListAlignment, ListState, SharedString,
    StyledText, Window,
};
use scrollback_core::search::{Hit, Searcher};
use scrollback_core::{summarize, Harness, Outcome, Session, Store, Summary};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant};

/// Refresh the index and read every session from it.
pub fn load() -> (Vec<Session>, SharedString) {
    let loaded = Store::open().and_then(|mut store| {
        let report = store.refresh()?;
        Ok((store.sessions(false)?, report))
    });
    match loaded {
        Ok((sessions, report)) => {
            let status = format!(
                "{} sessions\nindexed in {:.0?}",
                sessions.len(),
                report.elapsed
            );
            (sessions, status.into())
        }
        Err(err) => (Vec::new(), format!("Index error: {err:#}").into()),
    }
}

enum Row {
    Header(SharedString),
    /// An index into `Home::hits`.
    Session(usize),
}

pub struct Home {
    sessions: Vec<Session>,
    hits: Vec<Hit>,
    rows: Vec<Row>,
    query: String,
    harness: Option<Harness>,
    project: Option<String>,
    selected: Option<usize>,
    copied: Option<usize>,
    /// The transcript being read, when the reader is open.
    reading: Option<Reading>,
    /// Keys of sessions whose summary is being written right now.
    busy: HashSet<String>,
    /// The last summary error per session key.
    failed: HashMap<String, String>,
    searcher: Searcher,
    search_time: Duration,
    list: ListState,
    status: SharedString,
    pub focus: FocusHandle,
}

impl Home {
    pub fn new(
        sessions: Vec<Session>,
        status: SharedString,
        query: String,
        cx: &mut Context<Self>,
    ) -> Home {
        let mut home = Home {
            sessions,
            hits: Vec::new(),
            rows: Vec::new(),
            query,
            harness: None,
            project: None,
            selected: None,
            copied: None,
            reading: None,
            busy: HashSet::new(),
            failed: HashMap::new(),
            searcher: Searcher::new(),
            search_time: Duration::ZERO,
            list: ListState::new(0, ListAlignment::Top, px(800.)),
            status,
            focus: cx.focus_handle(),
        };
        home.recompute();
        // Development aid: write the selected session's summary on launch,
        // to exercise the background path without a click.
        if std::env::var_os("SCROLLBACK_DEV_SUMMARIZE").is_some() {
            home.summarize_selected(cx);
        }
        // Development aid: open straight into the selected session's transcript.
        if std::env::var_os("SCROLLBACK_DEV_READ").is_some() {
            home.open_transcript_now(cx);
        }
        home
    }

    /// Re-run the search and filters, and rebuild the rows.
    fn recompute(&mut self) {
        let started = Instant::now();
        let mut hits = self.searcher.search(&self.query, &self.sessions);
        hits.retain(|hit| {
            let session = &self.sessions[hit.index];
            self.harness.is_none_or(|h| session.harness == h)
                && self
                    .project
                    .as_deref()
                    .is_none_or(|p| session.project() == p)
        });
        self.search_time = started.elapsed();
        self.hits = hits;

        self.rows.clear();
        let grouped_by_day = self.query.trim().is_empty();
        let mut last_day = String::new();
        for (i, hit) in self.hits.iter().enumerate() {
            if grouped_by_day {
                let day = day_label(self.sessions[hit.index].updated_ms);
                if day != last_day {
                    self.rows.push(Row::Header(day.to_uppercase().into()));
                    last_day = day;
                }
            }
            self.rows.push(Row::Session(i));
        }
        self.selected = (!self.hits.is_empty()).then_some(0);
        self.copied = None;
        self.list.reset(self.rows.len());
    }

    fn row_of(&self, hit: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, Row::Session(h) if *h == hit))
    }

    fn select(&mut self, hit: usize, cx: &mut Context<Self>) {
        if self.selected == Some(hit) {
            return;
        }
        // Both rows change height, so the list has to measure them again.
        for changed in [self.selected, Some(hit)].into_iter().flatten() {
            if let Some(row) = self.row_of(changed) {
                self.list.splice(row..row + 1, 1);
            }
        }
        self.selected = Some(hit);
        self.copied = None;
        if let Some(row) = self.row_of(hit) {
            self.list.scroll_to_reveal_item(row);
        }
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            return;
        }
        let current = self.selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.hits.len() as isize - 1) as usize;
        self.select(next, cx);
    }

    fn copy_resume(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.selected else { return };
        let session = &self.sessions[self.hits[hit].index];
        cx.write_to_clipboard(ClipboardItem::new_string(resume_command(session)));
        self.copied = Some(hit);
        cx.notify();
    }

    fn reindex(&mut self, cx: &mut Context<Self>) {
        let (sessions, status) = load();
        self.sessions = sessions;
        self.status = status;
        self.recompute();
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        if self.reading.is_some() {
            // The reader has its own small set of keys; typing does nothing.
            match keystroke.key.as_str() {
                "escape" | "left" => self.close_transcript(cx),
                "enter" => self.copy_resume(cx),
                "q" if mods.platform => cx.quit(),
                "w" if mods.platform => window.remove_window(),
                _ => {}
            }
            return;
        }
        match keystroke.key.as_str() {
            "right" => self.open_transcript(cx),
            "down" => self.move_selection(1, cx),
            "up" => self.move_selection(-1, cx),
            "enter" => self.copy_resume(cx),
            "q" if mods.platform => cx.quit(),
            "w" if mods.platform => window.remove_window(),
            "r" if mods.platform => self.reindex(cx),
            "s" if mods.platform => self.summarize_selected(cx),
            "escape" => {
                if self.query.is_empty() {
                    self.harness = None;
                    self.project = None;
                } else {
                    self.query.clear();
                }
                self.recompute();
                cx.notify();
            }
            "backspace" => {
                if mods.platform || mods.alt {
                    self.query.clear();
                } else {
                    self.query.pop();
                }
                self.recompute();
                cx.notify();
            }
            _ => {
                // Anything that types a character goes into the search.
                if mods.platform || mods.control || mods.function {
                    return;
                }
                let Some(typed) = keystroke.key_char.as_deref() else {
                    return;
                };
                if typed.is_empty() || typed.chars().any(char::is_control) {
                    return;
                }
                self.query.push_str(typed);
                self.recompute();
                cx.notify();
            }
        }
    }

    fn sidebar(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let mut harnesses = div().flex().flex_col().gap(px(2.)).child(div().px(px(10.)).child(label("Harness", t)));
        for harness in Harness::ALL {
            let count = self.sessions.iter().filter(|s| s.harness == harness).count();
            let active = self.harness == Some(harness);
            harnesses = harnesses.child(
                nav_row(t, active)
                    .id(harness.slug())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.harness = (this.harness != Some(harness)).then_some(harness);
                        this.recompute();
                        cx.notify();
                    }))
                    .child(logo(harness, t, 16.))
                    .child(div().flex_1().child(harness.name()))
                    .child(count_badge(count, t)),
            );
        }

        // Projects, busiest first.
        let mut counts: Vec<(String, usize)> = Vec::new();
        for session in &self.sessions {
            let name = session.project();
            if name.is_empty() {
                continue;
            }
            match counts.iter_mut().find(|(n, _)| n == name) {
                Some(entry) => entry.1 += 1,
                None => counts.push((name.to_string(), 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        let mut projects = div().flex().flex_col().gap(px(2.)).child(div().px(px(10.)).child(label("Project", t)));
        for (i, (name, count)) in counts.into_iter().take(10).enumerate() {
            let active = self.project.as_deref() == Some(name.as_str());
            let clicked = name.clone();
            projects = projects.child(
                nav_row(t, active)
                    .id(("project", i))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let same = this.project.as_deref() == Some(clicked.as_str());
                        this.project = (!same).then(|| clicked.clone());
                        this.recompute();
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(name))
                    .child(count_badge(count, t)),
            );
        }

        div()
            .w(px(216.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(22.))
            .pt(px(52.))
            .pb(px(14.))
            .px(px(10.))
            .bg(t.rail)
            .border_r_1()
            .border_color(t.line)
            .child(
                div()
                    .px(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("scrollback"),
            )
            .child(harnesses)
            .child(projects)
            .child(div().flex_1())
            .child(
                div()
                    .px(px(10.))
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .line_height(px(17.))
                    .text_color(t.ink3)
                    .child(self.status.clone()),
            )
    }

    fn search_bar(&self, t: &Theme) -> Div {
        let searching = !self.query.is_empty();
        let text = if searching {
            div().text_color(t.ink).child(self.query.clone())
        } else {
            div()
                .text_color(t.ink3)
                .child("Type to search sessions, projects and prompts")
        };
        let hint = if searching {
            format!("{} matches · {:.1?}", self.hits.len(), self.search_time)
        } else {
            "↑↓ move   → read   ↵ copy resume   ⌘S summarise   ⌘R reindex".to_string()
        };
        div().w_full().flex().justify_center().child(
            div()
                .w_full()
                .max_w(px(924.))
                .px(px(32.))
                .pt(px(14.))
                .pb(px(8.))
                .child(
                    div()
                        .h(px(40.))
                        .px(px(14.))
                        .rounded(px(10.))
                        .bg(t.fill)
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(0.)).truncate().child(text))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(MONO_FONT)
                                .text_size(px(11.))
                                .text_color(t.ink3)
                                .child(hint),
                        ),
                ),
        )
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = Theme::for_appearance(window.appearance());
        let content = match self.rows.get(ix) {
            None => div().into_any_element(),
            Some(Row::Header(text)) => div()
                .px(px(20.))
                .pt(px(22.))
                .pb(px(8.))
                .child(label(text.clone(), &t))
                .into_any_element(),
            Some(Row::Session(hit)) => {
                let hit = *hit;
                if self.selected == Some(hit) {
                    self.card(hit, &t, cx).into_any_element()
                } else {
                    self.compact(hit, ix, &t, cx).into_any_element()
                }
            }
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(div().w_full().max_w(px(924.)).px(px(32.)).child(content))
            .into_any_element()
    }

    /// Write (or rewrite) the selected session's summary in the background.
    fn summarize_selected(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.selected else { return };
        let session = self.sessions[self.hits[hit].index].clone();
        let key = session.key();
        if !self.busy.insert(key.clone()) {
            return;
        }
        self.failed.remove(&key);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let work = cx.background_executor().spawn(async move {
                let written = summarize::summarize(&session)?;
                Store::open()?.set_summary(&session, &written.summary)?;
                anyhow::Ok(written.summary)
            });
            let result = work.await;
            this.update(cx, |this, cx| this.summary_arrived(key, result, cx))
                .ok();
        })
        .detach();
    }

    fn summary_arrived(&mut self, key: String, result: anyhow::Result<Summary>, cx: &mut Context<Self>) {
        self.busy.remove(&key);
        let index = self.sessions.iter().position(|s| s.key() == key);
        match result {
            Ok(summary) => {
                eprintln!("summary written for {key}: {}", summary.title);
                if let Some(index) = index {
                    self.sessions[index].summary = Some(summary);
                    self.sessions[index].summary_stale = false;
                }
            }
            Err(err) => {
                eprintln!("summary failed for {key}: {err:#}");
                self.failed.insert(key, format!("{err:#}"));
            }
        }
        // The row's height has changed, so have the list measure it again.
        let row = index
            .and_then(|index| self.hits.iter().position(|hit| hit.index == index))
            .and_then(|hit| self.row_of(hit));
        if let Some(row) = row {
            self.list.splice(row..row + 1, 1);
        }
        cx.notify();
    }

    /// One unselected session: time, title, a line or two about it, and facts.
    fn compact(&self, hit: usize, ix: usize, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let found = &self.hits[hit];
        let session = &self.sessions[found.index];
        let blurb = match &session.summary {
            Some(summary) => summary.summary.clone(),
            None if session.first_prompt != session.title => session.first_prompt.clone(),
            None => String::new(),
        };
        div()
            .id(("session", ix))
            .flex()
            .gap(px(20.))
            .px(px(20.))
            .py(px(14.))
            .rounded(px(10.))
            .cursor_pointer()
            .hover(|style| style.bg(t.fill.opacity(0.5)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(hit, cx)))
            .child(
                div()
                    .w(px(44.))
                    .flex_shrink_0()
                    .pt(px(2.))
                    .font_family(MONO_FONT)
                    .text_size(px(12.))
                    .text_color(t.ink3)
                    .child(clock(session.updated_ms)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(16.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(title_text(
                                        session.display_title(),
                                        &found.title_matches,
                                        t,
                                    )),
                            )
                            .children(
                                session
                                    .summary
                                    .as_ref()
                                    .map(|summary| outcome_mark(summary, false, t)),
                            ),
                    )
                    .when(!blurb.is_empty(), |el| {
                        el.child(div().text_color(t.ink2).line_clamp(2).child(blurb))
                    })
                    .child(facts(session, t, false)),
            )
    }

    /// The selected session, lifted into a card with everything known about it.
    fn card(&self, hit: usize, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let found = &self.hits[hit];
        let session = &self.sessions[found.index];
        let key = session.key();
        let busy = self.busy.contains(&key);
        let copied = self.copied == Some(hit);

        let mut stats = div()
            .flex()
            .flex_wrap()
            .gap_x(px(18.))
            .gap_y(px(4.))
            .font_family(MONO_FONT)
            .text_size(px(12.))
            .text_color(t.ink3)
            .child(format!("{} prompts", session.user_turns))
            .child(format!("{} replies", session.assistant_turns))
            .child(format!("{} tool calls", session.tool_calls))
            .child(format!(
                "{} tokens",
                compact_number(session.tokens_in + session.tokens_out)
            ));
        if let Some(cost) = session.cost_usd {
            stats = stats.child(format!("${cost:.2}"));
        }
        if let Some(model) = &session.model {
            stats = stats.child(model.clone());
        }

        let mut card = div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .px(px(30.))
            .py(px(26.))
            .rounded(px(14.))
            .bg(t.raise)
            .shadow(t.card_shadow.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(facts(session, t, true))
                    .children(
                        session
                            .summary
                            .as_ref()
                            .map(|summary| outcome_mark(summary, true, t)),
                    ),
            )
            .child(
                div()
                    .text_size(px(21.))
                    .line_height(px(27.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title_text(
                        session.display_title(),
                        &found.title_matches,
                        t,
                    )),
            );

        match &session.summary {
            Some(summary) => {
                card = card.child(
                    div()
                        .text_size(px(15.))
                        .line_height(px(24.))
                        .text_color(t.ink2)
                        .child(summary.summary.clone()),
                );
                let mut grid = div()
                    .flex()
                    .flex_col()
                    .gap(px(22.))
                    .pt(px(20.))
                    .border_t_1()
                    .border_color(t.line)
                    .child(
                        div()
                            .flex()
                            .gap(px(40.))
                            .child(section(
                                "You asked",
                                std::slice::from_ref(&summary.asked),
                                Marker::None,
                                t,
                            ))
                            .child(section("What happened", &summary.happened, Marker::Number, t)),
                    );
                if !summary.open_ends.is_empty() || !summary.friction.is_empty() {
                    grid = grid.child(
                        div()
                            .flex()
                            .gap(px(40.))
                            .child(section("Left open", &summary.open_ends, Marker::Dash, t))
                            .child(section("Friction", &summary.friction, Marker::Dash, t)),
                    );
                }
                card = card.child(grid);
                if !summary.files.is_empty() {
                    let shown = summary.files.len().min(6);
                    let mut files = div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .text_color(t.ink2);
                    for file in &summary.files[..shown] {
                        files = files.child(div().truncate().child(file.clone()));
                    }
                    if summary.files.len() > shown {
                        files = files.child(
                            div()
                                .text_color(t.ink3)
                                .child(format!("and {} more", summary.files.len() - shown)),
                        );
                    }
                    card = card.child(
                        div()
                            .flex()
                            .flex_col()
                            .child(label("Files edited", t))
                            .child(files),
                    );
                }
            }
            None => {
                card = card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .pt(px(18.))
                        .border_t_1()
                        .border_color(t.line)
                        .child(label("You asked", t))
                        .child(
                            div()
                                .text_size(px(15.))
                                .line_height(px(23.))
                                .text_color(t.ink2)
                                .line_clamp(6)
                                .child(if session.first_prompt.is_empty() {
                                    "No typed prompt was recorded for this session.".to_string()
                                } else {
                                    session.first_prompt.clone()
                                }),
                        ),
                );
            }
        }

        let summary_action = if busy {
            "Writing summary…"
        } else if session.summary.is_none() {
            "Write summary"
        } else if session.summary_stale {
            "Update summary"
        } else {
            "Rewrite summary"
        };
        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .id("copy-resume")
                    .h(px(36.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(t.accent)
                    .text_color(t.on_accent)
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.9))
                    .on_click(cx.listener(|this, _, _, cx| this.copy_resume(cx)))
                    .child(if copied {
                        "Copied to clipboard".to_string()
                    } else {
                        format!("Copy resume command for {}", session.harness.name())
                    }),
            )
            .child(
                div()
                    .id("read-transcript")
                    .h(px(36.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.fill))
                    .on_click(cx.listener(|this, _, _, cx| this.open_transcript(cx)))
                    .child("Read transcript"),
            )
            .child(
                div()
                    .id("write-summary")
                    .h(px(36.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.line)
                    .text_color(if busy { t.ink3 } else { t.ink })
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.fill))
                    .on_click(cx.listener(|this, _, _, cx| this.summarize_selected(cx)))
                    .child(summary_action),
            );
        if session.summary_stale && !busy {
            actions = actions.child(
                div()
                    .text_size(px(12.))
                    .text_color(t.ink3)
                    .child("The session has moved on since this was written."),
            );
        }

        card = card
            .child(stats)
            .child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(12.))
                    .text_color(t.ink3)
                    .truncate()
                    .child(session.cwd.clone()),
            )
            .child(actions);
        if let Some(error) = self.failed.get(&key) {
            card = card.child(
                div()
                    .text_size(px(13.))
                    .text_color(t.ink2)
                    .child(format!("The summary could not be written: {error}")),
            );
        }
        div().py(px(8.)).child(card)
    }
}

impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        let feed: AnyElement = if self.rows.is_empty() {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.ink3)
                .child(if self.sessions.is_empty() {
                    "No sessions found on this Mac yet."
                } else {
                    "Nothing matches. Press esc to clear."
                })
                .into_any_element()
        } else {
            list(
                self.list.clone(),
                cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx)),
            )
            .size_full()
            .into_any_element()
        };

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(UI_FONT)
            .text_size(px(14.))
            .line_height(px(20.))
            .child(self.sidebar(&t, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .pt(px(28.))
                    .when(self.reading.is_some(), |el| el.child(self.reader(&t, cx)))
                    .when(self.reading.is_none(), |el| {
                        el.child(self.search_bar(&t))
                            .child(div().flex_1().min_h(px(0.)).child(feed))
                    }),
            )
    }
}

fn label(text: impl Into<SharedString>, t: &Theme) -> Div {
    let text: SharedString = text.into();
    div()
        .pb(px(6.))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.ink3)
        .child(SharedString::from(text.to_uppercase()))
}

enum Marker {
    None,
    Number,
    Dash,
}

/// One labelled column of the summary: a heading and its lines.
fn section(title: &'static str, lines: &[String], marker: Marker, t: &Theme) -> Div {
    let mut column = div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(label(title, t));
    if lines.is_empty() {
        return column.child(div().text_color(t.ink3).child("Nothing."));
    }
    for (i, line) in lines.iter().enumerate() {
        let text = div().flex_1().min_w(px(0.)).child(line.clone());
        column = match marker {
            Marker::None => column.child(text),
            Marker::Number | Marker::Dash => column.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_color(t.ink3)
                            .child(match marker {
                                Marker::Number => format!("{}.", i + 1),
                                _ => "–".to_string(),
                            }),
                    )
                    .child(text),
            ),
        };
    }
    column
}

/// How the session ended: a filled, half or empty mark, plus words.
fn outcome_mark(summary: &Summary, with_open_ends: bool, t: &Theme) -> Div {
    let icon = match summary.outcome {
        Outcome::Done => "outcome-done.svg",
        Outcome::Partial => "outcome-partial.svg",
        Outcome::Stalled => "outcome-stalled.svg",
    };
    let mut words = summary.outcome.label().to_string();
    if with_open_ends {
        match summary.open_ends.len() {
            0 => {}
            1 => words.push_str(", 1 open end"),
            n => words.push_str(&format!(", {n} open ends")),
        }
    }
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(7.))
        .font_family(UI_FONT)
        .text_size(px(13.))
        .text_color(t.ink2)
        .child(svg().path(icon).size(px(11.)).flex_shrink_0().text_color(t.ink))
        .child(words)
}

fn nav_row(t: &Theme, active: bool) -> Div {
    let fill = t.fill;
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .h(px(32.))
        .px(px(10.))
        .rounded(px(7.))
        .text_size(px(13.))
        .text_color(if active { t.ink } else { t.ink2 })
        .when(active, |el| el.bg(fill).font_weight(FontWeight::MEDIUM))
        .cursor_pointer()
        .hover(move |style| style.bg(fill))
}

fn count_badge(count: usize, t: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .font_family(MONO_FONT)
        .text_size(px(11.))
        .text_color(t.ink3)
        .child(count.to_string())
}

fn logo(harness: Harness, t: &Theme, size: f32) -> impl IntoElement {
    let (path, color): (&'static str, Hsla) = match harness {
        Harness::ClaudeCode => ("claude-code.svg", claude_orange()),
        Harness::Codex => ("codex.svg", t.ink),
        Harness::OpenCode => ("opencode.svg", t.ink),
        Harness::Pi => ("pi.svg", t.ink),
    };
    svg()
        .path(path)
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
}

/// The line of small facts under a session: harness, project, branch, time.
fn facts(session: &Session, t: &Theme, with_time: bool) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .font_family(MONO_FONT)
        .text_size(px(12.))
        .text_color(t.ink3)
        .child(logo(session.harness, t, 14.));
    if !session.project().is_empty() {
        row = row.child(session.project().to_string());
    }
    // A detached checkout reports its branch as HEAD, which says nothing.
    if let Some(branch) = session
        .branch
        .as_deref()
        .filter(|b| !b.is_empty() && *b != "HEAD")
    {
        row = row.child(div().min_w(px(0.)).truncate().child(branch.to_string()));
    }
    if with_time {
        row = row.child(format!(
            "{} {}",
            day_label(session.updated_ms).to_lowercase(),
            clock(session.updated_ms)
        ));
    }
    row.child(duration_label(session.duration_ms()))
}

/// The title with fuzzy-matched characters picked out in the accent colour.
fn title_text(title: &str, matches: &[u32], t: &Theme) -> StyledText {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    if !matches.is_empty() {
        for (char_ix, (byte_ix, ch)) in title.char_indices().enumerate() {
            if matches.binary_search(&(char_ix as u32)).is_err() {
                continue;
            }
            let end = byte_ix + ch.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == byte_ix => last.end = end,
                _ => ranges.push(byte_ix..end),
            }
        }
    }
    let style = HighlightStyle {
        color: Some(t.accent),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    StyledText::new(SharedString::from(title.to_string()))
        .with_highlights(ranges.into_iter().map(|range| (range, style)))
}

fn local(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

fn clock(ms: i64) -> String {
    local(ms)
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

fn day_label(ms: i64) -> String {
    let Some(then) = local(ms) else {
        return String::new();
    };
    let days = (Local::now().date_naive() - then.date_naive()).num_days();
    match days {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => then.format("%A").to_string(),
        _ => then.format("%-d %b %Y").to_string(),
    }
}

fn duration_label(ms: i64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "under a minute".to_string(),
        1..=59 => format!("{minutes} min"),
        _ => format!("{} h {} min", minutes / 60, minutes % 60),
    }
}

fn compact_number(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.0}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// A shell command that reopens the session in its own harness.
fn resume_command(session: &Session) -> String {
    let dir = format!("'{}'", session.cwd.replace('\'', "'\\''"));
    let resume = match session.harness {
        Harness::ClaudeCode => format!("claude --resume {}", session.id),
        Harness::Codex => format!("codex resume {}", session.id),
        Harness::OpenCode => format!("opencode --session {}", session.id),
        Harness::Pi => format!("pi --session {}", session.id),
    };
    format!("cd {dir} && {resume}")
}

// `App` is only named in signatures GPUI infers; keep the import honest.
#[allow(dead_code)]
fn _uses(_: &App) {}
