//! Totals over a stretch of recent days: how many sessions, how long, in
//! which harness, on which days, what was left open and what was edited.
//!
//! Everything here is worked out from the sessions already in memory. Open
//! ends and edited files come from summaries, so sessions without one count
//! towards the totals but contribute neither.

use crate::model::{Harness, Session};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

/// How many recent edits are kept.
const EDITS_KEPT: usize = 8;
/// How many of them one session may supply, so a session that touched
/// dozens of files does not fill the list on its own.
const EDITS_PER_SESSION: usize = 2;

/// Something a summary said still needs doing.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenEnd {
    /// The session it came from.
    pub key: String,
    pub project: String,
    pub text: String,
}

/// One harness's share of the period.
#[derive(Debug, Clone, PartialEq)]
pub struct Share {
    pub harness: Harness,
    pub sessions: usize,
    pub active_ms: i64,
}

/// One bar of the sessions-over-time chart: a day, or a week of days.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub first_day: NaiveDate,
    pub days: u32,
    pub sessions: usize,
    /// Whether today falls in this bucket.
    pub current: bool,
}

/// A file a session edited, most recent session first.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub key: String,
    pub project: String,
    pub file: String,
    pub title: String,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Activity {
    pub days: u32,
    pub sessions: usize,
    /// How many of those sessions have a summary.
    pub summarized: usize,
    pub active_ms: i64,
    /// New input plus output.
    pub tokens: u64,
    /// Context re-read from the prompt cache, kept apart from `tokens`.
    pub tokens_cached: u64,
    /// Spend, counting only sessions whose harness reports a cost.
    pub cost_usd: f64,
    /// How many sessions that spend covers.
    pub costed: usize,
    /// Newest session first.
    pub open_ends: Vec<OpenEnd>,
    /// Every harness with at least one session, busiest first.
    pub by_harness: Vec<Share>,
    /// Oldest first, covering the whole period with no gaps.
    pub buckets: Vec<Bucket>,
    pub edits: Vec<Edit>,
    /// Changes whenever the summaries the period is built from change.
    pub fingerprint: u64,
}

fn local_day(ms: i64) -> Option<NaiveDate> {
    Local.timestamp_millis_opt(ms).single().map(|t| t.date_naive())
}

/// The sessions last used within the last `days` calendar days, today
/// included, newest first.
pub fn recent<'a>(
    sessions: impl IntoIterator<Item = &'a Session>,
    days: u32,
    now: DateTime<Local>,
) -> Vec<&'a Session> {
    let today = now.date_naive();
    let first = today - Duration::days(i64::from(days.max(1)) - 1);
    let mut within: Vec<&Session> = sessions
        .into_iter()
        .filter(|s| local_day(s.updated_ms).is_some_and(|day| day >= first && day <= today))
        .collect();
    within.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    within
}

impl Activity {
    /// Totals for the last `days` calendar days, today included. A session
    /// belongs to the day it was last used.
    pub fn over<'a>(sessions: impl IntoIterator<Item = &'a Session>, days: u32) -> Activity {
        Activity::as_of(sessions, days, Local::now())
    }

    pub fn as_of<'a>(
        sessions: impl IntoIterator<Item = &'a Session>,
        days: u32,
        now: DateTime<Local>,
    ) -> Activity {
        let days = days.max(1);
        let today = now.date_naive();
        let first = today - Duration::days(i64::from(days) - 1);

        // Daily bars up to a month; beyond that a bar is a week, ending today.
        let width: u32 = if days > 31 { 7 } else { 1 };
        let count = days.div_ceil(width);
        let mut buckets: Vec<Bucket> = (0..count)
            .map(|i| {
                let back = i64::from((count - 1 - i) * width + width - 1);
                Bucket {
                    first_day: (today - Duration::days(back)).max(first),
                    days: width,
                    sessions: 0,
                    current: i == count - 1,
                }
            })
            .collect();

        let mut activity = Activity { days, ..Default::default() };
        let within = recent(sessions, days, now);

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut seen_files: HashSet<(String, String)> = HashSet::new();
        for session in within {
            let Some(day) = local_day(session.updated_ms) else { continue };
            activity.sessions += 1;
            activity.active_ms += session.active_ms;
            activity.tokens += session.tokens();
            activity.tokens_cached += session.tokens_cached;
            if let Some(cost) = session.cost_usd {
                activity.cost_usd += cost;
                activity.costed += 1;
            }

            let back = (today - day).num_days() as u32;
            let bucket = (count - 1).saturating_sub(back / width) as usize;
            buckets[bucket].sessions += 1;

            match activity.by_harness.iter_mut().find(|share| share.harness == session.harness) {
                Some(share) => {
                    share.sessions += 1;
                    share.active_ms += session.active_ms;
                }
                None => activity.by_harness.push(Share {
                    harness: session.harness,
                    sessions: 1,
                    active_ms: session.active_ms,
                }),
            }

            let Some(summary) = &session.summary else { continue };
            activity.summarized += 1;
            session.key().hash(&mut hasher);
            summary.summary.hash(&mut hasher);
            summary.open_ends.hash(&mut hasher);

            let project = session.project().to_string();
            for text in &summary.open_ends {
                activity.open_ends.push(OpenEnd {
                    key: session.key(),
                    project: project.clone(),
                    text: text.clone(),
                });
            }
            let mut supplied = 0;
            for file in &summary.files {
                if activity.edits.len() == EDITS_KEPT || supplied == EDITS_PER_SESSION {
                    break;
                }
                if seen_files.insert((project.clone(), file.clone())) {
                    supplied += 1;
                    activity.edits.push(Edit {
                        key: session.key(),
                        project: project.clone(),
                        file: file.clone(),
                        title: session.display_title().to_string(),
                        updated_ms: session.updated_ms,
                    });
                }
            }
        }
        activity.by_harness.sort_by_key(|share| std::cmp::Reverse(share.sessions));
        activity.buckets = buckets;
        activity.fingerprint = hasher.finish();
        activity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Outcome, Summary};

    fn noon(now: DateTime<Local>, days_ago: i64) -> i64 {
        let day = now.date_naive() - Duration::days(days_ago);
        Local
            .from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn session(harness: Harness, id: &str, updated_ms: i64) -> Session {
        let mut s = Session::new(harness, id, "test");
        s.cwd = "/work/api".into();
        s.title = format!("session {id}");
        s.updated_ms = updated_ms;
        s.active_ms = 60_000;
        s
    }

    fn summary(open_ends: &[&str], files: &[&str]) -> Summary {
        Summary {
            title: "t".into(),
            summary: "s".into(),
            asked: "a".into(),
            happened: Vec::new(),
            open_ends: open_ends.iter().map(|s| s.to_string()).collect(),
            friction: Vec::new(),
            outcome: Outcome::Done,
            files: files.iter().map(|s| s.to_string()).collect(),
            step_starts: Vec::new(),
            blocks_seen: 0,
        }
    }

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 19, 15, 0, 0).single().unwrap()
    }

    #[test]
    fn counts_only_the_days_asked_for() {
        let now = now();
        let sessions = vec![
            session(Harness::ClaudeCode, "today", noon(now, 0)),
            session(Harness::Codex, "edge", noon(now, 6)),
            session(Harness::Codex, "too-old", noon(now, 7)),
        ];
        let week = Activity::as_of(&sessions, 7, now);
        assert_eq!(week.sessions, 2);
        assert_eq!(week.active_ms, 120_000);
        assert_eq!(week.buckets.len(), 7);
        assert_eq!(week.buckets[0].sessions, 1);
        assert_eq!(week.buckets[6].sessions, 1);
        assert!(week.buckets[6].current);
        assert_eq!(week.buckets[0].first_day, now.date_naive() - Duration::days(6));
    }

    #[test]
    fn long_ranges_are_charted_by_week() {
        let now = now();
        let sessions = vec![
            session(Harness::ClaudeCode, "a", noon(now, 0)),
            session(Harness::ClaudeCode, "b", noon(now, 6)),
            session(Harness::ClaudeCode, "c", noon(now, 7)),
            session(Harness::ClaudeCode, "d", noon(now, 89)),
        ];
        let quarter = Activity::as_of(&sessions, 90, now);
        assert_eq!(quarter.sessions, 4);
        assert_eq!(quarter.buckets.len(), 13);
        assert_eq!(quarter.buckets[12].sessions, 2);
        assert_eq!(quarter.buckets[11].sessions, 1);
        assert_eq!(quarter.buckets[0].sessions, 1);
        assert_eq!(quarter.buckets.iter().map(|b| b.sessions).sum::<usize>(), 4);
        // The oldest bar never starts before the period does.
        assert_eq!(quarter.buckets[0].first_day, now.date_naive() - Duration::days(89));
    }

    #[test]
    fn open_ends_and_edits_come_from_summaries_newest_first() {
        let now = now();
        let mut older = session(Harness::Codex, "older", noon(now, 2));
        older.summary = Some(summary(&["ship it"], &["src/a.rs", "src/b.rs"]));
        let mut newer = session(Harness::ClaudeCode, "newer", noon(now, 1));
        newer.summary = Some(summary(&["fix the test"], &["src/a.rs"]));
        let bare = session(Harness::ClaudeCode, "bare", noon(now, 0));

        let activity = Activity::as_of([&older, &newer, &bare], 7, now);
        assert_eq!(activity.sessions, 3);
        assert_eq!(activity.summarized, 2);
        let texts: Vec<&str> = activity.open_ends.iter().map(|o| o.text.as_str()).collect();
        assert_eq!(texts, ["fix the test", "ship it"]);
        // A file edited twice is listed once, under the newer session.
        let files: Vec<(&str, &str)> = activity
            .edits
            .iter()
            .map(|e| (e.file.as_str(), e.key.as_str()))
            .collect();
        assert_eq!(files, [("src/a.rs", newer.key().as_str()), ("src/b.rs", older.key().as_str())]);
        assert_eq!(activity.by_harness[0].harness, Harness::ClaudeCode);
        assert_eq!(activity.by_harness[0].sessions, 2);
    }

    #[test]
    fn one_session_supplies_only_a_couple_of_edits() {
        let now = now();
        let mut busy = session(Harness::ClaudeCode, "busy", noon(now, 0));
        busy.summary = Some(summary(&[], &["a", "b", "c", "d"]));
        let mut quiet = session(Harness::ClaudeCode, "quiet", noon(now, 1));
        quiet.summary = Some(summary(&[], &["e"]));
        let activity = Activity::as_of([&busy, &quiet], 7, now);
        let files: Vec<&str> = activity.edits.iter().map(|e| e.file.as_str()).collect();
        assert_eq!(files, ["a", "b", "e"]);
    }

    #[test]
    fn the_fingerprint_follows_the_summaries() {
        let now = now();
        let mut s = session(Harness::Codex, "one", noon(now, 1));
        let empty = Activity::as_of([&s], 7, now).fingerprint;
        s.summary = Some(summary(&[], &[]));
        let first = Activity::as_of([&s], 7, now).fingerprint;
        s.summary = Some(summary(&["something new"], &[]));
        let second = Activity::as_of([&s], 7, now).fingerprint;
        assert_ne!(empty, first);
        assert_ne!(first, second);
        assert_eq!(second, Activity::as_of([&s], 7, now).fingerprint);
    }

    #[test]
    fn spend_counts_only_sessions_that_report_it() {
        let now = now();
        let mut paid = session(Harness::OpenCode, "paid", noon(now, 0));
        paid.cost_usd = Some(1.5);
        let unknown = session(Harness::ClaudeCode, "unknown", noon(now, 0));
        let activity = Activity::as_of([&paid, &unknown], 7, now);
        assert_eq!(activity.cost_usd, 1.5);
        assert_eq!(activity.costed, 1);
    }
}
