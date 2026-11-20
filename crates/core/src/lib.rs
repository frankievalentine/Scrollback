//! Reads coding-agent session history from every harness on this machine and
//! keeps a small local index of it. Nothing here writes to a harness's own
//! files, and nothing leaves the machine.

pub mod activity;
pub mod config;
pub mod export;
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
    // Named like the app on macOS and Windows. Linux keeps its data folders
    // in lower case, as the tools beside it in ~/.local/share do.
    let name = if cfg!(target_os = "linux") { "scrollback" } else { "Scrollback" };
    static SETTLED: std::sync::Once = std::sync::Once::new();
    SETTLED.call_once(|| settle_name(&parent, name));
    parent.join(name)
}

/// Bring a folder left under an older spelling of `name` (such as
/// `scrollback` before the app was named `Scrollback`) to the current one.
/// The disks of macOS and Windows usually ignore case, so the folder is
/// moved through a temporary name. A folder already under the current
/// name is left as it is.
fn settle_name(parent: &std::path::Path, name: &str) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    let names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|entry| entry.eq_ignore_ascii_case(name))
        .collect();
    if names.iter().any(|entry| entry == name) {
        return;
    }
    let Some(old) = names.first() else { return };
    let moving = parent.join(format!(".{name}-moving"));
    if std::fs::rename(parent.join(old), &moving).is_ok() && std::fs::rename(&moving, parent.join(name)).is_err() {
        // Put it back rather than leave it under the temporary name.
        let _ = std::fs::rename(&moving, parent.join(old));
    }
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

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn an_older_spelling_is_moved_to_the_current_one() {
        let parent = std::env::temp_dir().join(format!("scrollback-name-{}", std::process::id()));
        std::fs::create_dir_all(parent.join("scrollback")).unwrap();
        std::fs::write(parent.join("scrollback").join("index.db"), "x").unwrap();
        settle_name(&parent, "Scrollback");
        let listed: Vec<String> = std::fs::read_dir(&parent)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert_eq!(listed, vec!["Scrollback".to_string()]);
        assert!(parent.join("Scrollback").join("index.db").is_file());
        // Already right: nothing moves.
        settle_name(&parent, "Scrollback");
        assert!(parent.join("Scrollback").join("index.db").is_file());
        std::fs::remove_dir_all(&parent).ok();
    }
}
