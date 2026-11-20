//! The home view: a sidebar of filters and a feed of sessions, newest first,
//! with type-to-search over everything.

mod activity;
mod first_run;
mod reader;

use crate::app_menu::{CopyResume, ReadTranscript, Reindex, ShowActivity, ShowSessions, WriteSummary};
use crate::settings::{self, Settings, Tab};
use crate::theme::{text, claude_orange, Theme, MONO_FONT, UI_FONT};
use crate::ui::{self, OneLine, Reachable};
use reader::Reading;
use chrono::{DateTime, Local, TimeZone};
use gpui::{
    div, list, prelude::*, px, svg, AnyElement, App, ClipboardItem, Context, Div, FocusHandle,
    FontWeight, HighlightStyle, Hsla, KeyDownEvent, ListAlignment, ListState, SharedString,
    StyledText, Window,
};
use scrollback_core::search::{Hit, Searcher};
use scrollback_core::{
    summarize, Activity, Config, Digest, Harness, MessageHit, Origin, Outcome, Session, Store,
    Summary,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the window looks for new or changed sessions.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// How many summaries are written at once.
const SUMMARIES_AT_ONCE: usize = 2;
/// How long to hold off after the Claude plan limit is reported.
const LIMIT_PAUSE: Duration = Duration::from_secs(30 * 60);

/// Read the sessions the index already holds, without looking at the
/// harnesses again. Instant, and possibly a minute or so behind.
pub fn load_saved() -> Vec<Session> {
    let hidden = Config::load().hidden_sources;
    let mut sessions = Store::open()
        .and_then(|store| store.sessions(false))
        .unwrap_or_default();
    sessions.retain(|s| !hidden.iter().any(|slug| slug == s.origin.slug()));
    sessions
}

/// Refresh the index and read every session from it.
pub fn load() -> (Vec<Session>, SharedString) {
    let loaded = Store::open().and_then(|mut store| {
        let report = store.refresh()?;
        Ok((store.sessions(false)?, report))
    });
    match loaded {
        Ok((mut sessions, report)) => {
            // Sources switched off in the settings are left out entirely.
            let hidden = Config::load().hidden_sources;
            sessions.retain(|s| !hidden.iter().any(|slug| slug == s.origin.slug()));
            let status = format!(
                "{} sessions\nindexed in {:.0?}",
                sessions.len(),
                report.elapsed
            );
            (sessions, status.into())
        }
        Err(err) => (
            Vec::new(),
            format!("Could not read the index: {err:#}. Press ⌘R to try again.").into(),
        ),
    }
}

/// What the main column is showing.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Sessions,
    Activity,
}

enum Row {
    Header(SharedString),
    /// An index into `Home::hits`.
    Session(usize),
    /// An index into `Home::message_hits`.
    Message(usize),
}

pub struct Home {
    sessions: Vec<Session>,
    hits: Vec<Hit>,
    /// Messages matching the query. A selection index past the end of `hits`
    /// refers into this list.
    message_hits: Vec<MessageHit>,
    /// Session key to index in `sessions`.
    by_key: HashMap<String, usize>,
    /// The index database, for message search.
    store: Option<Store>,
    rows: Vec<Row>,
    query: String,
    /// The source the sidebar is filtering to.
    origin: Option<Origin>,
    project: Option<String>,
    selected: Option<usize>,
    /// While searching, results stay compact so more of them fit; this is
    /// set when the selected one has been opened into its full card.
    expanded: bool,
    copied: Option<usize>,
    /// Scroll positions, kept so each area can draw a scrollbar.
    sidebar_scroll: gpui::ScrollHandle,
    activity_scroll: gpui::ScrollHandle,
    first_run_scroll: gpui::ScrollHandle,
    measured: ui::Measured,
    /// Whether the search caret is in the lit half of its blink.
    caret_lit: bool,
    /// When a key last changed the search, so the caret stays lit while typing.
    typed_at: Instant,
    /// Whether this is the window the keyboard is going to.
    window_active: bool,
    /// Showing the first-run screen, which holds everything else back.
    first_run: bool,
    /// Whether the `claude` command, which writes summaries, is installed.
    claude_found: bool,
    view: View,
    /// The period the activity view covers, in days.
    range_days: u32,
    /// Totals for that period, worked out when the activity view is showing.
    activity: Activity,
    /// The paragraph saved for that period, which may be out of date.
    digest: Option<Digest>,
    digest_busy: bool,
    /// The activity fingerprint the last failed digest was for, so the same
    /// request is not sent again unasked.
    digest_failed: Option<u64>,
    digest_error: Option<String>,
    /// The transcript being read, when the reader is open.
    reading: Option<Reading>,
    /// Keys of sessions whose summary is being written right now.
    busy: HashSet<String>,
    /// The last summary error per session key.
    failed: HashMap<String, String>,
    config: Config,
    /// Set when the Claude plan limit was hit; no summaries start before it.
    paused_until: Option<Instant>,
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
        let config = cx.global::<Settings>().0.clone();
        cx.observe_global::<Settings>(|this, cx| this.settings_changed(cx)).detach();
        // Development aids: `SCROLLBACK_FIRST_RUN=1` shows the first-run
        // screen again and `=0` skips it without saving anything, and
        // `SCROLLBACK_NO_CLAUDE=1` behaves as if the `claude` command were
        // missing, so nothing is sent anywhere.
        let first_run = match std::env::var("SCROLLBACK_FIRST_RUN").as_deref() {
            Ok("0") => false,
            Ok(_) => true,
            Err(_) => !config.onboarded,
        };
        let claude_found = std::env::var_os("SCROLLBACK_NO_CLAUDE").is_none()
            && summarize::claude_binary().is_some();
        let mut home = Home {
            sessions,
            hits: Vec::new(),
            message_hits: Vec::new(),
            by_key: HashMap::new(),
            store: Store::open().ok(),
            rows: Vec::new(),
            query,
            origin: None,
            project: None,
            selected: None,
            expanded: false,
            copied: None,
            sidebar_scroll: gpui::ScrollHandle::new(),
            activity_scroll: gpui::ScrollHandle::new(),
            first_run_scroll: gpui::ScrollHandle::new(),
            measured: Default::default(),
            caret_lit: true,
            typed_at: Instant::now(),
            window_active: true,
            first_run,
            claude_found,
            view: View::Sessions,
            range_days: activity::RANGES[0],
            activity: Activity::default(),
            digest: None,
            digest_busy: false,
            digest_failed: None,
            digest_error: None,
            reading: None,
            busy: HashSet::new(),
            failed: HashMap::new(),
            config,
            paused_until: None,
            searcher: Searcher::new(),
            search_time: Duration::ZERO,
            list: ListState::new(0, ListAlignment::Top, px(800.)),
            status,
            focus: cx.focus_handle(),
        };
        if first_run && !claude_found {
            home.config.auto_summarize = false;
        }
        home.recompute();
        home.pump_summaries(cx);

        // Blink the search caret, but only while it is on screen in the
        // window being typed into, so an idle window draws nothing.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(ui::CARET_BLINK).await;
            let alive = this.update(cx, |this, cx| {
                let showing = this.window_active
                    && !this.first_run
                    && this.reading.is_none()
                    && this.view == View::Sessions;
                if showing {
                    this.caret_lit = !this.caret_lit;
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();

        // Look for new and changed sessions for as long as the window lives.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH_EVERY).await;
            let (sessions, status) = cx.background_executor().spawn(async { load() }).await;
            let alive = this.update(cx, |this, cx| this.apply_sessions(sessions, status, cx));
            if alive.is_err() {
                break;
            }
        })
        .detach();

        // Development aid: write the selected session's summary on launch,
        // to exercise the background path without a click.
        if std::env::var_os("SCROLLBACK_DEV_SUMMARIZE").is_some() {
            home.summarize_selected(cx);
        }
        // Development aid: open straight into the selected session's transcript.
        // `SCROLLBACK_DEV_READ=message` opens the first message result instead.
        if let Ok(what) = std::env::var("SCROLLBACK_DEV_READ") {
            if what == "message" && !home.message_hits.is_empty() {
                home.selected = Some(home.hits.len());
            }
            home.open_transcript_now(cx);
        }
        // Development aid: `SCROLLBACK_VIEW=activity` opens on the activity
        // view, and `SCROLLBACK_RANGE=30` picks its period.
        if std::env::var("SCROLLBACK_VIEW").as_deref() == Ok("activity") {
            let days = std::env::var("SCROLLBACK_RANGE").ok().and_then(|d| d.parse().ok());
            home.range_days = days
                .filter(|d| activity::RANGES.contains(d))
                .unwrap_or(home.range_days);
            home.show_activity(cx);
        }
        home
    }

    /// Follow whether the window has the keyboard, so the caret only blinks
    /// in the window that typing goes to.
    pub fn watch_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_active = window.is_window_active();
        cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            this.caret_lit = true;
            cx.notify();
        })
        .detach();
    }

    /// What the view is showing, for the development key-press log.
    pub fn dev_state(&self) -> String {
        format!(
            "first_run={} view={} reading={} query={:?} sessions={} messages={} selected={:?}",
            self.first_run,
            if self.view == View::Activity { "activity" } else { "sessions" },
            self.reading.is_some(),
            self.query,
            self.hits.len(),
            self.message_hits.len(),
            self.selected,
        )
    }

    /// Re-run the search and filters, and rebuild the rows.
    fn recompute(&mut self) {
        let started = Instant::now();
        let mut hits = self.searcher.search(&self.query, &self.sessions);
        hits.retain(|hit| {
            let session = &self.sessions[hit.index];
            self.origin.is_none_or(|o| session.origin == o)
                && self
                    .project
                    .as_deref()
                    .is_none_or(|p| session.project() == p)
        });
        self.hits = hits;

        self.by_key = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, session)| (session.key(), i))
            .collect();
        let grouped_by_day = self.query.trim().is_empty();
        let mut message_hits = match &self.store {
            Some(store) if !grouped_by_day => {
                store.search_messages(&self.query, 40).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        message_hits.retain(|hit| {
            self.by_key.get(&hit.key).is_some_and(|&i| {
                let session = &self.sessions[i];
                self.origin.is_none_or(|o| session.origin == o)
                    && self
                        .project
                        .as_deref()
                        .is_none_or(|p| session.project() == p)
            })
        });
        self.message_hits = message_hits;
        self.search_time = started.elapsed();

        self.rows.clear();
        let mut last_day = String::new();
        if !grouped_by_day && !self.hits.is_empty() {
            self.rows.push(Row::Header("SESSIONS".into()));
        }
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
        if !self.message_hits.is_empty() {
            self.rows.push(Row::Header("IN MESSAGES".into()));
            self.rows
                .extend((0..self.message_hits.len()).map(Row::Message));
        }
        self.selected = (self.selectable() > 0).then_some(0);
        self.expanded = false;
        self.copied = None;
        self.list.reset(self.rows.len());
        if self.view == View::Activity {
            self.refresh_activity();
        }
    }

    /// How many rows can be selected: session results, then message results.
    fn selectable(&self) -> usize {
        self.hits.len() + self.message_hits.len()
    }

    /// The row showing selection index `selection`.
    fn row_of(&self, selection: usize) -> Option<usize> {
        let sessions = self.hits.len();
        self.rows.iter().position(|row| match row {
            Row::Session(hit) => *hit == selection,
            Row::Message(hit) => *hit + sessions == selection,
            Row::Header(_) => false,
        })
    }

    /// The message result that is selected, if the selection is on one.
    fn selected_message(&self) -> Option<&MessageHit> {
        self.selected
            .and_then(|selection| selection.checked_sub(self.hits.len()))
            .and_then(|i| self.message_hits.get(i))
    }

    /// The session the selection belongs to, as an index into `sessions`.
    fn selected_session(&self) -> Option<usize> {
        let selection = self.selected?;
        match self.hits.get(selection) {
            Some(hit) => Some(hit.index),
            None => self
                .selected_message()
                .and_then(|hit| self.by_key.get(&hit.key).copied()),
        }
    }

    fn select(&mut self, hit: usize, cx: &mut Context<Self>) {
        if self.selected == Some(hit) {
            // A second click on a search result opens or closes its card.
            if !self.query.trim().is_empty() {
                self.toggle_expanded(cx);
            }
            return;
        }
        self.expanded = false;
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

    fn toggle_expanded(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.selected else { return };
        self.expanded = !self.expanded;
        if let Some(row) = self.row_of(selection) {
            self.list.splice(row..row + 1, 1);
            self.list.scroll_to_reveal_item(row);
        }
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.selectable() == 0 {
            return;
        }
        let current = self.selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.selectable() as isize - 1) as usize;
        self.select(next, cx);
    }

    fn copy_resume(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_session() else {
            return;
        };
        let session = &self.sessions[index];
        cx.write_to_clipboard(ClipboardItem::new_string(resume_command(session)));
        self.copied = self.selected;
        cx.notify();
    }

    fn reindex(&mut self, cx: &mut Context<Self>) {
        let (sessions, status) = load();
        // A manual reindex is also a request to try again.
        self.failed.clear();
        self.digest_failed = None;
        self.digest_error = None;
        self.paused_until = None;
        self.sessions = sessions;
        self.status = status;
        self.recompute();
        self.pump_summaries(cx);
        self.pump_digest(cx);
        cx.notify();
    }

    /// Take in a fresh read of the index. When nothing has changed the view
    /// is left exactly as it is; otherwise the selection and scroll position
    /// are carried over.
    fn apply_sessions(&mut self, sessions: Vec<Session>, status: SharedString, cx: &mut Context<Self>) {
        let fingerprint = |list: &[Session]| -> Vec<(String, i64, bool, Option<i64>)> {
            list.iter()
                .map(|s| (s.key(), s.updated_ms, s.summary.is_some(), s.summary_for_ms))
                .collect()
        };
        self.status = status;
        if fingerprint(&sessions) != fingerprint(&self.sessions) {
            let selected_key = self
                .selected
                .and_then(|selection| self.hits.get(selection))
                .map(|hit| self.sessions[hit.index].key());
            let reading_key = self
                .reading
                .as_ref()
                .map(|reading| self.sessions[reading.index].key());
            let scroll = self.list.logical_scroll_top();
            let expanded = self.expanded;

            self.sessions = sessions;
            self.recompute();

            let restored = selected_key.and_then(|key| {
                self.hits
                    .iter()
                    .position(|hit| self.sessions[hit.index].key() == key)
            });
            if let Some(selection) = restored {
                self.selected = Some(selection);
                self.expanded = expanded;
            }
            self.list.scroll_to(scroll);
            if let Some(key) = reading_key {
                match self.by_key.get(&key).copied() {
                    Some(index) => {
                        if let Some(reading) = self.reading.as_mut() {
                            reading.index = index;
                        }
                    }
                    None => self.reading = None,
                }
            }
        } else if self.view == View::Activity {
            // The day may have turned over even though no session changed.
            self.refresh_activity();
        }
        self.pump_summaries(cx);
        self.pump_digest(cx);
        cx.notify();
    }

    /// Start summaries for sessions that are due one, newest first, a couple
    /// at a time: those gone quiet, and those in use that have moved on.
    /// Does nothing when the setting is off or the plan limit was reported
    /// recently.
    fn pump_summaries(&mut self, cx: &mut Context<Self>) {
        if self.first_run || !self.claude_found || !self.config.auto_summarize {
            return;
        }
        if self.paused_until.is_some_and(|until| Instant::now() < until) {
            return;
        }
        self.paused_until = None;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        while self.busy.len() < SUMMARIES_AT_ONCE {
            let next = self.sessions.iter().position(|s| {
                summarize::due(s, &self.config, now_ms)
                    && !self.busy.contains(&s.key())
                    && !self.failed.contains_key(&s.key())
            });
            match next {
                Some(index) => self.start_summary(index, cx),
                None => break,
            }
        }
    }

    fn toggle_auto_summarize(&mut self, cx: &mut Context<Self>) {
        self.config.auto_summarize = !self.config.auto_summarize;
        settings::store(self.config.clone(), cx);
        self.pump_summaries(cx);
        self.pump_digest(cx);
        cx.notify();
    }

    /// Take in settings changed elsewhere, such as in the settings window.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let new = cx.global::<Settings>().0.clone();
        if new == self.config {
            return;
        }
        // Which sources show, and where they are read from, decide what the
        // index holds, so those need the sessions read again.
        let reread =
            new.hidden_sources != self.config.hidden_sources || new.folders != self.config.folders;
        self.config = new;
        if reread {
            cx.spawn(async move |this, cx| {
                let (sessions, status) = cx.background_executor().spawn(async { load() }).await;
                this.update(cx, |this, cx| this.apply_sessions(sessions, status, cx))
                    .ok();
            })
            .detach();
        }
        self.pump_summaries(cx);
        self.pump_digest(cx);
        cx.notify();
    }

    /// The menu bar's commands that act on this window. During the first
    /// run they do nothing, as there is nothing behind it to act on yet.
    fn commands(root: Div, cx: &mut Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &Reindex, _, cx| {
            if !this.first_run {
                this.reindex(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &ShowSessions, _, cx| {
            if !this.first_run {
                this.show_sessions(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &ShowActivity, _, cx| {
            if !this.first_run {
                this.show_activity(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &WriteSummary, _, cx| {
            if !this.first_run && this.view == View::Sessions {
                this.summarize_selected(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &ReadTranscript, _, cx| {
            if !this.first_run && this.view == View::Sessions && this.reading.is_none() {
                this.open_transcript(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &CopyResume, _, cx| {
            if !this.first_run && this.view == View::Sessions {
                this.copy_resume(cx);
            }
        }))
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ui::control_key(event, &self.focus, window, cx) {
            return;
        }
        // Keys this window acts on stop here, so they are not also offered to
        // the system's text input. Command shortcuts belong to the menu bar
        // and arrive as its commands; only command-backspace is handled here.
        let ours = !event.keystroke.modifiers.platform || event.keystroke.key == "backspace";
        if ours {
            cx.stop_propagation();
            self.typed_at = Instant::now();
        }
        if self.first_run {
            self.first_run_key(event, cx);
            return;
        }
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        if self.reading.is_some() {
            // The reader has its own small set of keys; typing does nothing.
            match keystroke.key.as_str() {
                "escape" | "left" => self.close_transcript(cx),
                "enter" => self.copy_resume(cx),
                "]" => self.move_step(1, cx),
                "[" => self.move_step(-1, cx),
                "o" if !mods.platform => self.toggle_current_step(cx),
                _ => {}
            }
            return;
        }
        if self.view == View::Activity {
            match keystroke.key.as_str() {
                "escape" => self.show_sessions(cx),
                "left" => self.step_range(-1, cx),
                "right" => self.step_range(1, cx),
                _ => {
                    // Typing starts a search, which belongs to the feed.
                    let types = !(mods.platform || mods.control || mods.function)
                        && keystroke.key_char.as_deref().is_some_and(|typed| {
                            !typed.is_empty() && !typed.chars().any(char::is_control)
                        });
                    if types {
                        self.view = View::Sessions;
                        self.query.push_str(keystroke.key_char.as_deref().unwrap_or(""));
                        self.recompute();
                        cx.notify();
                    }
                }
            }
            return;
        }
        match keystroke.key.as_str() {
            "right" => self.open_transcript(cx),
            // Tab moves between controls, so opening a search result into
            // its full card is on the space bar when nothing is being typed
            // mid-word: shift-space, to keep plain space for the search.
            "space" if mods.shift => self.toggle_expanded(cx),
            "down" => self.move_selection(1, cx),
            "up" => self.move_selection(-1, cx),
            // On a message result, enter goes to that message.
            "enter" if self.selected_message().is_some() => self.open_transcript(cx),
            "enter" => self.copy_resume(cx),
            "escape" => {
                if self.query.is_empty() {
                    self.origin = None;
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
        let view_icon = |path: &'static str, active: bool| {
            svg()
                .path(path)
                .size(px(16.))
                .flex_shrink_0()
                .text_color(if active { t.ink } else { t.ink2 })
        };
        let on_sessions = self.view == View::Sessions;
        let views = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                nav_row(t, on_sessions)
                    .id("view-sessions")
                    .on_click(cx.listener(|this, _, _, cx| this.show_sessions(cx)))
                    .child(view_icon("view-sessions.svg", on_sessions))
                    .child("Sessions"),
            )
            .child(
                nav_row(t, !on_sessions)
                    .id("view-activity")
                    .on_click(cx.listener(|this, _, _, cx| this.show_activity(cx)))
                    .child(view_icon("view-activity.svg", !on_sessions))
                    .child("Activity"),
            );

        // One row for each program sessions were run from. The command-line
        // harnesses are always listed; apps are listed once they have
        // sessions, so the list stays short as more are supported.
        let mut harnesses = div().flex().flex_col().gap(px(2.)).child(div().px(px(10.)).child(label("Source", t)));
        for origin in Origin::ALL {
            let count = self.sessions.iter().filter(|s| s.origin == origin).count();
            let active = self.origin == Some(origin);
            if origin.is_app() && count == 0 && !active {
                continue;
            }
            harnesses = harnesses.child(
                nav_row(t, active)
                    .id(origin.slug())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.origin = (this.origin != Some(origin)).then_some(origin);
                        this.recompute();
                        this.pump_digest(cx);
                        cx.notify();
                    }))
                    .child(logo(origin, t, 16.))
                    .child(div().flex_1().min_w(px(0.)).one_line().child(origin.name()))
                    .child(count_badge(count, t)),
            );
        }

        // Projects, most recently used first: sessions arrive newest first,
        // so a project's place is that of its latest session.
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
        let mut projects = div().flex().flex_col().gap(px(2.)).child(div().px(px(10.)).child(label("Project", t)));
        for (i, (name, count)) in counts.into_iter().take(60).enumerate() {
            let active = self.project.as_deref() == Some(name.as_str());
            let clicked = name.clone();
            projects = projects.child(
                nav_row(t, active)
                    .id(("project", i))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let same = this.project.as_deref() == Some(clicked.as_str());
                        this.project = (!same).then(|| clicked.clone());
                        this.recompute();
                        this.pump_digest(cx);
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w(px(0.)).one_line().child(name))
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
            .child(views)
            // The lists scroll on their own, so the settings and status
            // below stay in view however many sources and projects there are.
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .id("sidebar-lists")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar_scroll)
                            .flex()
                            .flex_col()
                            .gap(px(22.))
                            .child(harnesses)
                            .child(projects),
                    )
                    // In the sidebar's own margin, clear of the counts.
                    .children(ui::scrollbar(&self.sidebar_scroll, t).map(|bar| bar.right(px(-7.)))),
            )
            .child(
                nav_row(t, false)
                    .id("open-settings")
                    .on_click(cx.listener(|_, _, _, cx| settings::open(cx, Tab::General)))
                    .child(div().flex_1().child("Settings"))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(text::CAPTION))
                            .text_color(t.ink3)
                            .child("⌘,"),
                    ),
            )
            .child(
                nav_row(t, false)
                    .id("auto-summarise")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_auto_summarize(cx)))
                    .child(div().flex_1().child("Summarise automatically"))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(text::CAPTION))
                            .text_color(t.ink3)
                            .child(if self.config.auto_summarize { "on" } else { "off" }),
                    ),
            )
            .child(
                div()
                    .px(px(10.))
                    .font_family(MONO_FONT)
                    .text_size(px(text::CAPTION))
                    .line_height(px(17.))
                    .text_color(t.ink3)
                    .child(self.status.clone())
                    .children(self.summary_status()),
            )
    }

    /// A line about summary work in progress, when there is something to say.
    fn summary_status(&self) -> Option<String> {
        if let Some(until) = self.paused_until.filter(|until| Instant::now() < *until) {
            // Say when it resumes by itself and how to try sooner.
            let resumes = Local::now() + chrono::Duration::from_std(until - Instant::now()).unwrap_or_default();
            return Some(format!(
                "summaries paused until {}: Claude plan limit reached. ⌘R tries again now.",
                resumes.format("%H:%M")
            ));
        }
        match self.busy.len() {
            0 if self.digest_busy => Some("writing the digest…".to_string()),
            0 => None,
            1 => Some("writing 1 summary…".to_string()),
            n => Some(format!("writing {n} summaries…")),
        }
    }

    /// Drop the harness and project filters, keeping what was typed.
    fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.origin = None;
        self.project = None;
        self.recompute();
        self.pump_digest(cx);
        cx.notify();
    }

    /// The filters in force, as words: "Codex, runyard".
    fn filter_words(&self) -> Option<String> {
        let parts: Vec<&str> = self
            .origin
            .map(|o| o.name())
            .into_iter()
            .chain(self.project.as_deref())
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    fn search_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let searching = !self.query.is_empty();
        // The caret shows that typing anywhere in the window lands here.
        // It blinks, holds steady while keys are arriving, and is hidden in
        // a window that does not have the keyboard.
        let lit = self.window_active && (self.caret_lit || self.typed_at.elapsed() < ui::CARET_BLINK);
        let text = ui::search_field(
            &self.query,
            "Search sessions, projects and messages",
            lit,
            t,
        );
        // A filter narrows every search, so it is shown where the search is.
        let filter = self.filter_words().map(|words| {
            let fill = t.fill;
            div()
                .id("clear-filters")
                    .reachable(t)
                .flex_shrink_0()
                .h(px(26.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(7.))
                // The box is rounded 10 and pads the chip by 7.
                .rounded(px(3.))
                .bg(t.raise)
                .border_1()
                .border_color(t.line)
                .text_size(px(text::CAPTION))
                .text_color(t.ink2)
                .cursor_pointer()
                .hover(move |style| style.bg(fill))
                .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx)))
                .child(format!("in {words}"))
                .child(div().text_color(t.ink3).child("×"))
        });
        let hint = if searching {
            format!(
                "{} sessions · {} messages · {:.1?}   ⇧space expand",
                self.hits.len(),
                self.message_hits.len(),
                self.search_time
            )
        } else {
            "↑↓ move   → read   ↵ copy resume   ⌘S summarise   ⌘2 activity".to_string()
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
                        .children(filter)
                        .child(div().flex_1().min_w(px(0.)).child(text))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(MONO_FONT)
                                .text_size(px(text::CAPTION))
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
            Some(Row::Message(hit)) => self.message_row(*hit, ix, &t, cx).into_any_element(),
            Some(Row::Session(hit)) => {
                let hit = *hit;
                let as_card = self.selected == Some(hit)
                    && (self.query.trim().is_empty() || self.expanded);
                if as_card {
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
        let Some(index) = self.selected_session() else {
            return;
        };
        // Asking by hand overrides an earlier failure or pause.
        self.failed.remove(&self.sessions[index].key());
        self.paused_until = None;
        self.start_summary(index, cx);
    }

    /// Write a summary for one session in the background, unless one is
    /// already being written for it.
    fn start_summary(&mut self, index: usize, cx: &mut Context<Self>) {
        let session = self.sessions[index].clone();
        let key = session.key();
        if !self.busy.insert(key.clone()) {
            return;
        }
        // What the summary will cover; the session may move on meanwhile.
        let covered_ms = session.updated_ms;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let work = cx.background_executor().spawn(async move {
                let written = summarize::summarize(&session)?;
                Store::open()?.set_summary(&session, &written.summary)?;
                anyhow::Ok(written.summary)
            });
            let result = work.await;
            this.update(cx, |this, cx| this.summary_arrived(key, covered_ms, result, cx))
                .ok();
        })
        .detach();
    }

    fn summary_arrived(
        &mut self,
        key: String,
        covered_ms: i64,
        result: anyhow::Result<Summary>,
        cx: &mut Context<Self>,
    ) {
        self.busy.remove(&key);
        let index = self.sessions.iter().position(|s| s.key() == key);
        match result {
            Ok(summary) => {
                eprintln!("summary written for {key}: {}", summary.title);
                if let Some(index) = index {
                    let session = &mut self.sessions[index];
                    session.summary = Some(summary);
                    session.summary_for_ms = Some(covered_ms);
                    session.summary_stale = covered_ms < session.updated_ms;
                }
            }
            Err(err) => {
                let message = format!("{err:#}");
                eprintln!("summary failed for {key}: {message}");
                // The plan limit affects every request, so stop asking for a
                // while instead of failing each session in turn.
                if message.to_lowercase().contains("limit") {
                    self.paused_until = Some(Instant::now() + LIMIT_PAUSE);
                }
                self.failed.insert(key, message);
            }
        }
        // The row's height has changed, so have the list measure it again.
        let row = index
            .and_then(|index| self.hits.iter().position(|hit| hit.index == index))
            .and_then(|hit| self.row_of(hit));
        if let Some(row) = row {
            self.list.splice(row..row + 1, 1);
        }
        if self.view == View::Activity {
            self.refresh_activity();
        }
        // A transcript open on this session takes up the new steps.
        if let Some(index) = index {
            self.regroup_transcript(index);
        }
        self.pump_summaries(cx);
        self.pump_digest(cx);
        cx.notify();
    }

    /// One message that matched the search: who said it, the text around the
    /// match, and which session it is in. Opens the transcript at that message.
    fn message_row(&self, i: usize, ix: usize, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let hit = &self.message_hits[i];
        let selection = self.hits.len() + i;
        let selected = self.selected == Some(selection);
        let fill = t.fill;
        let highlight = HighlightStyle {
            // Marked with weight and a tint; the accent colour itself is
            // kept for things that can be acted on.
            font_weight: Some(FontWeight::BOLD),
            background_color: Some(t.accent.opacity(0.18)),
            ..Default::default()
        };
        let text = StyledText::new(SharedString::from(hit.snippet.clone()))
            .with_highlights(hit.matches.iter().cloned().map(|range| (range, highlight)));

        let mut origin = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(text::CAPTION))
            .text_color(t.ink3);
        if let Some(session) = self.by_key.get(&hit.key).map(|&i| &self.sessions[i]) {
            origin = origin
                .child(logo(session.origin, t, 12.))
                .child(
                    div()
                        .min_w(px(0.))
                        .one_line()
                        .child(format!("in {}", session.display_title())),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .child(day_label(session.updated_ms).to_lowercase()),
                );
        }

        div()
            .id(("message", ix))
            .flex()
            .gap(px(20.))
            .px(px(20.))
            .py(px(10.))
            .rounded(px(10.))
            .cursor_pointer()
            .when(selected, |el| el.bg(fill))
            .hover(move |style| style.bg(fill))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(selection, cx);
                this.open_transcript(cx);
            }))
            .child(
                div()
                    .w(px(44.))
                    .flex_shrink_0()
                    .pt(px(2.))
                    .font_family(MONO_FONT)
                    .text_size(px(text::CAPTION))
                    .text_color(t.ink3)
                    .child(if hit.from_user { "you" } else { "agent" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().text_color(t.ink2).line_clamp(3).text_ellipsis().child(text))
                    .child(origin),
            )
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
        let selected = self.selected == Some(hit);
        let fill = t.fill;
        div()
            .id(("session", ix))
            .flex()
            .gap(px(20.))
            .px(px(20.))
            .py(px(14.))
            .rounded(px(10.))
            .when(selected, |el| el.bg(fill))
            .cursor_pointer()
            .hover(|style| style.bg(t.fill.opacity(0.5)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(hit, cx)))
            .child(
                div()
                    .w(px(44.))
                    .flex_shrink_0()
                    .pt(px(2.))
                    .font_family(MONO_FONT)
                    .text_size(px(text::CAPTION))
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
                                    .text_size(px(text::LEAD))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .one_line()
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
                        el.child(div().text_color(t.ink2).line_clamp(2).text_ellipsis().child(blurb))
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
            .text_size(px(text::CAPTION))
            .text_color(t.ink3)
            .child(format!("{} prompts", session.user_turns))
            .child(format!("{} replies", session.assistant_turns))
            .child(format!("{} tool calls", session.tool_calls))
            .child(format!("{} tokens", compact_number(session.tokens())));
        // Context re-read from the cache on every turn; far larger than the
        // work itself, so it is shown apart.
        if session.tokens_cached > 0 {
            stats = stats.child(format!("{} cached", compact_number(session.tokens_cached)));
        }
        if let Some(cost) = session.cost_usd {
            stats = stats.child(format!("${cost:.2}"));
        }
        if let Some(model) = &session.model {
            stats = stats.child(model.clone());
        }

        let mut card = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(18.))
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
                    .text_size(px(text::HEADLINE))
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
                        .text_size(px(text::LEAD))
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
                            .child(section("Open ends", &summary.open_ends, Marker::Dash, t))
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
                        .text_size(px(text::CAPTION))
                        .text_color(t.ink2);
                    for file in &summary.files[..shown] {
                        files = files.child(div().one_line().child(file.clone()));
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
                                .text_size(px(text::LEAD))
                                .line_height(px(23.))
                                .text_color(t.ink2)
                                .line_clamp(6).text_ellipsis()
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
                    .reachable(t)
                    .h(px(36.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(t.accent)
                    .text_color(t.on_accent)
                    .text_size(px(text::SMALL))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.9))
                    .active(|style| style.opacity(0.75))
                    .on_click(cx.listener(|this, _, _, cx| this.copy_resume(cx)))
                    .child(if copied {
                        "Copied to clipboard".to_string()
                    } else {
                        match session.harness {
                            // Cursor has no way to reopen one conversation.
                            Harness::Cursor => "Copy command to open the project in Cursor".to_string(),
                            harness => format!("Copy resume command for {}", harness.name()),
                        }
                    }),
            )
            .child(
                div()
                    .id("read-transcript")
                    .reachable(t)
                    .h(px(36.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(text::SMALL))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.fill))
                    .active(|style| style.opacity(0.75))
                    .on_click(cx.listener(|this, _, _, cx| this.open_transcript(cx)))
                    .child("Read transcript"),
            )
            .child(
                div()
                    .id("write-summary")
                    .reachable(t)
                    .h(px(36.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(text::SMALL))
                    .font_weight(FontWeight::MEDIUM)
                    // While a summary is being written the button is plainly
                    // not available: dimmed, and it does nothing.
                    .when(busy, |el| el.text_color(t.ink3).opacity(0.6))
                    .when(!busy, |el| {
                        el.cursor_pointer()
                            .hover(|style| style.bg(t.fill))
                            .active(|style| style.opacity(0.75))
                            .on_click(cx.listener(|this, _, _, cx| this.summarize_selected(cx)))
                    })
                    .child(summary_action),
            );
        if let Some(covers) = covers(session).filter(|_| !busy) {
            actions = actions.child(
                div()
                    .text_size(px(text::CAPTION))
                    .text_color(t.ink3)
                    .child(covers),
            );
        }

        card = card
            .child(stats)
            .child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(text::CAPTION))
                    .text_color(t.ink3)
                    .id("card-folder")
                    .tooltip(ui::tip(session.cwd.clone()))
                    .one_line()
                    .child(ui::middle(&session.cwd, 96)),
            )
            .child(actions);
        if let Some(error) = self.failed.get(&key) {
            card = card.child(
                div()
                    .text_size(px(text::SMALL))
                    .text_color(t.ink2)
                    .child(format!("The summary could not be written: {error}")),
            );
        }
        // The same gutter the rows around it have, with the time in it, so
        // the title keeps its place as the selection moves.
        div().py(px(8.)).child(
            div()
                .flex()
                .gap(px(20.))
                .pl(px(20.))
                .pr(px(30.))
                .py(px(26.))
                .rounded(px(14.))
                .bg(t.raise)
                .shadow(t.card_shadow.clone())
                .child(
                    div()
                        .w(px(44.))
                        .flex_shrink_0()
                        .pt(px(1.))
                        .font_family(MONO_FONT)
                        .text_size(px(text::CAPTION))
                        .text_color(t.ink3)
                        .child(clock(session.updated_ms)),
                )
                .child(card),
        )
    }
}

impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        ui::settle(
            &[&self.sidebar_scroll, &self.activity_scroll, &self.first_run_scroll],
            &self.measured,
            window,
            cx,
        );
        if self.first_run {
            return Self::commands(div(), cx)
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::on_key))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, _| ui::pointer_used(&this.focus, window)),
                )
                .size_full()
                .bg(t.bg)
                .text_color(t.ink)
                .font_family(UI_FONT)
                .text_size(px(text::BODY))
                .line_height(px(20.))
                .child(self.first_run_view(&t, cx));
        }
        let feed: AnyElement = if self.rows.is_empty() {
            let filtered = self.filter_words();
            let message = if self.sessions.is_empty() {
                "No sessions found on this Mac yet.".to_string()
            } else {
                match (&filtered, self.query.trim().is_empty()) {
                    (Some(words), true) => format!("No sessions in {words}."),
                    (Some(words), false) => format!("Nothing matches in {words}."),
                    (None, _) => "Nothing matches. Press esc to clear.".to_string(),
                }
            };
            let fill = t.fill;
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .text_color(t.ink3)
                .child(message)
                .children(filtered.is_some().then(|| {
                    div()
                        .id("search-everywhere")
                    .reachable(&t)
                        .h(px(34.))
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(t.line)
                        .text_size(px(text::SMALL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.ink)
                        .cursor_pointer()
                        .hover(move |style| style.bg(fill))
                        .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx)))
                        .child("Search everywhere")
                }))
                .into_any_element()
        } else {
            list(
                self.list.clone(),
                cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx)),
            )
            .size_full()
            .into_any_element()
        };

        Self::commands(div(), cx)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| ui::pointer_used(&this.focus, window)),
            )
            .size_full()
            .flex()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
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
                    .when(self.reading.is_none() && self.view == View::Activity, |el| {
                        el.child(self.activity_view(&t, cx))
                    })
                    .when(self.reading.is_none() && self.view == View::Sessions, |el| {
                        el.child(self.search_bar(&t, cx))
                            .child(div().flex_1().min_h(px(0.)).child(feed))
                    }),
            )
    }
}

pub(crate) fn label(text: impl Into<SharedString>, t: &Theme) -> Div {
    let text: SharedString = text.into();
    div()
        .pb(px(6.))
        .text_size(px(text::CAPTION))
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
pub(crate) fn outcome_mark(summary: &Summary, with_open_ends: bool, t: &Theme) -> Div {
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
        .text_size(px(text::SMALL))
        .text_color(t.ink2)
        .child(svg().path(icon).size(px(11.)).flex_shrink_0().text_color(t.ink))
        .child(words)
}

pub(crate) fn nav_row(t: &Theme, active: bool) -> Div {
    let fill = t.fill;
    ui::tab_stop(div(), t)
        .flex()
        .items_center()
        .gap(px(10.))
        .h(px(32.))
        .px(px(10.))
        .rounded(px(7.))
        .text_size(px(text::SMALL))
        .text_color(if active { t.ink } else { t.ink2 })
        // The fill alone is too faint a mark, so the current row also
        // carries a bar of the accent colour and heavier text.
        .border_l_2()
        .border_color(if active { t.accent } else { t.accent.opacity(0.) })
        .when(active, |el| el.bg(fill).font_weight(FontWeight::MEDIUM))
        .cursor_pointer()
        .hover(move |style| style.bg(fill))
}

fn count_badge(count: usize, t: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .font_family(MONO_FONT)
        .text_size(px(text::CAPTION))
        .text_color(t.ink3)
        .child(count.to_string())
}

pub(crate) fn logo(origin: Origin, t: &Theme, size: f32) -> impl IntoElement {
    let (path, color): (&'static str, Hsla) = match origin {
        Origin::ClaudeCode => ("claude-code.svg", claude_orange()),
        Origin::ClaudeDesktop => ("claude.svg", claude_orange()),
        Origin::Codex => ("codex.svg", t.ink),
        Origin::CodexDesktop => ("openai.svg", t.ink),
        Origin::OpenCode => ("opencode.svg", t.ink),
        Origin::Pi => ("pi.svg", t.ink),
        Origin::Cursor => ("cursor.svg", t.ink),
        Origin::T3Code => ("t3.svg", t.ink),
    };
    svg()
        .path(path)
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
}

/// The line of small facts under a session: harness, project, branch, time.
pub(crate) fn facts(session: &Session, t: &Theme, with_time: bool) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .font_family(MONO_FONT)
        .text_size(px(text::CAPTION))
        .text_color(t.ink3)
        .child(logo(session.origin, t, 14.))
        .child(session.origin.name());
    if !session.project().is_empty() {
        row = row.child(session.project().to_string());
    }
    // A detached checkout reports its branch as HEAD, which says nothing.
    if let Some(branch) = session
        .branch
        .as_deref()
        .filter(|b| !b.is_empty() && *b != "HEAD")
    {
        row = row.child(div().min_w(px(0.)).one_line().child(branch.to_string()));
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
        font_weight: Some(FontWeight::BOLD),
        background_color: Some(t.accent.opacity(0.18)),
        ..Default::default()
    };
    StyledText::new(SharedString::from(title.to_string()))
        .with_highlights(ranges.into_iter().map(|range| (range, style)))
}

fn local(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

pub(crate) fn clock(ms: i64) -> String {
    local(ms)
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// How far a summary reaches, when the session has gone on past it: up to
/// what time today, or just that it is behind.
pub(crate) fn covers(session: &Session) -> Option<String> {
    if !session.summary_stale {
        return None;
    }
    let today = session
        .summary_for_ms
        .filter(|&covered_ms| day_label(covered_ms) == "Today");
    Some(match today {
        Some(covered_ms) => format!("Covers the session up to {}.", clock(covered_ms)),
        None => "The session has moved on since this was written.".to_string(),
    })
}

pub(crate) fn day_label(ms: i64) -> String {
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
pub(crate) fn resume_command(session: &Session) -> String {
    let dir = format!("'{}'", session.cwd.replace('\'', "'\\''"));
    let resume = match session.harness {
        Harness::ClaudeCode => format!("claude --resume {}", session.id),
        Harness::Codex => format!("codex resume {}", session.id),
        Harness::OpenCode => format!("opencode --session {}", session.id),
        // pi takes the session file itself, which every version accepts.
        Harness::Pi => format!("pi --session '{}'", session.source.replace('\'', "'\\''")),
        // Cursor conversations cannot be reopened by id; open the project.
        Harness::Cursor => return format!("cursor {dir}"),
    };
    format!("cd {dir} && {resume}")
}

// `App` is only named in signatures GPUI infers; keep the import honest.
#[allow(dead_code)]
fn _uses(_: &App) {}
