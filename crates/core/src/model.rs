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
}

/// A coding-agent harness whose session history scrollback can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
    Pi,
}

impl Harness {
    pub const ALL: [Harness; 4] = [
        Harness::ClaudeCode,
        Harness::Codex,
        Harness::OpenCode,
        Harness::Pi,
    ];

    /// Stable identifier used in the index and in session keys.
    pub fn slug(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
            Harness::Pi => "pi",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Codex => "Codex",
            Harness::OpenCode => "OpenCode",
            Harness::Pi => "pi",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.slug() == slug)
    }
}

/// One session, normalised across harnesses. Counts and token totals are what
/// the harness recorded; `cost_usd` is set only when the harness recorded one.
#[derive(Debug, Clone)]
pub struct Session {
    pub harness: Harness,
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
}

impl Session {
    pub fn new(harness: Harness, id: impl Into<String>, source: impl Into<String>) -> Session {
        Session {
            harness,
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
        Path::new(&self.cwd)
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
