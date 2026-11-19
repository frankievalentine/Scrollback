//! Codex: one `rollout-*.jsonl` per session under `~/.codex/sessions/YYYY/MM/DD/`
//! (and `~/.codex/archived_sessions/`). Thread names, where the user or the
//! app set one, are in `~/.codex/session_index.jsonl`.

use crate::model::{Harness, Session};
use crate::sources::{typed_prompt, walk};
use crate::{home, parse_ts, squash};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    home().join(".codex")
}

pub fn files() -> Vec<PathBuf> {
    let is_rollout = |p: &Path| {
        p.extension().is_some_and(|e| e == "jsonl")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
    };
    let mut found = walk(&root().join("sessions"), &is_rollout);
    found.extend(walk(&root().join("archived_sessions"), &is_rollout));
    found
}

/// Session id to thread name, from Codex's own index.
pub fn thread_names() -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        thread_name: Option<String>,
    }
    let mut names = HashMap::new();
    let Ok(file) = std::fs::File::open(root().join("session_index.jsonl")) else {
        return names;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(entry) = serde_json::from_str::<Entry>(&line) {
            if let Some(name) = entry.thread_name.filter(|n| !n.trim().is_empty()) {
                names.insert(entry.id, name);
            }
        }
    }
    names
}

#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

// Field types differ between payload kinds, so anything not needed as a
// specific type is read loosely.
#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<Value>,
    cwd: Option<Value>,
    model: Option<Value>,
    message: Option<Value>,
    git: Option<Value>,
    info: Option<Info>,
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<Totals>,
}

#[derive(Deserialize)]
struct Totals {
    /// Everything sent to the model, cached or not.
    #[serde(default)]
    input_tokens: u64,
    /// The part of `input_tokens` that was read from the cache.
    #[serde(default)]
    cached_input_tokens: u64,
    /// Includes reasoning.
    #[serde(default)]
    output_tokens: u64,
}

/// Parse one rollout file. Returns `None` for files with no conversation.
pub fn parse(path: &Path, names: &HashMap<String, String>) -> Result<Option<Session>> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    // rollout-<timestamp>-<uuid>: the id is the trailing 36 characters.
    let fallback_id = stem
        .char_indices()
        .rev()
        .nth(35)
        .map(|(i, _)| &stem[i..])
        .unwrap_or(stem);
    let mut session = Session::new(Harness::Codex, fallback_id, path.to_string_lossy());

    let reader = BufReader::new(std::fs::File::open(path)?);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let Ok(parsed) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        if let Some(ts) = parsed.timestamp.as_deref().and_then(parse_ts) {
            session.touch(ts);
        }
        let Some(payload) = parsed.payload else {
            continue;
        };
        let text = |v: &Option<Value>| v.as_ref().and_then(Value::as_str).map(str::to_string);

        match (parsed.kind.as_deref(), payload.kind.as_deref()) {
            (Some("session_meta"), _) => {
                if let Some(id) = text(&payload.id) {
                    session.id = id;
                }
                if let Some(cwd) = text(&payload.cwd) {
                    session.cwd = cwd;
                }
                session.branch = payload
                    .git
                    .as_ref()
                    .and_then(|g| g.get("branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            (Some("turn_context"), _) => {
                if let Some(model) = text(&payload.model) {
                    session.model = Some(model);
                }
                if session.cwd.is_empty() {
                    if let Some(cwd) = text(&payload.cwd) {
                        session.cwd = cwd;
                    }
                }
            }
            (Some("event_msg"), Some("user_message")) => {
                let message = text(&payload.message);
                let Some(prompt) = message.as_deref().and_then(typed_prompt) else {
                    continue;
                };
                session.user_turns += 1;
                if session.first_prompt.is_empty() {
                    session.first_prompt = squash(prompt, 600);
                }
            }
            (Some("event_msg"), Some("agent_message")) => session.assistant_turns += 1,
            (Some("event_msg"), Some("token_count")) => {
                // A running total, so the last one seen is the session total.
                if let Some(totals) = payload.info.and_then(|i| i.total_token_usage) {
                    session.tokens_in = totals.input_tokens.saturating_sub(totals.cached_input_tokens);
                    session.tokens_out = totals.output_tokens;
                    session.tokens_cached = totals.cached_input_tokens;
                }
            }
            (
                Some("response_item"),
                Some("function_call" | "custom_tool_call" | "local_shell_call"),
            ) => session.tool_calls += 1,
            _ => {}
        }
    }

    if session.user_turns == 0 && session.assistant_turns == 0 {
        return Ok(None);
    }
    session.title = names
        .get(&session.id)
        .cloned()
        .unwrap_or_else(|| squash(&session.first_prompt, 90));
    Ok(Some(session))
}
