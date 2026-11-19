//! The activity view: what the last week, month or quarter came to. A short
//! written digest and the open ends sit in a card at the top; totals, a
//! per-harness split, sessions over time and recent edits follow.

use super::reader::centered;
use super::{clock, compact_number, day_label, label, logo, Home, View, LIMIT_PAUSE};
use crate::theme::{Theme, MONO_FONT};
use chrono::Local;
use gpui::{
    div, point, prelude::*, px, relative, rgba, BoxShadow, Context, Div, FontWeight, ListOffset,
    SharedString,
};
use scrollback_core::activity::{self, Activity};
use scrollback_core::{summarize, Digest, Session, Store};
use std::time::Instant;

/// The periods the view offers, in days.
pub(super) const RANGES: [u32; 3] = [7, 30, 90];

const OPEN_ENDS_SHOWN: usize = 6;
/// So one session with a long list does not crowd out the rest.
const OPEN_ENDS_PER_SESSION: usize = 2;

impl Home {
    /// The sessions the sidebar filters let through.
    fn in_scope(&self) -> impl Iterator<Item = &Session> {
        self.sessions.iter().filter(|session| {
            self.harness.is_none_or(|h| session.harness == h)
                && self
                    .project
                    .as_deref()
                    .is_none_or(|p| session.project() == p)
        })
    }

    /// Names the period and filters a digest covers.
    fn digest_scope(&self) -> String {
        format!(
            "{}|{}|{}",
            self.range_days,
            self.harness.map_or("", |h| h.slug()),
            self.project.as_deref().unwrap_or("")
        )
    }

    /// Work the totals out again and look up the digest saved for them.
    pub(super) fn refresh_activity(&mut self) {
        let activity = Activity::over(self.in_scope(), self.range_days);
        self.activity = activity;
        let scope = self.digest_scope();
        self.digest = self
            .store
            .as_ref()
            .and_then(|store| store.digest(&scope).ok().flatten());
    }

    pub(super) fn show_activity(&mut self, cx: &mut Context<Self>) {
        self.reading = None;
        self.view = View::Activity;
        self.refresh_activity();
        self.pump_digest(cx);
        cx.notify();
    }

    pub(super) fn show_sessions(&mut self, cx: &mut Context<Self>) {
        self.reading = None;
        self.view = View::Sessions;
        cx.notify();
    }

    pub(super) fn set_range(&mut self, days: u32, cx: &mut Context<Self>) {
        self.range_days = days;
        self.digest_error = None;
        self.refresh_activity();
        self.pump_digest(cx);
        cx.notify();
    }

    /// Move to the next shorter or longer period.
    pub(super) fn step_range(&mut self, delta: isize, cx: &mut Context<Self>) {
        let at = RANGES.iter().position(|d| *d == self.range_days).unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, RANGES.len() as isize - 1) as usize;
        if RANGES[next] != self.range_days {
            self.set_range(RANGES[next], cx);
        }
    }

    /// Leave the activity view for the feed, with one session selected.
    fn show_session(&mut self, key: &str, cx: &mut Context<Self>) {
        self.view = View::Sessions;
        if !self.query.is_empty() {
            self.query.clear();
            self.recompute();
        }
        let found = self
            .hits
            .iter()
            .position(|hit| self.sessions[hit.index].key() == key);
        if let Some(selection) = found {
            self.select(selection, cx);
            // The feed has not been laid out while this view was showing, so
            // put the session at the top rather than asking for it to be revealed.
            if let Some(row) = self.row_of(selection) {
                self.list.scroll_to(ListOffset {
                    item_ix: row,
                    offset_in_item: px(0.),
                });
            }
        }
        cx.notify();
    }

    fn digest_is_current(&self) -> bool {
        self.digest
            .as_ref()
            .is_some_and(|digest| digest.fingerprint == self.activity.fingerprint)
    }

    /// Write the digest when the view is open and the one on hand is missing
    /// or out of date. Follows the auto-summarise setting, and waits for
    /// session summaries still being written so the digest is written once.
    pub(super) fn pump_digest(&mut self, cx: &mut Context<Self>) {
        if self.first_run || !self.claude_found {
            return;
        }
        if self.view != View::Activity || !self.config.auto_summarize {
            return;
        }
        if self.paused_until.is_some_and(|until| Instant::now() < until) {
            return;
        }
        if !self.busy.is_empty() || self.activity.summarized == 0 || self.digest_is_current() {
            return;
        }
        if self.digest_failed == Some(self.activity.fingerprint) {
            return;
        }
        self.start_digest(cx);
    }

    /// The button: write the digest now, whatever happened before.
    fn write_digest(&mut self, cx: &mut Context<Self>) {
        self.digest_failed = None;
        self.digest_error = None;
        self.paused_until = None;
        self.start_digest(cx);
    }

    fn start_digest(&mut self, cx: &mut Context<Self>) {
        if self.digest_busy {
            return;
        }
        self.digest_busy = true;
        let scope = self.digest_scope();
        let fingerprint = self.activity.fingerprint;
        let days = self.range_days;
        let sessions: Vec<Session> = activity::recent(self.in_scope(), days, Local::now())
            .into_iter()
            .filter(|session| session.summary.is_some())
            .cloned()
            .collect();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let saved_as = scope.clone();
            let work = cx.background_executor().spawn(async move {
                let entries: Vec<&Session> = sessions.iter().collect();
                let written = summarize::digest(&entries, days)?;
                let digest = Digest {
                    text: written.text,
                    fingerprint,
                };
                Store::open()?.set_digest(&saved_as, &digest)?;
                anyhow::Ok(digest)
            });
            let result = work.await;
            this.update(cx, |this, cx| this.digest_arrived(scope, fingerprint, result, cx))
                .ok();
        })
        .detach();
    }

    fn digest_arrived(
        &mut self,
        scope: String,
        fingerprint: u64,
        result: anyhow::Result<Digest>,
        cx: &mut Context<Self>,
    ) {
        self.digest_busy = false;
        match result {
            Ok(digest) => {
                eprintln!("digest written for {scope}: {}", digest.text);
                // The period or filters may have changed while it was written.
                if scope == self.digest_scope() {
                    self.digest = Some(digest);
                    self.digest_error = None;
                }
            }
            Err(err) => {
                let message = format!("{err:#}");
                eprintln!("digest failed for {scope}: {message}");
                if message.to_lowercase().contains("limit") {
                    self.paused_until = Some(Instant::now() + LIMIT_PAUSE);
                }
                self.digest_failed = Some(fingerprint);
                self.digest_error = Some(message);
            }
        }
        self.pump_digest(cx);
        cx.notify();
    }

    /// The whole view: a header above one scrolling column.
    pub(super) fn activity_view(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let a = &self.activity;
        let column = div()
            .flex()
            .flex_col()
            .gap(px(28.))
            .pt(px(14.))
            .pb(px(48.))
            .child(self.activity_header(t, cx))
            .child(self.digest_card(t, cx))
            .child(totals(a, t))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(48.))
                    .gap_y(px(32.))
                    .px(px(4.))
                    .child(by_harness(a, t))
                    .child(over_time(a, t)),
            )
            .child(self.recent_edits(t, cx));
        div()
            .id("activity")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(centered(column))
    }

    fn activity_header(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let mut ranges = div()
            .flex()
            .gap(px(2.))
            .p(px(3.))
            .rounded(px(9.))
            .bg(t.fill);
        for days in RANGES {
            let active = self.range_days == days;
            let segment = t.segment;
            ranges = ranges.child(
                div()
                    .id(("range", days as usize))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(7.))
                    .text_size(px(13.))
                    .text_color(if active { t.ink } else { t.ink2 })
                    .cursor_pointer()
                    .when(active, |el| {
                        el.bg(segment).font_weight(FontWeight::MEDIUM).shadow(vec![BoxShadow {
                            color: rgba(0x00000024).into(),
                            offset: point(px(0.), px(1.)),
                            blur_radius: px(2.),
                            spread_radius: px(0.),
                        }])
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_range(days, cx)))
                    .child(format!("{days} days")),
            );
        }
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(10.))
            .child(
                div()
                    .pl(px(4.))
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Activity"),
            )
            .child(ranges)
    }

    /// The card: the written digest, then the open ends worth picking up.
    fn digest_card(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let a = &self.activity;
        let current = self.digest_is_current();
        let quiet = |text: &str| {
            div()
                .text_size(px(15.))
                .line_height(px(24.))
                .text_color(t.ink3)
                .child(text.to_string())
        };

        let mut written = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(label(format!("The last {} days, in short", a.days), t));
        written = match &self.digest {
            Some(digest) => written.child(
                div()
                    .text_size(px(17.))
                    .line_height(px(27.))
                    .child(digest.text.clone()),
            ),
            None if a.sessions == 0 => written.child(quiet("No sessions in this period.")),
            None if a.summarized == 0 => {
                written.child(quiet("None of these sessions has a summary yet."))
            }
            None if self.digest_busy => written.child(quiet("Writing…")),
            None => written.child(quiet("Not written yet.")),
        };

        // What can be done about a missing or out-of-date digest.
        if a.summarized > 0 && !current {
            let mut state = div()
                .flex()
                .items_center()
                .gap(px(12.))
                .text_size(px(12.))
                .text_color(t.ink3);
            if self.digest_busy {
                if self.digest.is_some() {
                    state = state.child("Updating…");
                }
            } else {
                if self.digest.is_some() {
                    state = state.child("Written before the latest summaries.");
                }
                state = state.child(
                    div()
                        .id("write-digest")
                        .h(px(30.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(7.))
                        .border_1()
                        .border_color(t.line)
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.ink)
                        .cursor_pointer()
                        .hover(|style| style.bg(t.fill))
                        .on_click(cx.listener(|this, _, _, cx| this.write_digest(cx)))
                        .child(if self.digest.is_some() { "Update" } else { "Write it" }),
                );
            }
            written = written.child(state);
        }
        if let Some(error) = &self.digest_error {
            written = written.child(
                div()
                    .text_size(px(13.))
                    .text_color(t.ink2)
                    .child(format!("It could not be written: {error}")),
            );
        }

        let mut open = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(20.))
            .border_t_1()
            .border_color(t.line)
            .child(label("Open ends worth picking up", t));
        // Two per session first, so one long list does not crowd out the
        // rest; any room left over goes to whatever was held back.
        let mut per_session: Vec<(&str, usize)> = Vec::new();
        let mut chosen: Vec<usize> = Vec::new();
        let mut held_back: Vec<usize> = Vec::new();
        for (i, end) in a.open_ends.iter().enumerate() {
            let taken = match per_session.iter_mut().find(|(key, _)| *key == end.key) {
                Some(entry) => {
                    entry.1 += 1;
                    entry.1
                }
                None => {
                    per_session.push((&end.key, 1));
                    1
                }
            };
            if taken <= OPEN_ENDS_PER_SESSION {
                chosen.push(i);
            } else {
                held_back.push(i);
            }
        }
        chosen.truncate(OPEN_ENDS_SHOWN);
        let room = OPEN_ENDS_SHOWN - chosen.len();
        chosen.extend(held_back.into_iter().take(room));
        chosen.sort_unstable();
        let shown = chosen.len();
        for i in chosen {
            let end = &a.open_ends[i];
            let key = end.key.clone();
            let fill = t.fill;
            open = open.child(
                div()
                    .id(("open-end", i))
                    .flex()
                    .gap(px(14.))
                    .mx(px(-8.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(fill))
                    .on_click(cx.listener(move |this, _, _, cx| this.show_session(&key, cx)))
                    .child(
                        div()
                            .w(px(150.))
                            .flex_shrink_0()
                            .pt(px(1.))
                            .truncate()
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .text_color(t.ink3)
                            .child(end.project.clone()),
                    )
                    .child(div().flex_1().min_w(px(0.)).child(end.text.clone())),
            );
        }
        if a.open_ends.is_empty() {
            open = open.child(div().text_color(t.ink3).child(if a.summarized == 0 {
                "Open ends come from summaries, and there are none yet."
            } else {
                "Nothing was left open."
            }));
        } else if a.open_ends.len() > shown {
            open = open.child(
                div()
                    .pt(px(4.))
                    .text_size(px(12.))
                    .text_color(t.ink3)
                    .child(format!("and {} more", a.open_ends.len() - shown)),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .px(px(32.))
            .py(px(28.))
            .rounded(px(14.))
            .bg(t.raise)
            .shadow(t.card_shadow.clone())
            .child(written)
            .child(open)
    }

    /// Files the period's sessions edited, most recent first. Each row opens
    /// the session it came from.
    fn recent_edits(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let edits = &self.activity.edits;
        let mut section = div()
            .flex()
            .flex_col()
            .px(px(4.))
            .child(label("Recent edits", t));
        if edits.is_empty() {
            return section.child(
                div()
                    .text_color(t.ink3)
                    .child("Edited files are listed once their sessions have summaries."),
            );
        }
        for (i, edit) in edits.iter().enumerate() {
            let key = edit.key.clone();
            let accent = t.accent;
            section = section.child(
                div()
                    .id(("edit", i))
                    .group("edit")
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(16.))
                    .py(px(11.))
                    .when(i + 1 < edits.len(), |el| el.border_b_1().border_color(t.line))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.show_session(&key, cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(MONO_FONT)
                            .text_size(px(13.))
                            .group_hover("edit", move |style| style.text_color(accent))
                            .child(format!("{}/{}", edit.project, edit.file)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(420.))
                            .flex()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .text_color(t.ink3)
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(format!("in {}", edit.title)),
                            )
                            .child(div().flex_shrink_0().child(format!(
                                "· {} {}",
                                day_label(edit.updated_ms).to_lowercase(),
                                clock(edit.updated_ms)
                            ))),
                    ),
            );
        }
        section
    }
}

/// The four headline numbers.
fn totals(a: &Activity, t: &Theme) -> Div {
    let unsummarised = a.sessions - a.summarized;
    div()
        .flex()
        .gap(px(16.))
        .px(px(4.))
        .child(total(
            "Sessions",
            a.sessions.to_string(),
            (unsummarised > 0).then(|| format!("{unsummarised} without a summary")),
            t,
        ))
        .child(total("Active time", hours_label(a.active_ms), None, t))
        .child(total("Open ends", a.open_ends.len().to_string(), None, t))
        .child(total(
            "Tokens",
            compact_number(a.tokens),
            // New input and output. What was re-read from the cache is far
            // larger and says little, so it goes in the note. Only some
            // harnesses record what a session cost.
            match (a.tokens_cached, a.costed) {
                (0, 0) => None,
                (cached, 0) => Some(format!("plus {} re-read from cache", compact_number(cached))),
                (_, costed) => Some(format!(
                    "${:.2} reported for {} {}",
                    a.cost_usd,
                    costed,
                    if costed == 1 { "session" } else { "sessions" }
                )),
            },
            t,
        ))
}

fn total(name: &'static str, value: String, note: Option<String>, t: &Theme) -> Div {
    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().text_size(px(12.)).text_color(t.ink3).child(name))
        .child(
            div()
                .text_size(px(24.))
                .line_height(px(30.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
        .children(note.map(|note| {
            div()
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(t.ink3)
                .child(note)
        }))
}

/// One bar per harness, as long as its share of the busiest one's sessions.
fn by_harness(a: &Activity, t: &Theme) -> Div {
    let most = a.by_harness.iter().map(|s| s.sessions).max().unwrap_or(1).max(1);
    let mut section = div()
        .flex_1()
        .min_w(px(300.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(label("By harness", t));
    if a.by_harness.is_empty() {
        return section.child(div().text_color(t.ink3).child("No sessions."));
    }
    for share in &a.by_harness {
        section = section.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(logo(share.harness, t, 16.))
                .child(
                    div()
                        .w(px(88.))
                        .flex_shrink_0()
                        .text_size(px(13.))
                        .child(share.harness.name()),
                )
                .child(
                    div().flex_1().h(px(6.)).rounded(px(3.)).bg(t.fill).child(
                        div()
                            .h(px(6.))
                            .rounded(px(3.))
                            .bg(t.ink)
                            .w(relative(share.sessions as f32 / most as f32)),
                    ),
                )
                .child(
                    div()
                        .w(px(116.))
                        .flex_shrink_0()
                        .flex()
                        .justify_end()
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .text_color(t.ink3)
                        .child(format!("{} · {}", share.sessions, hours_label(share.active_ms))),
                ),
        );
    }
    section
}

/// Sessions per day, or per week for the longest period. The bar holding
/// today is in the accent colour.
fn over_time(a: &Activity, t: &Theme) -> Div {
    const TALLEST: f32 = 74.;
    let weekly = a.buckets.first().is_some_and(|b| b.days > 1);
    let few = a.buckets.len() <= 13;
    let most = a.buckets.iter().map(|b| b.sessions).max().unwrap_or(0).max(1);

    let mut bars = div()
        .flex()
        .items_end()
        .gap(px(if few { 10. } else { 3. }))
        .h(px(96.));
    for bucket in &a.buckets {
        let height = match bucket.sessions {
            0 => 2.,
            n => (TALLEST * n as f32 / most as f32).max(4.),
        };
        let color = match (bucket.current, bucket.sessions) {
            (true, _) => t.accent,
            (false, 0) => t.ink3.opacity(0.5),
            (false, _) => t.ink,
        };
        bars = bars.child(
            div()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .flex()
                .flex_col()
                .justify_end()
                .items_center()
                .gap(px(4.))
                .when(few && bucket.sessions > 0, |el| {
                    el.child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(11.))
                            .line_height(px(14.))
                            .text_color(t.ink3)
                            .child(bucket.sessions.to_string()),
                    )
                })
                .child(
                    div()
                        .w_full()
                        .h(px(height))
                        .rounded_t(px(if few { 4. } else { 2. }))
                        .bg(color),
                ),
        );
    }

    let mut axis = div()
        .flex()
        .font_family(MONO_FONT)
        .text_size(px(11.))
        .text_color(t.ink3);
    if a.buckets.len() <= 7 {
        // Room for a weekday under every bar.
        axis = axis.gap(px(10.));
        for bucket in &a.buckets {
            let day: String = bucket.first_day.format("%a").to_string().chars().take(2).collect();
            axis = axis.child(div().flex_1().flex().justify_center().child(day));
        }
    } else {
        let first = a
            .buckets
            .first()
            .map(|b| b.first_day.format("%-d %b").to_string())
            .unwrap_or_default();
        axis = axis.justify_between().child(first).child("today");
    }

    div()
        .flex_1()
        .min_w(px(280.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(label(
            SharedString::from(if weekly { "Sessions per week" } else { "Sessions per day" }),
            t,
        ))
        .child(bars)
        .child(axis)
}

/// Active time as a headline figure: "9 h 40 min", "40 min".
fn hours_label(ms: i64) -> String {
    let minutes = ms / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}
