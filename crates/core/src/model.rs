use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A pause longer than this between two events is treated as time away.
pub const IDLE_GAP_MS: i64 = 5 * 60 * 1000;

/// How a session ended, as judged by the summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Done,
    Partial,
    Stalled,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Done => "Done",
            Outcome::Partial => "Partial",
            Outcome::Stalled => "Stalled",
        }
    }
}

/// A written account of one session. Everything except `files` is written by
/// the model; `files` is taken directly from the session's edit tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub title: String,
    pub summary: String,
    pub asked: String,
    pub happened: Vec<String>,
    pub open_ends: Vec<String>,
    pub friction: Vec<String>,
    pub outcome: Outcome,
    #[serde(default)]
    pub files: Vec<String>,
    /// For each step in `happened`, the transcript block it begins at,
    /// counting from zero. Empty for summaries written before steps were
    /// tied to the transcript, or when the model's answer could not be used.
    #[serde(default)]
    pub step_starts: Vec<usize>,
    /// How many transcript blocks the session had when this was written, so
    /// anything after them can be shown as newer than the summary.
    #[serde(default)]
    pub blocks_seen: usize,
}

/// A coding-agent harness whose session history Scrollback can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
    Pi,
    Cursor,
    Antigravity,
    Fx,
    Grok,
    Hermes,
    Copilot,
    CopilotChat,
}

impl Harness {
    pub const ALL: [Harness; 11] = [
        Harness::ClaudeCode,
        Harness::Codex,
        Harness::OpenCode,
        Harness::Pi,
        Harness::Cursor,
        Harness::Antigravity,
        Harness::Fx,
        Harness::Grok,
        Harness::Hermes,
        Harness::Copilot,
        Harness::CopilotChat,
    ];

    /// Stable identifier used in the index and in session keys.
    pub fn slug(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
            Harness::Pi => "pi",
            Harness::Cursor => "cursor",
            Harness::Antigravity => "antigravity",
            Harness::Fx => "fx",
            Harness::Grok => "grok",
            Harness::Hermes => "hermes",
            Harness::Copilot => "copilot",
            Harness::CopilotChat => "copilot-chat",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Codex => "Codex",
            Harness::OpenCode => "OpenCode",
            Harness::Pi => "pi",
            Harness::Cursor => "Cursor",
            Harness::Antigravity => "Antigravity",
            Harness::Fx => "fx",
            Harness::Grok => "Grok",
            Harness::Hermes => "Hermes",
            Harness::Copilot => "Copilot CLI",
            Harness::CopilotChat => "Copilot",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.slug() == slug)
    }

    /// Where a session of this harness comes from unless something says
    /// otherwise: the harness's own command-line tool or app.
    pub fn origin(self) -> Origin {
        match self {
            Harness::ClaudeCode => Origin::ClaudeCode,
            Harness::Codex => Origin::Codex,
            Harness::OpenCode => Origin::OpenCode,
            Harness::Pi => Origin::Pi,
            Harness::Cursor => Origin::Cursor,
            Harness::Antigravity => Origin::Antigravity,
            Harness::Fx => Origin::Fx,
            Harness::Grok => Origin::Grok,
            Harness::Hermes => Origin::Hermes,
            Harness::Copilot => Origin::Copilot,
            Harness::CopilotChat => Origin::CopilotChat,
        }
    }
}

/// Where a session was run from, as the sidebar lists it. A harness says how
/// a session is stored and read; an origin says which program the user was
/// sitting in. Claude Code and Codex each have a desktop app that writes the
/// same files as the command-line tool, and T3 Code drives other harnesses,
/// so one harness can have several origins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    CodexDesktop,
    OpenCode,
    Pi,
    Cursor,
    Antigravity,
    Fx,
    Grok,
    Hermes,
    Copilot,
    CopilotChat,
    T3Code,
    Paseo,
}

impl Origin {
    pub const ALL: [Origin; 15] = [
        Origin::ClaudeCode,
        Origin::ClaudeDesktop,
        Origin::Codex,
        Origin::CodexDesktop,
        Origin::OpenCode,
        Origin::Pi,
        Origin::Cursor,
        Origin::Antigravity,
        Origin::Fx,
        Origin::Grok,
        Origin::Hermes,
        Origin::Copilot,
        Origin::CopilotChat,
        Origin::T3Code,
        Origin::Paseo,
    ];

    /// Stable identifier used in the index.
    pub fn slug(self) -> &'static str {
        match self {
            Origin::ClaudeCode => "claude-code",
            Origin::ClaudeDesktop => "claude-desktop",
            Origin::Codex => "codex",
            Origin::CodexDesktop => "codex-desktop",
            Origin::OpenCode => "opencode",
            Origin::Pi => "pi",
            Origin::Cursor => "cursor",
            Origin::Antigravity => "antigravity",
            Origin::Fx => "fx",
            Origin::Grok => "grok",
            Origin::Hermes => "hermes",
            Origin::Copilot => "copilot",
            Origin::CopilotChat => "copilot-chat",
            Origin::T3Code => "t3-code",
            Origin::Paseo => "paseo",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Origin::ClaudeCode => "Claude Code",
            // The desktop apps go by their own names: the Claude app, and
            // the ChatGPT app, which is where Codex's desktop sessions run.
            Origin::ClaudeDesktop => "Claude",
            Origin::Codex => "Codex",
            Origin::CodexDesktop => "ChatGPT",
            Origin::OpenCode => "OpenCode",
            Origin::Pi => "pi",
            Origin::Cursor => "Cursor",
            Origin::Antigravity => "Antigravity",
            Origin::Fx => "fx",
            Origin::Grok => "Grok",
            Origin::Hermes => "Hermes",
            Origin::Copilot => "Copilot CLI",
            Origin::CopilotChat => "Copilot",
            Origin::T3Code => "T3 Code",
            Origin::Paseo => "Paseo",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Origin> {
        Origin::ALL.into_iter().find(|o| o.slug() == slug)
    }

    /// Whether the sidebar lists this source even with no sessions: the
    /// first command-line tools Scrollback read. Others appear once they
    /// have sessions.
    pub fn always_listed(self) -> bool {
        matches!(self, Origin::ClaudeCode | Origin::Codex | Origin::OpenCode | Origin::Pi)
    }

    /// Whether this is an app that runs sessions, as opposed to a harness's
    /// own command-line tool.
    pub fn is_app(self) -> bool {
        matches!(
            self,
            Origin::ClaudeDesktop
                | Origin::CodexDesktop
                | Origin::Cursor
                | Origin::Antigravity
                | Origin::CopilotChat
                | Origin::T3Code
                | Origin::Paseo
        )
    }
}

/// One session, normalised across harnesses. Counts and token totals are what
/// the harness recorded; `cost_usd` is set only when the harness recorded one.
#[derive(Debug, Clone)]
pub struct Session {
    pub harness: Harness,
    /// The program the session was run from.
    pub origin: Origin,
    /// The harness's own session id.
    pub id: String,
    /// A harness-provided title when there is one, otherwise the first prompt.
    pub title: String,
    pub first_prompt: String,
    pub cwd: String,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub started_ms: i64,
    pub updated_ms: i64,
    /// Time the session was actually in use: the sum of gaps between
    /// consecutive events, ignoring gaps longer than `IDLE_GAP_MS`.
    pub active_ms: i64,
    /// The same time split by the local day it was spent on, keyed by date
    /// (`2026-11-20`), so a session that ran over several days counts on
    /// each of them.
    pub active_days: BTreeMap<String, i64>,
    pub user_turns: u32,
    pub assistant_turns: u32,
    pub tool_calls: u32,
    /// Input the model had not seen before: new prompt text, tool results
    /// and anything written to the prompt cache.
    pub tokens_in: u64,
    /// Output, including reasoning.
    pub tokens_out: u64,
    /// Context re-read from the prompt cache. Every turn re-reads the whole
    /// conversation so far, so this dwarfs the other two and is kept apart.
    pub tokens_cached: u64,
    pub cost_usd: Option<f64>,
    /// The file or database the session was read from.
    pub source: String,
    /// Set for sub-agent sessions, which are hidden from the main list.
    pub parent: Option<String>,
    pub summary: Option<Summary>,
    /// True when the session has had activity since its summary was written.
    pub summary_stale: bool,
    /// The session's last activity at the time its summary was written, so
    /// the summary covers everything up to then.
    pub summary_for_ms: Option<i64>,
}

impl Session {
    pub fn new(harness: Harness, id: impl Into<String>, source: impl Into<String>) -> Session {
        Session {
            harness,
            origin: harness.origin(),
            id: id.into(),
            title: String::new(),
            first_prompt: String::new(),
            cwd: String::new(),
            branch: None,
            model: None,
            started_ms: 0,
            updated_ms: 0,
            active_ms: 0,
            active_days: BTreeMap::new(),
            user_turns: 0,
            assistant_turns: 0,
            tool_calls: 0,
            tokens_in: 0,
            tokens_out: 0,
            tokens_cached: 0,
            cost_usd: None,
            source: source.into(),
            parent: None,
            summary: None,
            summary_stale: false,
            summary_for_ms: None,
        }
    }

    /// Unique across harnesses, e.g. `codex:019e6d09-…`.
    /// New input plus output: the work done, leaving out cached re-reads.
    pub fn tokens(&self) -> u64 {
        self.tokens_in + self.tokens_out
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.harness.slug(), self.id)
    }

    /// The summary's title when there is one, otherwise the harness's.
    pub fn display_title(&self) -> &str {
        match &self.summary {
            Some(summary) if !summary.title.is_empty() => &summary.title,
            _ => &self.title,
        }
    }

    /// The last component of the working directory.
    pub fn project(&self) -> &str {
        // A worktree an agent made inside a repository belongs to that
        // repository, not to a project named after the worktree.
        let parts: Vec<&str> = crate::path_parts(&self.cwd).collect();
        let worktrees = parts.iter().enumerate().position(|(i, part)| {
            i > 0
                && i + 1 < parts.len()
                && (*part == ".worktrees"
                    || (*part == ".claude" && parts[i + 1] == "worktrees" && i + 2 < parts.len()))
        });
        match worktrees {
            Some(at) => parts[at - 1],
            None => parts.last().copied().unwrap_or(""),
        }
    }

    /// How long the session was in use. A session resumed days later counts
    /// only the time around its events, not the days in between.
    pub fn duration_ms(&self) -> i64 {
        self.active_ms
    }

    /// Count `gap` of time in use, ending at `ts`, towards the session and
    /// towards the day it ended on.
    pub(crate) fn add_active(&mut self, ts: i64, gap: i64) {
        self.active_ms += gap;
        *self.active_days.entry(day_key(ts)).or_insert(0) += gap;
    }

    /// Time in use on each local day, oldest first. Sessions indexed before
    /// time was kept by day count all of it on the day they were last used.
    pub fn active_by_day(&self) -> Vec<(chrono::NaiveDate, i64)> {
        if self.active_days.is_empty() {
            return local_date(self.updated_ms)
                .filter(|_| self.active_ms > 0)
                .map(|date| vec![(date, self.active_ms)])
                .unwrap_or_default();
        }
        self.active_days
            .iter()
            .filter_map(|(day, ms)| {
                chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok().map(|date| (date, *ms))
            })
            .collect()
    }

    /// Widen the session's time range to include `ts`.
    pub(crate) fn touch(&mut self, ts: i64) {
        let gap = ts - self.updated_ms;
        if self.updated_ms != 0 && gap > 0 && gap <= IDLE_GAP_MS {
            self.add_active(ts, gap);
        }
        if self.started_ms == 0 || ts < self.started_ms {
            self.started_ms = ts;
        }
        if ts > self.updated_ms {
            self.updated_ms = ts;
        }
    }
}

/// The local date of a moment, as Unix milliseconds.
fn local_date(ms: i64) -> Option<chrono::NaiveDate> {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.date_naive())
}

/// The key a moment's day is kept under in `active_days`.
pub(crate) fn day_key(ms: i64) -> String {
    local_date(ms).map(|date| date.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_belongs_to_its_repository() {
        let mut s = Session::new(Harness::ClaudeCode, "s", "test");
        s.cwd = "/Users/me/Development/runyard".into();
        assert_eq!(s.project(), "runyard");
        s.cwd = "/Users/me/Development/runyard/.claude/worktrees/adoring-mirzakhani-bd3bb0".into();
        assert_eq!(s.project(), "runyard");
        s.cwd = "/Users/me/Development/runyard/.worktrees/fix-login/apps/web".into();
        assert_eq!(s.project(), "runyard");
        s.cwd = String::new();
        assert_eq!(s.project(), "");
    }

    #[test]
    fn time_in_use_is_kept_on_the_day_it_was_spent() {
        use chrono::TimeZone;
        let at = |day: u32, hour: u32, minute: u32| {
            chrono::Local.with_ymd_and_hms(2026, 11, day, hour, minute, 0).unwrap().timestamp_millis()
        };
        let mut s = Session::new(Harness::ClaudeCode, "s", "test");
        // Two minutes late on the 18th, then three minutes on the 20th.
        s.touch(at(18, 23, 50));
        s.touch(at(18, 23, 52));
        s.touch(at(20, 9, 0));
        s.touch(at(20, 9, 3));
        assert_eq!(s.active_ms, 5 * 60_000);
        let days = s.active_by_day();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].1, 2 * 60_000);
        assert_eq!(days[1].1, 3 * 60_000);
    }

    #[test]
    fn a_windows_folder_names_its_project() {
        let mut s = Session::new(Harness::ClaudeCode, "s", "test");
        s.cwd = r"C:\Users\me\dev\runyard".into();
        assert_eq!(s.project(), "runyard");
        s.cwd = r"C:\Users\me\dev\runyard\.claude\worktrees\quiet-fox".into();
        assert_eq!(s.project(), "runyard");
        // A backslash in a Unix folder name is part of the name.
        s.cwd = r"/home/me/odd\name".into();
        assert_eq!(s.project(), r"odd\name");
    }
}
