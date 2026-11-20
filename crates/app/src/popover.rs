//! The popover under the menu bar icon: a quick look at recent sessions and
//! their summaries without opening the window. Type to search, arrow through
//! the list, copy a resume command, or hand a session to the window.

use crate::home::{self, clock, day_label, facts, label, outcome_mark, resume_command};
use crate::open_main;
use crate::theme::{Theme, MONO_FONT, UI_FONT};
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
/// How many sessions the list holds.
const SHOWN: usize = 40;
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

    let (sessions, _) = home::load();
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
    scroll: ScrollHandle,
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
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        };
        popover.recompute();
        popover
    }

    /// What the popover holds, for the development key-press log.
    pub fn dev_state(&self) -> String {
        format!(
            "popover query={:?} listed={} selected={} copied={:?} title={:?}",
            self.query,
            self.listed.len(),
            self.selected,
            self.copied,
            self.current().map(|s| s.display_title().chars().take(40).collect::<String>()),
        )
    }

    fn current(&self) -> Option<&Session> {
        self.listed.get(self.selected).map(|&i| &self.sessions[i])
    }

    /// Rebuild the list for the current search.
    fn recompute(&mut self) {
        self.entries.clear();
        self.selected = 0;
        self.copied = None;
        if !self.query.trim().is_empty() {
            self.listed = self
                .searcher
                .search(&self.query, &self.sessions)
                .into_iter()
                .take(SHOWN)
                .map(|hit| hit.index)
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
        self.listed = (0..self.sessions.len().min(SHOWN)).collect();
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
                text.push_str("\n\nLeft open:");
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
        let caret = div().w(px(2.)).h(px(18.)).flex_shrink_0().rounded(px(1.)).bg(t.accent);
        let text = if self.query.is_empty() {
            div().flex().items_center().gap(px(6.)).child(caret).child(
                div()
                    .text_color(t.ink3)
                    .child("What was I doing with…"),
            )
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(1.))
                .child(div().min_w(px(0.)).truncate().child(self.query.clone()))
                .child(caret)
        };
        div()
            .h(px(52.))
            .flex_shrink_0()
            .px(px(18.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(t.line)
            .text_size(px(16.))
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
                    .text_size(px(13.))
                    .text_color(t.ink3)
                    .child(if self.sessions.is_empty() { "No sessions yet." } else { "Nothing matches." }),
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
                            .when(selected, |el| el.bg(fill))
                            .hover(move |style| style.bg(fill))
                            .on_click(cx.listener(move |this, _, _, cx| this.select(selection, cx)))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .when(selected, |el| el.font_weight(FontWeight::MEDIUM))
                                    .child(session.display_title().to_string()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .font_family(MONO_FONT)
                                    .text_size(px(11.))
                                    .text_color(t.ink3)
                                    .child(mark)
                                    .child(div().min_w(px(0.)).truncate().child(format!(
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
                    .text_size(px(19.))
                    .line_height(px(25.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_clamp(3)
                    .child(session.display_title().to_string()),
            );
        let mut open_ends = None;
        match &session.summary {
            Some(summary) => {
                top = top
                    .child(
                        div()
                            .line_height(px(22.))
                            .text_color(t.ink2)
                            .line_clamp(6)
                            .child(summary.summary.clone()),
                    )
                    .child(div().flex().child(outcome_mark(summary, true, t)));
                if !summary.open_ends.is_empty() {
                    let mut ends = div()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .pt(px(16.))
                        .border_t_1()
                        .border_color(t.line)
                        .child(label("Left open", t));
                    for end in summary.open_ends.iter().take(3) {
                        ends = ends.child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(div().w(px(10.)).flex_shrink_0().text_color(t.ink3).child("–"))
                                .child(div().flex_1().min_w(px(0.)).line_clamp(2).child(end.clone())),
                        );
                    }
                    open_ends = Some(ends);
                }
            }
            None => {
                top = top.child(
                    div()
                        .line_height(px(22.))
                        .text_color(t.ink2)
                        .line_clamp(8)
                        .child(if session.first_prompt.is_empty() {
                            "No summary yet.".to_string()
                        } else {
                            session.first_prompt.clone()
                        }),
                );
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
                .id("popover-detail")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(18.))
                .child(top)
                .children(open_ends),
        )
        .child(actions)
    }
}

impl Render for Popover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.raise)
            .text_color(t.ink)
            .font_family(UI_FONT)
            .text_size(px(14.))
            .line_height(px(20.))
            .child(self.search_row(&t))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.list(&t, cx))
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
                    .text_size(px(11.))
                    .text_color(t.ink3)
                    .child("↑↓ move")
                    .child("type to search")
                    .child("esc close"),
            )
    }
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
        .h(px(36.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(8.))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .when(primary, |el| el.bg(t.accent).text_color(t.on_accent).hover(|style| style.opacity(0.9)))
        .when(!primary, |el| el.border_1().border_color(t.line).hover(move |style| style.bg(fill)))
        .child(name.into())
        .child(
            div()
                .font_family(MONO_FONT)
                .text_size(px(11.))
                .when(!primary, |el| el.text_color(t.ink3))
                .child(key),
        )
}
