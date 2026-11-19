//! One module per harness. File-based harnesses expose `files()` and
//! `parse(path)`, so the indexer can skip files that have not changed.
//! Database-backed harnesses expose `scan()`.

pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod opencode;
pub mod pi;
pub mod t3code;

use crate::model::Harness;
use std::path::{Path, PathBuf};

/// Where a harness keeps its sessions.
pub fn location(harness: Harness) -> PathBuf {
    match harness {
        Harness::ClaudeCode => claude_code::root(),
        Harness::Codex => codex::root().join("sessions"),
        Harness::OpenCode => opencode::db_path(),
        Harness::Pi => pi::root(),
        Harness::Cursor => cursor::db_path(),
    }
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
