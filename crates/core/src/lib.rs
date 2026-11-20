//! Reads coding-agent session history from every harness on this machine and
//! keeps a small local index of it. Nothing here writes to a harness's own
//! files, and nothing leaves the machine.

pub mod activity;
pub mod config;
pub mod model;
pub mod search;
pub mod sources;
pub mod store;
pub mod summarize;
pub mod transcript;

pub use activity::Activity;
pub use config::{Config, QUIET_MINUTES, SUMMARY_MODELS};
pub use model::{Harness, Origin, Outcome, Session, Summary};
pub use store::{Digest, MessageHit, Store};

use std::path::PathBuf;

/// A folder named by an environment variable, when it is set to something.
pub fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// The user's home directory.
pub fn home() -> PathBuf {
    // Windows names it `USERPROFILE`; a Unix-style shell there may also set
    // `HOME`, sometimes to a folder of its own.
    let names: [&str; 2] = if cfg!(windows) { ["USERPROFILE", "HOME"] } else { ["HOME", "USERPROFILE"] };
    names.into_iter().find_map(env_dir).unwrap_or_default()
}

/// Where desktop apps keep each user's data: `~/Library/Application Support`
/// on macOS, `%APPDATA%` on Windows, `~/.config` on Linux. Cursor and T3
/// Code are found under here.
pub fn app_support() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(windows) {
        env_dir("APPDATA").unwrap_or_else(|| home().join("AppData").join("Roaming"))
    } else {
        env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
    }
}

/// Where command-line tools keep their data when they follow the XDG
/// layout, which the ones read here do on every system, Windows included.
pub fn xdg_data() -> PathBuf {
    env_dir("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local").join("share"))
}

/// Where Scrollback keeps its own index and settings. `SCROLLBACK_DATA_DIR`
/// points it somewhere else, so a change can be tried against a copy.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = env_dir("SCROLLBACK_DATA_DIR") {
        return dir;
    }
    let parent = if cfg!(target_os = "macos") {
        app_support()
    } else if cfg!(windows) {
        // The index is rebuilt from what is on this machine, so it does not
        // belong in the part of the profile that follows the user around.
        env_dir("LOCALAPPDATA").unwrap_or_else(|| home().join("AppData").join("Local"))
    } else {
        xdg_data()
    };
    parent.join("scrollback")
}

/// The parts of a path between its separators. Sessions carry paths as the
/// harness wrote them, so a backslash separates only in a path that starts
/// the way Windows paths do.
pub fn path_parts(path: &str) -> impl Iterator<Item = &str> {
    let bytes = path.as_bytes();
    let windows = path.starts_with("\\\\")
        || (bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'));
    path.split(move |c| c == '/' || (windows && c == '\\'))
        .filter(|part| !part.is_empty())
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
