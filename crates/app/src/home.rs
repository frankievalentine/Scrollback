//! The home view: a sidebar of filters and a feed of sessions, newest first,
//! with type-to-search over everything.

mod activity;
mod first_run;
mod reader;

use crate::app_menu::{
    CopyResume, ExportSession, FindInTranscript, ReadTranscript, Reindex, ShowActivity, ShowSessions,
    WriteSummary,
};
use crate::chrome;
use crate::settings::{self, Settings, Tab};
use crate::theme::{text, claude_orange, Theme, mono, UI_FONT};
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
    summarize, sync, Activity, Config, Digest, Harness, MessageHit, Origin, Outcome, Session, Store,
    Summary,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the window looks for new or changed sessions.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// How many summaries are written at once.
const SUMMARIES_AT_ONCE: usize = 2;
/// How long to hold off after the provider reports a usage limit.
const LIMIT_PAUSE: Duration = Duration::from_secs(30 * 60);

/// Read the sessions the index already holds, without looking at the
/// harnesses again. Instant, and possibly a minute or so behind.
pub fn load_saved() -> Vec<Session> {
    let config = Config::load();
    let mine = Store::open()
        .and_then(|store| store.sessions(false))
        .unwrap_or_default();
    let (machines, others) = sync::read(&config);
    *MACHINES.lock().unwrap() = machines;
    shown(sync::merge(mine, others), &config)
}

/// Sources switched off in the settings are left out entirely.
fn shown(mut sessions: Vec<Session>, config: &Config) -> Vec<Session> {
    sessions.retain(|s| !config.hidden_sources.iter().any(|slug| slug == s.origin.slug()));
    sessions
}

/// The other machines in the synced folder, as last read.
static MACHINES: std::sync::Mutex<Vec<sync::Machine>> = std::sync::Mutex::new(Vec::new());

pub fn machines() -> Vec<sync::Machine> {
    MACHINES.lock().unwrap().clone()
}

/// Bring this machine's file in the synced folder up to date, and read the
/// other machines' sessions from it, indexing their messages for search.
/// `mine` is this machine's sessions as just read from the index.
pub fn sync_with(store: &mut Store, mine: &[Session], config: &Config) -> Vec<Session> {
    let (machines, others) = sync::exchange(store, &shown(mine.to_vec(), config), config);
    *MACHINES.lock().unwrap() = machines;
    others
}

/// Refresh the index and read every session from it, and from the other
/// machines.
pub fn load() -> (Vec<Session>, SharedString) {
    let config = Config::load();
    let loaded = Store::open().and_then(|mut store| {
        let report = store.refresh()?;
        sync::restore(&mut store, &config);
        let mine = store.sessions(false)?;
        let others = sync_with(&mut store, &mine, &config);
        Ok((sync::merge(mine, others), report))
    });
    match loaded {
        Ok((sessions, report)) => {
            let sessions = shown(sessions, &config);
            let status = format!(
                "{} sessions\nindexed in {:.0?}",
                sessions.len(),
                report.elapsed
            );
            (sessions, status.into())
        }
        Err(err) => (
            Vec::new(),
            format!("Could not read the index: {err:#}. Press {} to try again.", ui::shortcut("R")).into(),
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
    /// The machine the sidebar is filtering to, by id; empty for this one.
    machine: Option<String>,
    /// The other machines in the synced folder.
    machines: Vec<sync::Machine>,
    /// Sidebar sections folded away, by name.
    folded: HashSet<&'static str>,
    /// Messages found on machines on Tailscale, and the search they answer.
    tailnet_hits: (String, Vec<MessageHit>),
    /// Counts searches sent to Tailscale, so only the latest is taken in.
    tailnet_asked: u64,
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
    /// Which menu is open, where the menus are drawn in the window.
    menu_open: Option<usize>,
    /// A short message about something just done, such as an export.
    notice: Option<String>,
    /// Whether the window is too narrow for the search box's key hints.
    narrow: bool,
    /// Whether the search caret is in the lit half of its blink.
    caret_lit: bool,
    /// When a key last changed the search, so the caret stays lit while typing.
    typed_at: Instant,
    /// Whether this is the window the keyboard is going to.
    window_active: bool,
    /// Showing the first-run screen, which holds everything else back.
    first_run: bool,
    /// Whether the `claude` command, which writes summaries, is installed.
    summaries_ready: bool,
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
    /// Set when the provider reported a usage limit; no summaries start before it.
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
        let summaries_ready = scrollback_core::provider::ready(&config).is_ok();
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
            machine: None,
            machines: machines(),
            folded: HashSet::new(),
            tailnet_hits: (String::new(), Vec::new()),
            tailnet_asked: 0,
            selected: None,
            expanded: false,
            copied: None,
            sidebar_scroll: gpui::ScrollHandle::new(),
            activity_scroll: gpui::ScrollHandle::new(),
            first_run_scroll: gpui::ScrollHandle::new(),
            measured: Default::default(),
            menu_open: chrome::open_at_launch(),
            notice: None,
            narrow: false,
            caret_lit: true,
            typed_at: Instant::now(),
            window_active: true,
            first_run,
            summaries_ready,
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
        if first_run && !summaries_ready {
            home.config.auto_summarize = false;
        }
        home.recompute();
        // Development aid: `SCROLLBACK_DEV_SELECT=2` selects the third row.
        if let Some(n) = std::env::var("SCROLLBACK_DEV_SELECT").ok().and_then(|n| n.parse().ok()) {
            if n < home.selectable() {
                home.selected = Some(n);
            }
        }
        // Development aid: `SCROLLBACK_DEV_MACHINE=devbox` filters to that machine.
        if let Ok(name) = std::env::var("SCROLLBACK_DEV_MACHINE") {
            home.machine = home.machines.iter().find(|m| m.name == name).map(|m| m.id.clone());
            home.recompute();
        }
        home.pump_summaries(cx);
        home.ask_tailnet(cx);

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
            "first_run={} view={} reading={} {}query={:?} sessions={} messages={} selected={:?}",
            self.first_run,
            if self.view == View::Activity { "activity" } else { "sessions" },
            self.reading.is_some(),
            self.reading.as_ref().map(|r| r.dev_state()).unwrap_or_default(),
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
        hits.retain(|hit| self.in_filters(&self.sessions[hit.index]));
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
        if !grouped_by_day && self.tailnet_hits.0 == self.query {
            message_hits.extend(self.tailnet_hits.1.iter().cloned());
        }
        message_hits.retain(|hit| {
            self.by_key
                .get(&hit.key)
                .is_some_and(|&i| self.in_filters(&self.sessions[i]))
        });
        // This machine's first, then each other machine's in a group.
        message_hits.sort_by_cached_key(|hit| self.message_group(hit));
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
        let mut group = None;
        for (i, hit) in self.message_hits.iter().enumerate() {
            let this = self.message_group(hit);
            if group.as_ref() != Some(&this) {
                self.rows.push(Row::Header(match &this.1 {
                    None => "IN MESSAGES".into(),
                    Some(name) => format!("IN MESSAGES ON {}", name.to_uppercase()).into(),
                }));
                group = Some(this);
            }
            self.rows.push(Row::Message(i));
        }
        self.selected = (self.selectable() > 0).then_some(0);
        self.expanded = false;
        self.copied = None;
        self.list.reset(self.rows.len());
        if self.view == View::Activity {
            self.refresh_activity();
        }
    }

    /// Which group a message result is listed in: this machine's, then
    /// another machine's by name.
    fn message_group(&self, hit: &MessageHit) -> (bool, Option<String>) {
        let machine = self
            .by_key
            .get(&hit.key)
            .and_then(|&i| self.sessions[i].machine.as_ref())
            .map(|m| m.name.clone());
        (machine.is_some(), machine)
    }

    /// Search the messages on machines on Tailscale, a moment after typing
    /// stops. Each answers within half a second or is left out; the results
    /// join this machine's when they arrive.
    fn ask_tailnet(&mut self, cx: &mut Context<Self>) {
        if self.config.tailnet_machines.is_empty() || self.query.trim().chars().count() < 3 {
            return;
        }
        self.tailnet_asked += 1;
        let asked = self.tailnet_asked;
        let (query, config) = (self.query.clone(), self.config.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let current = this.read_with(cx, |this, _| this.tailnet_asked == asked).unwrap_or(false);
            if !current {
                return;
            }
            let searched = query.clone();
            let hits = cx
                .background_executor()
                .spawn(async move { scrollback_core::tailnet::search(&config, &searched, 20) })
                .await;
            this.update(cx, |this, cx| {
                if this.tailnet_asked != asked || this.query != query {
                    return;
                }
                let found = hits.len();
                this.tailnet_hits = (query, hits);
                if found > 0 {
                    // Keep the selection where it was.
                    let selected = this.selected;
                    this.recompute();
                    if selected.is_some_and(|s| s < this.selectable()) {
                        this.selected = selected;
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Whether a session is on the machine the sidebar is filtering to.
    fn on_machine(&self, session: &Session) -> bool {
        self.machine.as_deref().is_none_or(|id| machine_id(session) == id)
    }

    /// Whether a session gets through every sidebar filter.
    pub(crate) fn in_filters(&self, session: &Session) -> bool {
        self.on_machine(session)
            && self.origin.is_none_or(|o| session.origin == o)
            && self
                .project
                .as_deref()
                .is_none_or(|p| session.project() == p)
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
        self.machines = machines();
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
        self.machines = machines();
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
        if self.first_run || !self.summaries_ready || !self.config.auto_summarize {
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
        let reread = new.hidden_sources != self.config.hidden_sources
            || new.folders != self.config.folders
            || new.sync_enabled != self.config.sync_enabled
            || new.sync_folder != self.config.sync_folder
            || new.sync_full_messages != self.config.sync_full_messages
            || new.hidden_machines != self.config.hidden_machines
            || new.tailnet_machines != self.config.tailnet_machines;
        self.config = new;
        // A change of provider can make summaries possible, or not.
        self.summaries_ready = scrollback_core::provider::ready(&self.config).is_ok();
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
        .on_action(cx.listener(|this, _: &FindInTranscript, _, cx| {
            if this.first_run || this.view != View::Sessions {
                return;
            }
            // From the list, find opens the selected session to search it.
            if this.reading.is_none() {
                this.open_transcript(cx);
            }
            this.open_find(cx);
        }))
        .on_action(cx.listener(|this, _: &ExportSession, _, cx| {
            if !this.first_run && this.view == View::Sessions {
                this.export_selected(cx);
            }
        }))
    }

    /// Write the selected session out as Markdown, to a file the person
    /// picks, then show it.
    fn export_selected(&mut self, cx: &mut Context<Self>) {
        let index = match &self.reading {
            Some(reading) => reading.index,
            None => match self.selected_session() {
                Some(index) => index,
                None => return,
            },
        };
        let session = self.sessions[index].clone();
        let folder = dirs_downloads();
        let chosen = cx.prompt_for_new_path(&folder, Some(&scrollback_core::export::file_name(&session)));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else { return };
            let written = cx
                .background_executor()
                .spawn(async move {
                    let text = scrollback_core::export::markdown(&session)?;
                    std::fs::write(&path, text)?;
                    anyhow::Ok(path)
                })
                .await;
            this.update(cx, |this, cx| {
                match written {
                    Ok(path) => {
                        this.notice = Some(format!("Exported to {}", path.display()));
                        cx.reveal_path(&path);
                    }
                    Err(err) => this.notice = Some(format!("Could not export: {err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && chrome::close(self, cx) {
            cx.stop_propagation();
            return;
        }
        if ui::control_key(event, &self.focus, window, cx) {
            return;
        }
        // Keys this window acts on stop here, so they are not also offered to
        // the system's text input. Command shortcuts (control ones off the
        // Mac) belong to the menus and arrive as their commands; only the one
        // with backspace is handled here.
        let ours = !event.keystroke.modifiers.secondary() || event.keystroke.key == "backspace";
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
            // While finding, keys type into the find field.
            if self.find_key(keystroke, cx) {
                return;
            }
            // The reader has its own small set of keys; typing does nothing.
            match keystroke.key.as_str() {
                "escape" | "left" => self.close_transcript(cx),
                "enter" => self.copy_resume(cx),
                "]" => self.move_step(1, cx),
                "[" => self.move_step(-1, cx),
                "o" if !mods.secondary() => self.toggle_current_step(cx),
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
                        self.ask_tailnet(cx);
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
                    self.machine = None;
                } else {
                    self.query.clear();
                }
                self.recompute();
                cx.notify();
            }
            "backspace" => {
                if mods.secondary() || mods.alt {
                    self.query.clear();
                } else {
                    self.query.pop();
                }
                self.recompute();
                self.ask_tailnet(cx);
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
                self.ask_tailnet(cx);
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

        // One row for each program sessions were run from. The first
        // command-line harnesses are always listed; the rest are listed once
        // they have sessions, so the list stays short as more are supported.
        // Choosing a machine narrows the other lists' counts to it.
        let here: Vec<&Session> = self.sessions.iter().filter(|s| self.on_machine(s)).collect();
        let mut harnesses = self.section("Source", t, cx);
        for origin in Origin::ALL {
            if self.folded.contains("Source") {
                break;
            }
            let count = here.iter().filter(|s| s.origin == origin).count();
            let active = self.origin == Some(origin);
            if !origin.always_listed() && count == 0 && !active {
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
        for session in &here {
            let name = session.project();
            if name.is_empty() {
                continue;
            }
            match counts.iter_mut().find(|(n, _)| n == name) {
                Some(entry) => entry.1 += 1,
                None => counts.push((name.to_string(), 1)),
            }
        }
        let mut projects = self.section("Project", t, cx);
        let shown = if self.folded.contains("Project") { 0 } else { 60 };
        for (i, (name, count)) in counts.into_iter().take(shown).enumerate() {
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
            // Wide enough for "Summarise automatically" and its "off" to sit
            // on one line without cutting the name short.
            .w(px(228.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(22.))
            .pt(px(ui::TOP_INSET))
            .pb(px(14.))
            .px(px(10.))
            .bg(t.rail)
            .border_r_1()
            .border_color(t.line)
            .child(
                div()
                    .px(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Scrollback"),
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
                            .children(self.machine_section(t, cx))
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
                    // A comma's ink sits at the left of its cell, so the
                    // shortcut is nudged right to end where the counts do.
                    .child(trailing(ui::shortcut(","), t).mr(px(-2.))),
            )
            .child(
                nav_row(t, false)
                    .id("auto-summarise")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_auto_summarize(cx)))
                    .child(div().flex_1().child("Summarise automatically"))
                    .child(trailing(if self.config.auto_summarize { "on" } else { "off" }, t)),
            )
            .child(
                div()
                    .px(px(10.))
                    .font_family(mono())
                    .text_size(px(text::CAPTION))
                    .line_height(px(17.))
                    .text_color(t.ink3)
                    .child(self.status.clone())
                    .children(self.notice.clone())
                    .children(self.summary_status()),
            )
    }

    /// A sidebar section's heading, which folds the section away.
    fn section(&self, title: &'static str, t: &Theme, cx: &mut Context<Self>) -> Div {
        let folded = self.folded.contains(title);
        let head = div()
            .id(title)
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(10.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.folded.remove(title) {
                    this.folded.insert(title);
                }
                cx.notify();
            }))
            .child(label(title, t))
            .child(
                svg()
                    .path(if folded { "chevron-right.svg" } else { "chevron-down.svg" })
                    .size(px(12.))
                    .mb(px(6.))
                    .text_color(t.ink3),
            );
        div().flex().flex_col().gap(px(2.)).child(head)
    }

    /// The machines sessions come from, when there is more than one.
    fn machine_section(&self, t: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        let others: Vec<&sync::Machine> = self
            .machines
            .iter()
            .filter(|m| !self.config.hidden_machines.contains(&m.id))
            .collect();
        if others.is_empty() {
            return None;
        }
        let mut section = self.section("Machine", t, cx);
        if self.folded.contains("Machine") {
            return Some(section);
        }
        let mut rows: Vec<(String, String, Option<&sync::Machine>)> =
            vec![(String::new(), this_machine_label().to_string(), None)];
        rows.extend(others.into_iter().map(|m| (m.id.clone(), m.name.clone(), Some(m))));
        for (i, (id, name, machine)) in rows.into_iter().enumerate() {
            let count = self.sessions.iter().filter(|s| machine_id(s) == id).count();
            let active = self.machine.as_deref() == Some(id.as_str());
            let away = machine.is_some_and(|m| m.away());
            let dot = div().size(px(16.)).flex_shrink_0().flex().items_center().justify_center().child(
                div()
                    .size(px(8.))
                    .rounded(px(4.))
                    .bg(if away { t.caution } else { t.ok }),
            );
            let mut row = nav_row(t, active)
                .id(("machine", i))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let same = this.machine.as_deref() == Some(id.as_str());
                    this.machine = (!same).then(|| id.clone());
                    this.recompute();
                    this.pump_digest(cx);
                    cx.notify();
                }))
                .child(dot)
                .child(div().flex_1().min_w(px(0.)).one_line().child(name))
                .child(count_badge(count, t));
            if let Some(machine) = machine {
                row = row.tooltip(ui::tip(machine_state(machine)));
            }
            section = section.child(row);
        }
        Some(section)
    }

    /// A machine's name, from its id; empty is this one.
    fn machine_name(&self, id: &str) -> String {
        if id.is_empty() {
            return this_machine_label().to_string();
        }
        self.machines
            .iter()
            .find(|m| m.id == id)
            .map_or_else(|| "another machine".to_string(), |m| m.name.clone())
    }

    /// The other machine a session is from, as last read from the folder.
    pub(crate) fn machine_of(&self, session: &Session) -> Option<&sync::Machine> {
        let id = &session.machine.as_ref()?.id;
        self.machines.iter().find(|m| &m.id == id)
    }

    /// A line about summary work in progress, when there is something to say.
    fn summary_status(&self) -> Option<String> {
        if let Some(until) = self.paused_until.filter(|until| Instant::now() < *until) {
            // Say when it resumes by itself and how to try sooner.
            let resumes = Local::now() + chrono::Duration::from_std(until - Instant::now()).unwrap_or_default();
            return Some(format!(
                "summaries paused until {}: {} reported a usage limit. {} tries again now.",
                resumes.format("%H:%M"),
                self.config.summary_provider.name(),
                ui::shortcut("R")
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
        self.machine = None;
        self.recompute();
        self.pump_digest(cx);
        cx.notify();
    }

    /// The filters in force, as words: "Codex, runyard".
    fn filter_words(&self) -> Option<String> {
        let machine = self.machine.as_deref().map(|id| self.machine_name(id));
        let parts: Vec<&str> = machine
            .as_deref()
            .into_iter()
            .chain(self.origin.map(|o| o.name()))
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
        // In a narrow window the hints give way to what is being typed.
        let hint = if searching && self.narrow {
            format!("{} sessions · {} messages", self.hits.len(), self.message_hits.len())
        } else if self.narrow {
            String::new()
        } else if searching {
            format!(
                "{} sessions · {} messages · {:.1?}   {} expand",
                self.hits.len(),
                self.message_hits.len(),
                self.search_time,
                ui::shifted("space")
            )
        } else {
            format!(
                "↑↓ move   → read   ↵ copy resume   {} summarise   {} activity",
                ui::shortcut("S"),
                ui::shortcut("2")
            )
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
                                .font_family(mono())
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
        // Another machine's sessions are summarised there.
        if self.sessions[index].machine.is_some() {
            return;
        }
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
                // A usage limit affects every request, so stop asking for a
                // while instead of failing each session in turn.
                if scrollback_core::provider::is_usage_limit(&message) {
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
            if let Some(machine) = &session.machine {
                origin = origin.child(div().flex_shrink_0().text_color(t.ink2).child(machine.name.clone()));
            }
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
                    .font_family(mono())
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
        // One line about it: the summary's, when there is one. Without one the
        // title is already the first prompt, so nothing more is said.
        let blurb = session
            .summary
            .as_ref()
            .map(|summary| summary.summary.clone())
            .unwrap_or_default();
        let selected = self.selected == Some(hit);
        let fill = t.fill;
        div()
            .id(("session", ix))
            .flex()
            .gap(px(20.))
            .px(px(20.))
            .py(px(12.))
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
                    .font_family(mono())
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
                    .gap(px(4.))
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
                        el.child(div().text_color(t.ink2).line_clamp(1).text_ellipsis().child(blurb))
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

        let mut card = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(facts_after(machine_chip(session, self.machine_of(session), t), session, t, true))
                    .children(
                        session
                            .summary
                            .as_ref()
                            .map(|summary| outcome_mark(summary, false, t)),
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

        // A glance: what the session was, how it ended, and anything left to
        // pick up. The rest of the summary is in the transcript view.
        match &session.summary {
            Some(summary) => {
                card = card.child(
                    div()
                        .text_size(px(text::LEAD))
                        .line_height(px(23.))
                        .text_color(t.ink2)
                        .child(summary.summary.clone()),
                );
                if !summary.open_ends.is_empty() {
                    card = card.child(open_ends(&summary.open_ends, 2, t));
                }
            }
            None => {
                card = card.child(
                    div()
                        .text_size(px(text::LEAD))
                        .line_height(px(23.))
                        .text_color(t.ink2)
                        .line_clamp(3)
                        .text_ellipsis()
                        .child(if session.first_prompt.is_empty() {
                            "No typed prompt was recorded for this session.".to_string()
                        } else {
                            session.first_prompt.clone()
                        }),
                );
            }
        }

        let remote = session.machine.is_some();
        let on_tailnet = session.machine.as_ref().is_some_and(|m| m.address.is_some());
        let away = self.machine_of(session).is_some_and(|m| m.away());
        // A machine on Tailscale is asked for the transcript, so it has to
        // be answering.
        let has_transcript = !remote || (sync::has_events(session) && !(on_tailnet && away));
        if let Some(machine) = self.machine_of(session).filter(|m| m.away()) {
            let mut words = if on_tailnet {
                format!(
                    "{} is not answering on Tailscale; it was last reached {}. Its summary is here, \
                     and the transcript comes across once it is back.",
                    machine.name,
                    ago(machine.written_ms)
                )
            } else {
                format!(
                    "{} is asleep; it last synced {}. Its summary is here.",
                    machine.name,
                    ago(machine.written_ms)
                )
            };
            if !has_transcript && !on_tailnet {
                words.push_str(" The transcript comes across when Include full messages is on.");
            }
            card = card.child(caution_notice(words, t));
        }

        let summary_action = if busy {
            "Writing summary…"
        } else if session.summary.is_none() {
            "Write summary"
        } else {
            "Update summary"
        };
        // The summary button is offered only when there is something to do:
        // none written yet, or the session has moved on. Rewriting a current
        // one is in the Session menu.
        // Another machine's sessions are summarised there.
        let offer_summary = !remote && (busy || session.summary.is_none() || session.summary_stale);
        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .pt(px(6.))
            .child(
                div()
                    .id("copy-resume")
                    .reachable(t)
                    .h(px(32.))
                    .px(px(13.))
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
                    .tooltip(ui::tip(resume_command(session)))
                    .child(if copied {
                        "Copied"
                    } else {
                        match session.harness {
                            // Cursor has no way to reopen one conversation.
                            Harness::Cursor | Harness::CopilotChat => {
                                "Copy open command"
                            }
                            _ => "Copy resume command",
                        }
                    }),
            )
            .child(
                div()
                    .id("read-transcript")
                    .reachable(t)
                    .h(px(32.))
                    .px(px(13.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(text::SMALL))
                    .font_weight(FontWeight::MEDIUM)
                    .when(!has_transcript, |el| {
                        el.text_color(t.ink3).opacity(0.6).tooltip(ui::tip(
                            "Its messages are not shared. Turn on Include full messages on that machine.",
                        ))
                    })
                    .when(has_transcript, |el| {
                        el.cursor_pointer()
                            .hover(|style| style.bg(t.fill))
                            .active(|style| style.opacity(0.75))
                            .on_click(cx.listener(|this, _, _, cx| this.open_transcript(cx)))
                    })
                    .child("Read transcript"),
            )
            .when(offer_summary, |el| el.child(
                div()
                    .id("write-summary")
                    .reachable(t)
                    .h(px(32.))
                    .px(px(13.))
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
            ));
        if let Some(covers) = covers(session).filter(|_| !busy) {
            actions = actions.child(
                div()
                    .text_size(px(text::CAPTION))
                    .text_color(t.ink3)
                    .child(covers),
            );
        }

        card = card.child(actions);
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
                .py(px(20.))
                .rounded(px(14.))
                .bg(t.raise)
                .shadow(t.card_shadow.clone())
                .child(
                    div()
                        .w(px(44.))
                        .flex_shrink_0()
                        .pt(px(1.))
                        .font_family(mono())
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
        self.narrow = window.viewport_size().width < px(1040.);
        ui::settle(
            &[&self.sidebar_scroll, &self.activity_scroll, &self.first_run_scroll],
            &self.measured,
            window,
            cx,
        );
        if self.first_run {
            // The menus act on sessions, and there are none to act on yet.
            let bar = chrome::bar("Scrollback", false, None, window, cx, &t);
            let root = Self::commands(div(), cx)
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::on_key))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, _| ui::pointer_used(&this.focus, window)),
                )
                .size_full()
                .flex()
                .flex_col()
                .bg(t.bg)
                .text_color(t.ink)
                .font_family(UI_FONT)
                .text_size(px(text::BODY))
                .line_height(px(20.))
                .children(bar)
                .child(div().flex_1().min_h(px(0.)).child(self.first_run_view(&t, cx)));
            return chrome::frame(root, window, &t);
        }
        let feed: AnyElement = if self.rows.is_empty() {
            let filtered = self.filter_words();
            let message = if self.sessions.is_empty() {
                "No sessions found on this computer yet.".to_string()
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

        let bar = chrome::bar("Scrollback", true, self.menu_open, window, cx, &t);
        let body = div()
            .flex_1()
            .min_h(px(0.))
            .flex()
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
            );
        let root = Self::commands(div(), cx)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| ui::pointer_used(&this.focus, window)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .line_height(px(20.))
            .children(bar)
            .child(body);
        chrome::frame(root, window, &t)
    }
}

impl chrome::Menus for Home {
    fn open_menu(&mut self) -> &mut Option<usize> {
        &mut self.menu_open
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

pub(super) enum Marker {
    None,
    Number,
    Dash,
}

/// One labelled column of the summary: a heading and its lines.
pub(super) fn section(title: &'static str, lines: &[String], marker: Marker, t: &Theme) -> Div {
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

/// What is left to pick up, at most `shown` of them, one line each.
fn open_ends(lines: &[String], shown: usize, t: &Theme) -> Div {
    let mut list = div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .text_size(px(text::SMALL))
        .text_color(t.ink2);
    for line in lines.iter().take(shown) {
        list = list.child(
            div()
                .flex()
                .gap(px(8.))
                .child(div().flex_shrink_0().text_color(t.ink3).child("–"))
                .child(div().flex_1().min_w(px(0.)).line_clamp(1).text_ellipsis().child(line.clone())),
        );
    }
    if lines.len() > shown {
        list = list.child(
            div()
                .pl(px(16.))
                .text_color(t.ink3)
                .child(format!("and {} more in the transcript view", lines.len() - shown)),
        );
    }
    list
}

/// The session's numbers: turns, tool calls, tokens, cost and model.
pub(super) fn stats(session: &Session, t: &Theme) -> Div {
    let mut stats = div()
        .flex()
        .flex_wrap()
        .gap_x(px(18.))
        .gap_y(px(4.))
        .font_family(mono())
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
    stats
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
    trailing(count.to_string(), t)
}

/// What sits at the right end of a sidebar row: a count, a shortcut, or a
/// setting's state. Every row ends its text at the same edge, so these
/// read as one column.
fn trailing(text: impl Into<SharedString>, t: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .flex()
        .justify_end()
        .font_family(mono())
        .text_size(px(text::CAPTION))
        .text_color(t.ink3)
        .child(text.into())
}

pub(crate) fn logo(origin: Origin, t: &Theme, size: f32) -> AnyElement {
    let (path, color): (&'static str, Hsla) = match origin {
        Origin::ClaudeCode => ("claude-code.svg", claude_orange()),
        Origin::ClaudeDesktop => ("claude.svg", claude_orange()),
        Origin::Codex => ("codex.svg", t.ink),
        Origin::CodexDesktop => ("openai.svg", t.ink),
        Origin::OpenCode => ("opencode.svg", t.ink),
        Origin::Pi => ("pi.svg", t.ink),
        Origin::Cursor => ("cursor.svg", t.ink),
        Origin::T3Code => ("t3.svg", t.ink),
        // Sources without a drawn mark get their initial in a small tile.
        other => return initial(other.name(), t, size).into_any_element(),
    };
    svg()
        .path(path)
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .into_any_element()
}

/// A source's initial in a rounded tile, the size of a logo.
fn initial(name: &str, t: &Theme, size: f32) -> Div {
    let letter: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size * 0.28))
        .bg(t.ink)
        .text_color(t.bg)
        .font_family(UI_FONT)
        .font_weight(FontWeight::BOLD)
        .text_size(px(size * 0.72))
        .line_height(px(size))
        .child(letter)
}

/// The line of small facts under a session: harness, project, branch, time.
/// Another machine's session starts with that machine's name.
pub(crate) fn facts(session: &Session, t: &Theme, with_time: bool) -> Div {
    let lead = session.machine.as_ref().map(|machine| {
        div()
            .flex_shrink_0()
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.ink2)
            .child(machine.name.clone())
            .into_any_element()
    });
    facts_after(lead, session, t, with_time)
}

/// The facts line, after something that leads it.
fn facts_after(lead: Option<AnyElement>, session: &Session, t: &Theme, with_time: bool) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .font_family(mono())
        .text_size(px(text::CAPTION))
        .text_color(t.ink3)
        .children(lead)
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

/// The tinted chip naming another machine on a session's card: amber, with
/// a dot, when that machine is away.
fn machine_chip(session: &Session, machine: Option<&sync::Machine>, t: &Theme) -> Option<AnyElement> {
    let name = session.machine.as_ref()?.name.clone();
    let away = machine.is_some_and(|m| m.away());
    let chip = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(5.))
        .px(px(7.))
        .py(px(1.))
        .rounded(px(5.))
        .font_weight(FontWeight::SEMIBOLD);
    Some(if away {
        chip.bg(t.caution_fill)
            .text_color(t.caution_ink)
            .child(div().size(px(6.)).rounded(px(3.)).bg(t.caution))
            .child(if machine.is_some_and(|m| m.address.is_some()) {
                format!("{name} · away")
            } else {
                format!("{name} · asleep")
            })
            .into_any_element()
    } else {
        chip.bg(t.chip_fill).text_color(t.chip_ink).child(name).into_any_element()
    })
}

/// A notice in the caution colour, with its sign.
pub(crate) fn caution_notice(words: impl Into<SharedString>, t: &Theme) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(9.))
        .bg(t.caution_fill)
        .border_1()
        .border_color(t.caution_line)
        .child(
            svg()
                .path("caution.svg")
                .size(px(16.))
                .mt(px(2.))
                .flex_shrink_0()
                .text_color(t.caution),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(text::SMALL))
                .line_height(px(19.))
                .text_color(t.caution_ink)
                .child(words.into()),
        )
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

/// The id of the machine a session is from; empty for this one.
pub(crate) fn machine_id(session: &Session) -> &str {
    session.machine.as_ref().map_or("", |m| m.id.as_str())
}

/// What this machine is called in lists of machines.
pub(crate) fn this_machine_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "This Mac"
    } else {
        "This computer"
    }
}

/// How long ago, roughly: "just now", "4 min ago", "2 h ago", "3 days ago".
pub(crate) fn ago(ms: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let minutes = (now - ms).max(0) / 60_000;
    match minutes {
        0..=1 => "just now".to_string(),
        2..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        1440..=2879 => "yesterday".to_string(),
        _ => format!("{} days ago", minutes / 1440),
    }
}

/// Whether another machine is in touch, and when it last wrote.
pub(crate) fn machine_state(machine: &sync::Machine) -> String {
    if machine.written_ms == 0 {
        return "not reached yet".to_string();
    }
    if machine.address.is_some() && machine.away() {
        return format!("not answering, last reached {}", ago(machine.written_ms));
    }
    if machine.away() {
        format!("asleep, synced {}", ago(machine.written_ms))
    } else {
        format!("updated {}", ago(machine.written_ms))
    }
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

pub(crate) fn duration_label(ms: i64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "under a minute".to_string(),
        1..=59 => format!("{minutes} min"),
        _ => format!("{} h {} min", minutes / 60, minutes % 60),
    }
}

pub(crate) fn compact_number(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.0}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// A shell command that reopens the session in its own harness, written
/// for the shell most people have: PowerShell on Windows, a Unix shell
/// elsewhere.
pub(crate) fn resume_command(session: &Session) -> String {
    let Some(machine) = &session.machine else {
        return resume_for(session, cfg!(windows));
    };
    // Another machine's session is resumed there, over SSH.
    let host = Config::load()
        .ssh_hosts
        .get(&machine.id)
        .filter(|host| !host.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| machine.name.clone());
    over_ssh(&host, &resume_for(session, machine.os == "windows"), cfg!(windows))
}

/// A command run on `host` over SSH, quoted for the shell here.
fn over_ssh(host: &str, command: &str, powershell: bool) -> String {
    let quoted = match powershell {
        true => format!("'{}'", command.replace('\'', "''")),
        false => format!("'{}'", command.replace('\'', "'\\''")),
    };
    format!("ssh {host} -t {quoted}")
}

fn resume_for(session: &Session, powershell: bool) -> String {
    // Both shells take a single-quoted string literally; they differ in how
    // a quote inside it is written, and in how two commands are joined.
    let quoted = |text: &str| match powershell {
        true => format!("'{}'", text.replace('\'', "''")),
        false => format!("'{}'", text.replace('\'', "'\\''")),
    };
    let dir = quoted(&session.cwd);
    let resume = match session.harness {
        Harness::ClaudeCode => format!("claude --resume {}", session.id),
        Harness::Codex => format!("codex resume {}", session.id),
        Harness::OpenCode => format!("opencode --session {}", session.id),
        // pi takes the session file itself, which every version accepts.
        Harness::Pi => format!("pi --session {}", quoted(&session.source)),
        // Cursor conversations cannot be reopened by id; open the project.
        Harness::Cursor => return format!("cursor {dir}"),
        // fx resumes from a picker of this folder's sessions.
        Harness::Fx => "fx -r".to_string(),
        Harness::Grok => format!("grok --resume {}", session.id),
        Harness::Hermes => format!("hermes --resume {}", session.id),
        // The Copilot CLI resumes from a picker.
        Harness::Copilot => "copilot --resume".to_string(),
        // A Copilot chat reopens from VS Code's chat history.
        Harness::CopilotChat => return format!("code {dir}"),
    };
    // Windows PowerShell, the one that ships with Windows, has no `&&`.
    let then = if powershell { ";" } else { " &&" };
    format!("cd {dir}{then} {resume}")
}

// `App` is only named in signatures GPUI infers; keep the import honest.
#[allow(dead_code)]
fn _uses(_: &App) {}

/// Where exports are offered to go: Downloads, if there is one.
fn dirs_downloads() -> std::path::PathBuf {
    let home = scrollback_core::home();
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        home
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resume_command_suits_the_shell() {
        let mut session = Session::new(Harness::ClaudeCode, "abc", "a.jsonl");
        session.cwd = "/Users/me/it's here".into();
        assert_eq!(resume_for(&session, false), r"cd '/Users/me/it'\''s here' && claude --resume abc");
        session.cwd = r"C:\Users\me\it's here".into();
        assert_eq!(resume_for(&session, true), r"cd 'C:\Users\me\it''s here'; claude --resume abc");
    }

    #[test]
    fn another_machines_session_resumes_over_ssh() {
        let mut session = Session::new(Harness::ClaudeCode, "abc", "a.jsonl");
        session.cwd = "/home/me/atlas".into();
        let inner = resume_for(&session, false);
        assert_eq!(
            over_ssh("devbox", &inner, false),
            r"ssh devbox -t 'cd '\''/home/me/atlas'\'' && claude --resume abc'"
        );
    }
}
