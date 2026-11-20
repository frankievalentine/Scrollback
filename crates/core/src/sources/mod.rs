//! One module per harness. File-based harnesses expose `files()` and
//! `parse(path)`, so the indexer can skip files that have not changed.
//! Database-backed harnesses expose `scan()`.

pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod opencode;
pub mod pi;
pub mod t3code;

use crate::model::Harness;
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
