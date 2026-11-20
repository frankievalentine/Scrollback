use serde::{Deserialize, Serialize};
use std::path::Path;

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

/// A coding-agent harness whose session history scrollback can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
    Pi,
    Cursor,
}

impl Harness {
    pub const ALL: [Harness; 5] = [
        Harness::ClaudeCode,
        Harness::Codex,
        Harness::OpenCode,
        Harness::Pi,
        Harness::Cursor,
    ];

    /// Stable identifier used in the index and in session keys.
    pub fn slug(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
            Harness::Pi => "pi",
            Harness::Cursor => "cursor",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Codex => "Codex",
            Harness::OpenCode => "OpenCode",
            Harness::Pi => "pi",
            Harness::Cursor => "Cursor",
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
    T3Code,
}

impl Origin {
    pub const ALL: [Origin; 8] = [
        Origin::ClaudeCode,
        Origin::ClaudeDesktop,
        Origin::Codex,
        Origin::CodexDesktop,
        Origin::OpenCode,
        Origin::Pi,
        Origin::Cursor,
        Origin::T3Code,
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
            Origin::T3Code => "t3-code",
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
            Origin::T3Code => "T3 Code",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Origin> {
        Origin::ALL.into_iter().find(|o| o.slug() == slug)
    }

    /// Whether this is an app that runs sessions, as opposed to a harness's
    /// own command-line tool.
    pub fn is_app(self) -> bool {
        matches!(
            self,
            Origin::ClaudeDesktop | Origin::CodexDesktop | Origin::Cursor | Origin::T3Code
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
        let repository = ["/.claude/worktrees/", "/.worktrees/"]
            .iter()
            .find_map(|marker| self.cwd.find(marker))
            .map_or(self.cwd.as_str(), |at| &self.cwd[..at]);
        Path::new(repository)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    }

    /// How long the session was in use. A session resumed days later counts
    /// only the time around its events, not the days in between.
    pub fn duration_ms(&self) -> i64 {
        self.active_ms
    }

    /// Widen the session's time range to include `ts`.
    pub(crate) fn touch(&mut self, ts: i64) {
        let gap = ts - self.updated_ms;
        if self.updated_ms != 0 && gap > 0 && gap <= IDLE_GAP_MS {
            self.active_ms += gap;
        }
        if self.started_ms == 0 || ts < self.started_ms {
            self.started_ms = ts;
        }
        if ts > self.updated_ms {
            self.updated_ms = ts;
        }
    }
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
        s.cwd = String::new();
        assert_eq!(s.project(), "");
    }
}
