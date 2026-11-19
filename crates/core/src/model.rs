use std::path::Path;

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
    pub user_turns: u32,
    pub assistant_turns: u32,
    pub tool_calls: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: Option<f64>,
    /// The file or database the session was read from.
    pub source: String,
    /// Set for sub-agent sessions, which are hidden from the main list.
    pub parent: Option<String>,
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
            user_turns: 0,
            assistant_turns: 0,
            tool_calls: 0,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            source: source.into(),
            parent: None,
        }
    }

    /// Unique across harnesses, e.g. `codex:019e6d09-…`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.harness.slug(), self.id)
    }

    /// The last component of the working directory.
    pub fn project(&self) -> &str {
        Path::new(&self.cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    }

    pub fn duration_ms(&self) -> i64 {
        (self.updated_ms - self.started_ms).max(0)
    }

    /// Widen the session's time range to include `ts`.
    pub(crate) fn touch(&mut self, ts: i64) {
        if self.started_ms == 0 || ts < self.started_ms {
            self.started_ms = ts;
        }
        if ts > self.updated_ms {
            self.updated_ms = ts;
        }
    }
}
