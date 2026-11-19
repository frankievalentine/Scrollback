//! Reads coding-agent session history from every harness on this machine and
//! keeps a small local index of it. Nothing here writes to a harness's own
//! files, and nothing leaves the machine.

pub mod model;
pub mod search;
pub mod sources;
pub mod store;

pub use model::{Harness, Session};
pub use store::Store;

use std::path::PathBuf;

/// The user's home directory.
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where scrollback keeps its own index.
pub fn data_dir() -> PathBuf {
    home().join("Library/Application Support/scrollback")
}

/// Collapse whitespace and cut `text` to at most `max` characters.
pub fn squash(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(max.min(text.len()));
    let mut count = 0;
    let mut last_space = true;
    for ch in text.chars() {
        let ch = if ch.is_whitespace() { ' ' } else { ch };
        if ch == ' ' && last_space {
            continue;
        }
        last_space = ch == ' ';
        if count == max {
            out.push('…');
            break;
        }
        out.push(ch);
        count += 1;
    }
    out.trim_end().to_string()
}

/// Parse an RFC 3339 timestamp into Unix milliseconds.
pub fn parse_ts(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.timestamp_millis())
}
