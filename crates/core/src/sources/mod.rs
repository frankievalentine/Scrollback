//! One module per harness. File-based harnesses expose `files()` and
//! `parse(path)`, so the indexer can skip files that have not changed.
//! Database-backed harnesses expose `scan()`.

pub mod antigravity;
pub mod claude_code;
pub mod copilot;
pub mod fx;
pub mod grok;
pub mod hermes;
pub mod paseo;
pub mod vscode;
pub mod codex;
pub mod cursor;
pub mod opencode;
pub mod pi;
pub mod t3code;

use crate::model::{Harness, Session};
use std::path::{Path, PathBuf};

/// The folder a harness keeps its sessions in, unless the settings name
/// another.
pub fn default_folder(harness: Harness) -> PathBuf {
    // Each harness uses the same folder under the home folder on every
    // system, and each has a variable that moves it. Cursor is a desktop
    // app, so it is wherever the system keeps those.
    let home = crate::home();
    let or_home = |variable: &str, usual: &str| {
        crate::env_dir(variable).unwrap_or_else(|| home.join(usual))
    };
    match harness {
        Harness::ClaudeCode => or_home("CLAUDE_CONFIG_DIR", ".claude").join("projects"),
        Harness::Codex => or_home("CODEX_HOME", ".codex"),
        Harness::OpenCode => crate::xdg_data().join("opencode"),
        Harness::Pi => crate::env_dir("PI_CODING_AGENT_DIR")
            .unwrap_or_else(|| home.join(".pi").join("agent"))
            .join("sessions"),
        Harness::Cursor => crate::app_support().join("Cursor").join("User"),
        Harness::Antigravity => home.join(".gemini").join("antigravity"),
        Harness::Fx => or_home("FX_HOME", ".fx").join("sessions"),
        Harness::Grok => or_home("GROK_HOME", ".grok").join("sessions"),
        Harness::Hermes => crate::env_dir("HERMES_HOME").unwrap_or_else(|| {
            match cfg!(windows) {
                true => crate::env_dir("LOCALAPPDATA")
                    .unwrap_or_else(|| home.join("AppData").join("Local"))
                    .join("hermes"),
                false => home.join(".hermes"),
            }
        }),
        Harness::Copilot => or_home("COPILOT_HOME", ".copilot"),
        Harness::CopilotChat => crate::app_support().join("Code").join("User"),
    }
}

/// The folder a harness's sessions are read from: the one chosen in the
/// settings if there is one, otherwise the usual place.
pub fn location(harness: Harness) -> PathBuf {
    crate::Config::load()
        .folders
        .get(harness.slug())
        .filter(|folder| !folder.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_folder(harness))
}

/// Every file under `root` (recursively) that `keep` accepts.
pub(crate) fn walk(root: &Path, keep: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() && keep(&path) => found.push(path),
                _ => {}
            }
        }
    }
    found.sort();
    found
}

/// The folder a session belongs to, given how many of its lines were written
/// in each folder. A session can start in one place and move: a scratch
/// folder before the project is chosen, a project that is later renamed, a
/// sub-folder for a few commands. The session is filed under the folder
/// where most of it happened, counting sub-folders towards their parent,
/// and preferring folders that still exist.
pub(crate) fn working_folder(lines: &std::collections::HashMap<String, u32>) -> Option<String> {
    choose_folder(lines, &|folder| Path::new(folder).is_dir())
}

fn choose_folder(
    lines: &std::collections::HashMap<String, u32>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let inside = |inner: &str, outer: &str| {
        inner.len() > outer.len()
            && inner.starts_with(outer)
            && (outer.ends_with(['/', '\\']) || matches!(inner.as_bytes()[outer.len()], b'/' | b'\\'))
    };
    let mut scored: Vec<(&str, u32, bool)> = lines
        .iter()
        .filter_map(|(folder, own)| {
            let below: u32 = lines
                .iter()
                .filter(|(other, _)| inside(other, folder))
                .map(|(_, n)| *n)
                .sum();
            let total = own + below;
            // A folder only passed through on the way somewhere else (the
            // home folder, say) does not get credit for what happened below.
            (*own * 10 >= total).then_some((folder.as_str(), total, exists(folder)))
        })
        .collect();
    if scored.iter().any(|(_, _, there)| *there) {
        scored.retain(|(_, _, there)| *there);
    }
    scored
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(a.0.len().cmp(&b.0.len())).then(a.0.cmp(b.0)))
        .map(|(folder, _, _)| folder.to_string())
}

/// The part of a user-role message the user typed. Harnesses prepend blocks
/// such as `<system-reminder>…</system-reminder>` or `<environment_context>…`
/// to it; those are removed. Returns `None` when nothing typed remains.
pub(crate) fn typed_prompt(text: &str) -> Option<&str> {
    let mut rest = text.trim_start();
    while rest.starts_with('<') {
        let name_len = rest[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len() - 1);
        if name_len == 0 {
            break;
        }
        let closing = format!("</{}>", &rest[1..1 + name_len]);
        match rest.find(&closing) {
            Some(at) => rest = rest[at + closing.len()..].trim_start(),
            None => return None,
        }
    }
    let rest = rest.trim();
    let noise = rest.is_empty()
        || rest.starts_with("Caveat:")
        || rest.starts_with("[Request interrupted");
    (!noise).then_some(rest)
}

/// The file, folder or command a tool call was aimed at, from its
/// arguments, whatever the harness calls them.
pub(crate) fn tool_target(args: &serde_json::Value) -> Option<String> {
    // Arguments sometimes arrive as a JSON string of their own.
    if let Some(raw) = args.as_str() {
        return serde_json::from_str::<serde_json::Value>(raw).ok().and_then(|v| tool_target(&v));
    }
    const KEYS: [&str; 12] = [
        "file_path", "path", "filePath", "target_file", "file", "command", "cmd", "pattern",
        "query", "url", "glob", "commandLine",
    ];
    for key in KEYS {
        match &args[key] {
            serde_json::Value::String(text) if !text.trim().is_empty() => {
                return Some(crate::squash(text, 120));
            }
            serde_json::Value::Array(parts) => {
                let joined: Vec<&str> = parts.iter().filter_map(|p| p.as_str()).collect();
                if !joined.is_empty() {
                    return Some(crate::squash(&joined.join(" "), 120));
                }
            }
            _ => {}
        }
    }
    None
}

/// What a reader found in a session, in order, each with its time.
#[derive(Default)]
pub(crate) struct Found {
    pub events: Vec<(i64, crate::transcript::Event)>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub parent: Option<String>,
    pub started_ms: Option<i64>,
    pub updated_ms: Option<i64>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tokens_cached: u64,
}

impl Found {
    pub fn push(&mut self, ts: i64, event: crate::transcript::Event) {
        self.events.push((ts, event));
    }

    /// The session these make: counts, times, first prompt and title.
    pub fn session(&self, harness: Harness, id: &str, source: &str) -> Session {
        use crate::transcript::Event;
        let mut session = Session::new(harness, id, source);
        if let Some(started) = self.started_ms {
            session.touch(started);
        }
        for (ts, event) in &self.events {
            if *ts > 0 {
                session.touch(*ts);
            }
            match event {
                Event::Prompt(text) => {
                    session.user_turns += 1;
                    if session.first_prompt.is_empty() {
                        session.first_prompt = crate::squash(text, 600);
                    }
                }
                Event::Reply(_) => session.assistant_turns += 1,
                Event::Tool { .. } => session.tool_calls += 1,
                Event::Note(_) => {}
            }
        }
        if let Some(updated) = self.updated_ms {
            if updated > session.updated_ms {
                session.updated_ms = updated;
            }
        }
        session.cwd = self.cwd.clone().unwrap_or_default();
        session.model = self.model.clone();
        session.branch = self.branch.clone();
        session.parent = self.parent.clone();
        session.tokens_in = self.tokens_in;
        session.tokens_out = self.tokens_out;
        session.tokens_cached = self.tokens_cached;
        session.title = self
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| crate::squash(&session.first_prompt, 90));
        session
    }

    pub fn into_events(self) -> Vec<crate::transcript::Event> {
        self.events.into_iter().map(|(_, event)| event).collect()
    }
}

/// Read a JSON Lines file, skipping lines that are not JSON.
pub(crate) fn json_lines(path: &Path) -> anyhow::Result<Vec<serde_json::Value>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    Ok(std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect())
}

/// Turn `file:///Users/me/My%20Project` into a path; Windows drives become
/// `C:\...`.
pub(crate) fn file_uri_path(uri: &str) -> Option<String> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| encoded.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    let path = String::from_utf8(out).ok()?;
    let b = path.as_bytes();
    let drive = b.len() > 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    let path = if drive { path[1..].replace('/', "\\") } else { path };
    let path = path.trim_end_matches(['/', '\\']).to_string();
    (!path.is_empty()).then_some(path)
}

/// Unix milliseconds from a timestamp in any of the shapes harnesses use:
/// RFC 3339 text, seconds, or milliseconds.
pub(crate) fn any_ms(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::String(text) => crate::parse_ts(text),
        serde_json::Value::Number(n) => {
            let x = n.as_f64()?;
            // Anything before 2001 in milliseconds is a count of seconds.
            Some(if x < 1e12 { (x * 1000.0) as i64 } else { x as i64 })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn folder(counts: &[(&str, u32)], existing: &[&str]) -> Option<String> {
        let lines: HashMap<String, u32> = counts.iter().map(|(f, n)| (f.to_string(), *n)).collect();
        choose_folder(&lines, &|f| existing.contains(&f))
    }

    #[test]
    fn a_session_is_filed_where_most_of_it_happened() {
        // Started in a scratch folder, then moved into the project.
        let counts = [("/tmp/scratch-1", 233), ("/p/scrollback", 1323), ("/p/scrollback/crates/app", 10)];
        assert_eq!(
            folder(&counts, &["/tmp/scratch-1", "/p/scrollback", "/p/scrollback/crates/app"]).as_deref(),
            Some("/p/scrollback")
        );
    }

    #[test]
    fn sub_folders_count_towards_the_project() {
        let counts = [("/d/runyard", 300), ("/d/runyard/apps/web/src", 900), ("/d/other", 1000)];
        let all = ["/d/runyard", "/d/runyard/apps/web/src", "/d/other"];
        assert_eq!(folder(&counts, &all).as_deref(), Some("/d/runyard"));
    }

    #[test]
    fn windows_sub_folders_count_towards_the_project() {
        let counts = [(r"C:\d\runyard", 300), (r"C:\d\runyard\apps\web", 900), (r"C:\d\other", 1000)];
        let all = [r"C:\d\runyard", r"C:\d\runyard\apps\web", r"C:\d\other"];
        assert_eq!(folder(&counts, &all).as_deref(), Some(r"C:\d\runyard"));
    }

    #[test]
    fn a_folder_only_passed_through_takes_no_credit() {
        let counts = [("/Users/me", 5), ("/Users/me/p/a", 500)];
        assert_eq!(folder(&counts, &["/Users/me", "/Users/me/p/a"]).as_deref(), Some("/Users/me/p/a"));
    }

    #[test]
    fn a_renamed_project_is_filed_under_its_new_name() {
        // Most lines predate the rename, but that folder is gone.
        let counts = [("/p/campsite-exploration", 601), ("/p/Workbarn", 16)];
        assert_eq!(folder(&counts, &["/p/Workbarn"]).as_deref(), Some("/p/Workbarn"));
        // With nothing left on disk, the busiest folder still names it.
        assert_eq!(folder(&counts, &[]).as_deref(), Some("/p/campsite-exploration"));
    }

    #[test]
    fn similar_names_are_not_mistaken_for_sub_folders() {
        let counts = [("/p/power", 10), ("/p/powerhouse", 500)];
        assert_eq!(folder(&counts, &["/p/power", "/p/powerhouse"]).as_deref(), Some("/p/powerhouse"));
        assert_eq!(folder(&[], &[]), None);
    }
}
