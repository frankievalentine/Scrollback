//! The popover under the menu bar icon: quick access to the most recent
//! sessions that already have a summary. It opens at once from the saved
//! index and never waits on a summary being written; a session shows up here
//! once it has one. Type to search, arrow through the list, copy a resume
//! command, or hand a session to the window.

use crate::home::{self, clock, day_label, facts, label, outcome_mark, resume_command};
use crate::open_main;
use crate::theme::{text, Theme, MONO_FONT, UI_FONT};
use crate::ui::{self, OneLine, Reachable};
use gpui::{
    div, point, prelude::*, px, size, svg, App, Bounds, ClipboardItem, Context, Div, FocusHandle,
    FontWeight, Global, KeyDownEvent, Pixels, ScrollHandle, SharedString, Window, WindowBounds,
    WindowHandle, WindowKind, WindowOptions,
};
use scrollback_core::search::Searcher;
use scrollback_core::Session;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WIDTH: f32 = 800.;
const HEIGHT: f32 = 500.;
/// How many sessions the list holds: the most recent with a summary.
const SHOWN: usize = 15;
/// A session written to this recently counts as in use.
const ACTIVE_WITHIN_MS: i64 = 5 * 60 * 1000;

/// The popover, while it is open, and when it last closed itself.
struct Open {
    window: Option<WindowHandle<Popover>>,
    closed_at: Option<Instant>,
}

impl Global for Open {}

pub fn install(cx: &mut App) {
    cx.set_global(Open { window: None, closed_at: None });
}

/// Open the popover under the icon, or close it if it is open. `icon` is
/// where the menu bar icon is, in physical pixels, when that is known.
pub fn toggle(icon: Option<tray_icon::Rect>, cx: &mut App) {
    let (window, closed_at) = {
        let open = cx.global::<Open>();
        (open.window, open.closed_at)
    };
    if let Some(window) = window {
        if window.update(cx, |_, window, _| window.remove_window()).is_ok() {
            cx.global_mut::<Open>().window = None;
            return;
        }
    }
    // Clicking the icon while the popover is open first takes the keyboard
    // away from it, which closes it; without this the same click would then
    // open it again.
    if closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(350)) {
        return;
    }

    // What the index already holds, so the popover is there at once. It
    // brings itself up to date a moment later.
    let sessions = home::load_saved();
    let quiet = std::env::var_os("SCROLLBACK_BACKGROUND").is_some();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(place(icon, cx))),
            titlebar: None,
            kind: WindowKind::PopUp,
            focus: !quiet,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| Popover::new(sessions, window, cx));
            window.focus(&view.read(cx).focus);
            view
        },
    );
    if let Ok(window) = opened {
        cx.global_mut::<Open>().window = Some(window);
    }
}

/// The open popover, for development checks.
pub fn window(cx: &App) -> Option<WindowHandle<Popover>> {
    cx.global::<Open>().window
}

/// Where the popover goes: centred under the icon, kept on its screen.
fn place(icon: Option<tray_icon::Rect>, cx: &App) -> Bounds<Pixels> {
    let fallback = cx.primary_display().map(|display| display.bounds());
    let panel = size(px(WIDTH), px(HEIGHT));
    let Some(icon) = icon else {
        // No icon to hang from: the top right corner of the main screen.
        let screen = fallback.unwrap_or(Bounds { origin: point(px(0.), px(0.)), size: size(px(1440.), px(900.)) });
        let x = screen.origin.x + screen.size.width - panel.width - px(12.);
        return Bounds { origin: point(x, screen.origin.y + px(36.)), size: panel };
    };
    // The icon's place comes in physical pixels. A menu bar is about 24 to
    // 37 points tall, so anything much taller is a 2x screen.
    let scale = if icon.size.height >= 44 { 2. } else { 1. };
    let left = icon.position.x as f32 / scale;
    let middle = left + icon.size.width as f32 / scale / 2.;
    let bottom = (icon.position.y as f32 + icon.size.height as f32) / scale;
    let screen = cx
        .displays()
        .into_iter()
        .map(|display| display.bounds())
        .find(|b| {
            let x = px(middle);
            x >= b.origin.x && x < b.origin.x + b.size.width
        })
        .or(fallback);
    let mut x = px(middle) - panel.width / 2.;
    if let Some(screen) = screen {
        let most = screen.origin.x + screen.size.width - panel.width - px(8.);
        x = x.min(most).max(screen.origin.x + px(8.));
    }
    Bounds { origin: point(x, px(bottom + 6.)), size: panel }
}

/// One line of the list.
#[derive(Clone)]
enum Entry {
    Heading(&'static str),
    /// An index into `Popover::sessions`.
    Session(usize),
}

pub struct Popover {
    sessions: Vec<Session>,
    entries: Vec<Entry>,
    /// The sessions listed, in order; the selection is an index into this.
    listed: Vec<usize>,
    selected: usize,
    query: String,
    searcher: Searcher,
    /// Which button last copied something, to say so on it.
    copied: Option<&'static str>,
    /// Sessions newer than the oldest one listed that have no summary yet,
    /// so are not listed.
    waiting: usize,
    /// Whether the search caret is in the lit half of its blink.
    caret_lit: bool,
    /// When a key was last pressed, so the caret stays lit while typing.
    typed_at: Instant,
    scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    measured: ui::Measured,
    pub focus: FocusHandle,
}

impl Popover {
    fn new(sessions: Vec<Session>, window: &mut Window, cx: &mut Context<Self>) -> Popover {
        // A popover goes away as soon as attention moves elsewhere.
        cx.observe_window_activation(window, |_, window, cx| {
            if !window.is_window_active() {
                let open = cx.global_mut::<Open>();
                open.window = None;
                open.closed_at = Some(Instant::now());
                window.remove_window();
            }
        })
        .detach();
        let mut popover = Popover {
            sessions,
            entries: Vec::new(),
            listed: Vec::new(),
            selected: 0,
            query: String::new(),
            searcher: Searcher::new(),
            copied: None,
            waiting: 0,
            caret_lit: true,
            typed_at: Instant::now(),
            scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            measured: Default::default(),
            focus: cx.focus_handle(),
        };
        popover.recompute();

        // Blink the search caret for as long as the popover is open.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(ui::CARET_BLINK).await;
            let open = this.update(cx, |this, cx| {
                this.caret_lit = !this.caret_lit;
                cx.notify();
            });
            if open.is_err() {
                break;
            }
        })
        .detach();

        // The saved index may be a little behind: read the harnesses again in
        // the background and take in whatever is new.
        cx.spawn(async move |this, cx| {
            let (sessions, _) = cx.background_executor().spawn(async { home::load() }).await;
            this.update(cx, |this, cx| this.refreshed(sessions, cx)).ok();
        })
        .detach();
        popover
    }

    /// Take in a fresh read of the sessions, keeping the selection.
    fn refreshed(&mut self, sessions: Vec<Session>, cx: &mut Context<Self>) {
        let unchanged = sessions.len() == self.sessions.len()
            && sessions.iter().zip(&self.sessions).all(|(new, old)| {
                new.key() == old.key()
                    && new.updated_ms == old.updated_ms
                    && new.summary.is_some() == old.summary.is_some()
                    && new.summary_stale == old.summary_stale
            });
        if unchanged {
            return;
        }
        let selected = self.current().map(Session::key);
        let copied = self.copied;
        self.sessions = sessions;
        self.recompute();
        let kept = selected.and_then(|key| {
            self.listed.iter().position(|&i| self.sessions[i].key() == key)
        });
        if let Some(selection) = kept {
            self.selected = selection;
            self.copied = copied;
        }
        cx.notify();
    }

    /// What the popover holds, for the development key-press log.
    pub fn dev_state(&self) -> String {
        format!(
            "popover query={:?} listed={} selected={} copied={:?} waiting={} summary={} title={:?}",
            self.query,
            self.listed.len(),
            self.selected,
            self.copied,
            self.waiting,
            self.current().is_some_and(|s| s.summary.is_some()),
            self.current().map(|s| s.display_title().chars().take(40).collect::<String>()),
        )
    }

    fn current(&self) -> Option<&Session> {
        self.listed.get(self.selected).map(|&i| &self.sessions[i])
    }

    /// Rebuild the list for the current search. Only sessions that already
    /// have a summary are listed: the most recent few, or the best matches.
    fn recompute(&mut self) {
        self.entries.clear();
        self.selected = 0;
        self.copied = None;
        self.waiting = 0;
        if !self.query.trim().is_empty() {
            self.listed = self
                .searcher
                .search(&self.query, &self.sessions)
                .into_iter()
                .map(|hit| hit.index)
                .filter(|&i| self.sessions[i].summary.is_some())
                .take(SHOWN)
                .collect();
            if !self.listed.is_empty() {
                self.entries.push(Entry::Heading("Results"));
            }
            self.entries.extend(self.listed.iter().map(|&i| Entry::Session(i)));
            return;
        }
        // Newest first, under a heading for how long ago.
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        self.listed = (0..self.sessions.len())
            .filter(|&i| self.sessions[i].summary.is_some())
            .take(SHOWN)
            .collect();
        // Sessions are newest first, so anything before the last one listed
        // that is not itself listed is a newer session awaiting its summary.
        self.waiting = match self.listed.last() {
            Some(&last) => last + 1 - self.listed.len(),
            None => self.sessions.len().min(SHOWN),
        };
        let mut last = "";
        for &i in &self.listed {
            let session = &self.sessions[i];
            let group = if now_ms - session.updated_ms < ACTIVE_WITHIN_MS {
                "Active now"
            } else {
                match day_label(session.updated_ms).as_str() {
                    "Today" => "Earlier today",
                    "Yesterday" => "Yesterday",
                    _ => "Earlier",
                }
            };
            if group != last {
                self.entries.push(Entry::Heading(group));
                last = group;
            }
            self.entries.push(Entry::Session(i));
        }
    }

    fn select(&mut self, selection: usize, cx: &mut Context<Self>) {
        if selection >= self.listed.len() {
            return;
        }
        self.selected = selection;
        self.copied = None;
        let session = self.listed[selection];
        let line = self
            .entries
            .iter()
            .position(|entry| matches!(entry, Entry::Session(i) if *i == session));
        if let Some(line) = line {
            self.scroll.scroll_to_item(line);
        }
        cx.notify();
    }

    fn copy_resume(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.current() else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(resume_command(session)));
        self.copied = Some("resume");
        cx.notify();
    }

    fn copy_summary(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.current() else { return };
        let mut text = session.display_title().to_string();
        if let Some(summary) = &session.summary {
            text.push_str(&format!("\n\n{}", summary.summary));
            if !summary.open_ends.is_empty() {
                text.push_str("\n\nOpen ends:");
                for end in &summary.open_ends {
                    text.push_str(&format!("\n- {end}"));
                }
            }
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some("summary");
        cx.notify();
    }

    /// Hand the selected session to the window and get out of the way.
    fn open_in_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.current().map(Session::key) else { return };
        cx.global_mut::<Open>().window = None;
        window.remove_window();
        // Opening the window is left until this popover has finished closing.
        cx.defer(move |cx| {
            open_main(cx, Some(key));
        });
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.global_mut::<Open>().window = None;
        window.remove_window();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ui::control_key(event, &self.focus, window, cx) {
            return;
        }
        self.typed_at = Instant::now();
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        match keystroke.key.as_str() {
            "down" => self.select(self.selected + 1, cx),
            "up" => self.select(self.selected.saturating_sub(1), cx),
            "enter" => self.copy_resume(cx),
            "o" if mods.platform => self.open_in_window(window, cx),
            "c" if mods.platform => self.copy_summary(cx),
            "escape" if self.query.is_empty() => self.close(window, cx),
            "escape" => {
                self.query.clear();
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
                // The rest of the command shortcuts belong to the menu bar.
                if mods.platform || mods.control || mods.function {
                    return;
                }
                let Some(typed) = keystroke.key_char.as_deref() else { return };
                if typed.is_empty() || typed.chars().any(char::is_control) {
                    return;
                }
                self.query.push_str(typed);
                self.recompute();
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    fn search_row(&self, t: &Theme) -> Div {
        let lit = self.caret_lit || self.typed_at.elapsed() < ui::CARET_BLINK;
        let text = ui::search_field(&self.query, "Search sessions", lit, t);
        div()
            .h(px(52.))
            .flex_shrink_0()
            .px(px(18.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(t.line)
            .text_size(px(text::LEAD))
            .child(svg().path("search.svg").size(px(16.)).flex_shrink_0().text_color(t.ink3))
            .child(div().flex_1().min_w(px(0.)).child(text))
    }

    fn list(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let chosen = self.listed.get(self.selected).copied();
        let mut list = div()
            .id("popover-list")
            .w(px(292.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(8.))
            .py(px(10.))
            .border_r_1()
            .border_color(t.line);
        if self.entries.is_empty() {
            return list.child(
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .text_size(px(text::SMALL))
                    .text_color(t.ink3)
                    .line_height(px(19.))
                    .child(if !self.query.trim().is_empty() {
                        "No summarised session matches."
                    } else if self.sessions.is_empty() {
                        "No sessions yet."
                    } else {
                        "No summaries yet. Sessions appear here once they have one."
                    }),
            );
        }
        for (line, entry) in self.entries.iter().enumerate() {
            match entry {
                Entry::Heading(name) => {
                    list = list.child(
                        div()
                            .px(px(10.))
                            .pt(px(if line == 0 { 2. } else { 12. }))
                            .child(label(*name, t)),
                    );
                }
                Entry::Session(i) => {
                    let i = *i;
                    let session = &self.sessions[i];
                    let selected = chosen == Some(i);
                    let selection = self.listed.iter().position(|&l| l == i).unwrap_or(0);
                    let active = now_ms - session.updated_ms < ACTIVE_WITHIN_MS;
                    let when = match day_label(session.updated_ms).as_str() {
                        "Today" => clock(session.updated_ms),
                        other => other.to_lowercase(),
                    };
                    let mark = match (&session.summary, active) {
                        // A filled square for a session in use right now.
                        (_, true) => div().size(px(8.)).flex_shrink_0().rounded(px(2.)).bg(t.accent).into_any_element(),
                        (Some(summary), false) => svg()
                            .path(match summary.outcome {
                                scrollback_core::Outcome::Done => "outcome-done.svg",
                                scrollback_core::Outcome::Partial => "outcome-partial.svg",
                                scrollback_core::Outcome::Stalled => "outcome-stalled.svg",
                            })
                            .size(px(9.))
                            .flex_shrink_0()
                            .text_color(t.ink)
                            .into_any_element(),
                        (None, false) => div().size(px(8.)).flex_shrink_0().into_any_element(),
                    };
                    let fill = t.fill;
                    list = list.child(
                        div()
                            .id(("popover-session", line))
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .border_l_2()
                            .border_color(if selected { t.accent } else { t.accent.opacity(0.) })
                            .when(selected, |el| el.bg(fill))
                            .hover(move |style| style.bg(fill))
                            .on_click(cx.listener(move |this, _, _, cx| this.select(selection, cx)))
                            .child(
                                div()
                                    .w_full()
                                    .one_line()
                                    .text_size(px(text::SMALL))
                                    .when(selected, |el| el.font_weight(FontWeight::MEDIUM))
                                    .child(session.display_title().to_string()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .font_family(MONO_FONT)
                                    .text_size(px(text::CAPTION))
                                    .text_color(t.ink3)
                                    .child(mark)
                                    .child(div().min_w(px(0.)).one_line().child(format!(
                                        "{} · {} · {}",
                                        session.origin.name(),
                                        session.project(),
                                        when
                                    ))),
                            ),
                    );
                }
            }
        }
        list
    }

    fn detail(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let pane = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(18.))
            .px(px(28.))
            .py(px(22.));
        let Some(session) = self.current() else {
            return pane.child(div().text_color(t.ink3).child("Nothing selected."));
        };
        let mut top = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(facts(session, t, true))
            .child(
                div()
                    .text_size(px(text::TITLE))
                    .line_height(px(25.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_clamp(3).text_ellipsis()
                    .child(session.display_title().to_string()),
            );
        let mut sections: Vec<Div> = Vec::new();
        if let Some(summary) = &session.summary {
            top = top
                .child(
                    div()
                        .text_size(px(text::LEAD))
                        .line_height(px(23.))
                        .text_color(t.ink2)
                        .child(summary.summary.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(outcome_mark(summary, true, t))
                        .children(session.summary_stale.then(|| {
                            div()
                                .text_size(px(text::CAPTION))
                                .text_color(t.ink3)
                                .child("Written before the session moved on.")
                        })),
                );
            if !summary.open_ends.is_empty() {
                sections.push(listing("Open ends", &summary.open_ends, false, t));
            }
            if !summary.happened.is_empty() {
                sections.push(listing("What happened", &summary.happened, true, t));
            }
        }

        let resume = match self.copied {
            Some("resume") => "Copied".to_string(),
            _ => "Copy resume".to_string(),
        };
        let summary = match self.copied {
            Some("summary") => "Copied",
            _ => "Copy summary",
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .child(
                action("popover-resume", resume, "↵", true, t)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_resume(cx))),
            )
            .child(
                action("popover-open", "Open in window", "⌘O", false, t)
                    .on_click(cx.listener(|this, _, window, cx| this.open_in_window(window, cx))),
            )
            .child(
                action("popover-summary", summary, "⌘C", false, t)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_summary(cx))),
            );

        // The summary scrolls if it is long; the buttons stay put below it.
        pane.child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.))
                .child(
                    div()
                        .id("popover-detail")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.detail_scroll)
                        .flex()
                        .flex_col()
                        .gap(px(18.))
                        .pr(px(10.))
                        .child(top)
                        .children(sections),
                )
                .children(ui::scrollbar(&self.detail_scroll, t)),
        )
        .child(actions)
    }
}

impl Render for Popover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        ui::settle(&[&self.scroll, &self.detail_scroll], &self.measured, window, cx);
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| ui::pointer_used(&this.focus, window)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(t.raise)
            .text_color(t.ink)
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .line_height(px(20.))
            .child(self.search_row(&t))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .h_full()
                            .child(self.list(&t, cx))
                            .children(ui::scrollbar(&self.scroll, &t)),
                    )
                    .child(self.detail(&t, cx)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .gap(px(20.))
                    .px(px(18.))
                    .py(px(9.))
                    .border_t_1()
                    .border_color(t.line)
                    .font_family(MONO_FONT)
                    .text_size(px(text::CAPTION))
                    .text_color(t.ink3)
                    .child("↑↓ move")
                    .child("type to search")
                    .child("esc close")
                    .child(div().flex_1())
                    .children((self.waiting > 0 && self.query.trim().is_empty()).then(|| {
                        match self.waiting {
                            1 => "1 newer session has no summary yet".to_string(),
                            n => format!("{n} newer sessions have no summary yet"),
                        }
                    })),
            )
    }
}

/// A titled list inside the summary: open ends as dashes, steps as numbers.
fn listing(title: &'static str, lines: &[String], numbered: bool, t: &Theme) -> Div {
    let mut list = div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .pt(px(16.))
        .border_t_1()
        .border_color(t.line)
        .child(label(title, t));
    for (i, line) in lines.iter().enumerate() {
        list = list.child(
            div()
                .flex()
                .gap(px(8.))
                .child(
                    div()
                        .w(px(16.))
                        .flex_shrink_0()
                        .text_color(t.ink3)
                        .child(if numbered { format!("{}.", i + 1) } else { "–".to_string() }),
                )
                .child(div().flex_1().min_w(px(0.)).child(line.clone())),
        );
    }
    list
}

/// One of the buttons under a session: a name and the key that does the same.
fn action(
    id: &'static str,
    name: impl Into<SharedString>,
    key: &'static str,
    primary: bool,
    t: &Theme,
) -> gpui::Stateful<Div> {
    let fill = t.fill;
    div()
        .id(id)
        .reachable(t)
        .h(px(36.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(8.))
        .text_size(px(text::SMALL))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .active(|style| style.opacity(0.75))
        .when(primary, |el| el.bg(t.accent).text_color(t.on_accent).hover(|style| style.opacity(0.9)))
        .when(!primary, |el| el.border_1().border_color(t.line).hover(move |style| style.bg(fill)))
        .child(name.into())
        .when(!key.is_empty(), |el| {
            el.child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(text::CAPTION))
                    .when(!primary, |el| el.text_color(t.ink3))
                    .child(key),
            )
        })
}
