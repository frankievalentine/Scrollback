//! fx, Vercel's coding agent. Sessions live under `~/.fx/sessions` (or
//! `$FX_HOME/sessions`):
//!
//! - Current: `v2/<id>/log.jsonl`, an append-only log. Each line is JSON with
//!   a `kind`: `session_created` (id, workspace, role, parent), `set` (title,
//!   prefs with the model, usage), `item` (a piece of a turn: `user`,
//!   `assistant`, `tool_call`, …, its payload in `data`), and
//!   `turn_interrupted`. A payload too large to keep inline is
//!   `{"$blob": "<hash>"}`, stored in `blobs/<hash>` beside the log.
//! - Older: `<id>/session.json` with `events.jsonl`, each line holding an
//!   `event` whose single key is the piece's kind. fx converts these when
//!   they are opened, so both are read.
//!
//! A sub-agent's session has `role: "child"` (or `subagent_child` in the
//! older form) and names its parent.

use crate::model::{Harness, Session};
use crate::sources::{any_ms, json_lines, tool_target, typed_prompt, walk, Found};
use crate::transcript::Event;
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    super::location(Harness::Fx)
}

/// The log of every session, current and older form.
pub fn files() -> Vec<PathBuf> {
    walk(&root(), &|path: &Path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let in_trash = path.components().any(|c| c.as_os_str() == ".trash");
        !in_trash && (name == "log.jsonl" || name == "events.jsonl")
    })
}

pub fn parse(path: &Path) -> Result<Option<Session>> {
    let found = read(path)?;
    if found.events.is_empty() {
        return Ok(None);
    }
    let id = session_id(path);
    Ok(Some(found.session(Harness::Fx, &id, &path.to_string_lossy())))
}

pub fn events(session: &Session) -> Result<Vec<Event>> {
    Ok(read(Path::new(&session.source))?.into_events())
}

fn session_id(path: &Path) -> String {
    path.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string()
}

fn read(path: &Path) -> Result<Found> {
    let mut found = Found::default();
    let current = path.file_name().is_some_and(|name| name == "log.jsonl");
    if !current {
        // The older form keeps the session's details beside its events.
        if let Ok(text) = std::fs::read_to_string(path.with_file_name("session.json")) {
            if let Ok(meta) = serde_json::from_str::<Value>(&text) {
                found.cwd = meta["workspace_root"].as_str().map(str::to_string);
                found.title = meta["title"].as_str().map(str::to_string);
                found.model = meta["model"].as_str().map(str::to_string);
                found.started_ms = meta["created_at_ms"].as_i64();
                found.updated_ms = meta["updated_at_ms"].as_i64();
                if meta["subagent_child"].as_bool() == Some(true) {
                    found.parent = Some(meta["parent_id"].as_str().unwrap_or("fx").to_string());
                }
            }
        }
    }
    let folder = path.parent().unwrap_or(Path::new("."));
    for line in json_lines(path)? {
        if current {
            let ts = line["ts"].as_i64().unwrap_or(0);
            match line["kind"].as_str() {
                Some("session_created") => {
                    found.started_ms = Some(ts);
                    found.cwd = line["workspace"].as_str().map(str::to_string);
                    if line["role"].as_str() == Some("child") {
                        found.parent = Some(line["parent"].as_str().unwrap_or("fx").to_string());
                    }
                }
                Some("set") => match line["key"].as_str() {
                    Some("title") => {
                        found.title = match &line["value"] {
                            Value::String(text) => Some(text.clone()),
                            other => other["title"].as_str().map(str::to_string),
                        };
                    }
                    Some("prefs") => {
                        if let Some(model) = line["value"]["model"].as_str() {
                            found.model = Some(model.to_string());
                        }
                    }
                    Some("usage") => usage(&line["value"], &mut found),
                    _ => {}
                },
                Some("item") => {
                    let data = resolve(&line["data"], folder);
                    piece(line["type"].as_str().unwrap_or(""), &data, ts, &mut found);
                }
                Some("turn_interrupted") => {
                    let reason = line["reason"].as_str().unwrap_or("stopped");
                    found.push(ts, Event::Note(format!("the turn was interrupted ({reason})")));
                }
                _ => {}
            }
            if ts > 0 {
                found.updated_ms = Some(found.updated_ms.unwrap_or(0).max(ts));
            }
        } else {
            let ts = line["timestamp_ms"].as_i64().or_else(|| any_ms(&line["timestamp"])).unwrap_or(0);
            if let Some(event) = line["event"].as_object() {
                for (kind, data) in event {
                    piece(kind, data, ts, &mut found);
                }
            }
        }
    }
    Ok(found)
}

/// A payload, read from its blob file when it was too large to keep inline.
fn resolve(data: &Value, folder: &Path) -> Value {
    let Some(hash) = data["$blob"].as_str() else { return data.clone() };
    let safe = !hash.contains(['/', '\\']) && !hash.contains("..");
    safe.then(|| std::fs::read_to_string(folder.join("blobs").join(hash)).ok())
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// One piece of a turn.
fn piece(kind: &str, data: &Value, ts: i64, found: &mut Found) {
    match kind {
        "user" => {
            if let Some(text) = data["text"].as_str().and_then(typed_prompt) {
                found.push(ts, Event::Prompt(text.to_string()));
            }
        }
        "assistant" => {
            if let Some(text) = data["text"].as_str().filter(|t| !t.trim().is_empty()) {
                found.push(ts, Event::Reply(text.to_string()));
            }
        }
        "tool_call" => {
            let name = data["tool_name"].as_str().unwrap_or("tool").to_string();
            let target = tool_target(&data["arguments_json"]).or_else(|| tool_target(&data["arguments"]));
            found.push(ts, Event::Tool { name, target });
        }
        "interruption" => found.push(ts, Event::Note("the turn was interrupted".to_string())),
        "turn_completed" | "turn_end" => usage(&data["turn_summary"]["token_progress"], found),
        _ => {}
    }
}

fn usage(value: &Value, found: &mut Found) {
    let number = |keys: &[&str]| keys.iter().find_map(|key| value[*key].as_u64()).unwrap_or(0);
    found.tokens_in += number(&["input_tokens", "inputTokens", "input"]);
    found.tokens_out += number(&["output_tokens", "outputTokens", "output"]);
    found.tokens_cached += number(&["cache_read_tokens", "cachedReadTokens", "cache_read"]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scrollback-fx-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_the_current_log() {
        let folder = dir("v2").join("ckpGrV2Zj6ZA");
        std::fs::create_dir_all(folder.join("blobs")).unwrap();
        std::fs::write(folder.join("blobs/abc"), r#"{"text":"A long reply kept apart."}"#).unwrap();
        let log = folder.join("log.jsonl");
        std::fs::write(
            &log,
            [
                r#"{"v":1,"seq":1,"ts":1727400000000,"kind":"session_created","id":"ckpGrV2Zj6ZA","workspace":"/p/app","role":"root","host":"app","crc":"0"}"#,
                r#"{"v":1,"seq":2,"ts":1727400001000,"kind":"set","key":"prefs","value":{"model":"zai/glm-5.2"},"crc":"0"}"#,
                r#"{"v":1,"seq":3,"ts":1727400002000,"kind":"item","turn":1,"type":"user","data":{"text":"Add a retry"},"crc":"0"}"#,
                r#"{"v":1,"seq":4,"ts":1727400003000,"kind":"item","turn":1,"type":"tool_call","data":{"call_id":"c1","tool_name":"shell","arguments_json":"{\"action\":\"run\",\"command\":\"cargo test\"}"},"crc":"0"}"#,
                r#"{"v":1,"seq":5,"ts":1727400004000,"kind":"item","turn":1,"type":"assistant","data":{"$blob":"abc"},"crc":"0"}"#,
                r#"{"v":1,"seq":6,"ts":1727400005000,"kind":"set","key":"title","value":"Retry the fetch","crc":"0"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse(&log).unwrap().unwrap();
        assert_eq!(session.id, "ckpGrV2Zj6ZA");
        assert_eq!(session.title, "Retry the fetch");
        assert_eq!(session.cwd, "/p/app");
        assert_eq!(session.model.as_deref(), Some("zai/glm-5.2"));
        assert_eq!((session.user_turns, session.assistant_turns, session.tool_calls), (1, 1, 1));
        let events = events(&session).unwrap();
        assert!(matches!(&events[1], Event::Tool { name, target } if name == "shell" && target.as_deref() == Some("cargo test")));
        assert!(matches!(&events[2], Event::Reply(text) if text == "A long reply kept apart."));
    }

    #[test]
    fn reads_the_older_form() {
        let folder = dir("v1").join("abc123");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("session.json"),
            r#"{"schema_version":4,"id":"abc123","workspace_root":"/p/old","created_at_ms":1727400000000,"updated_at_ms":1727400060000,"model":"spacexai/grok-4.7","title":"CONV_FIRST","subagent_child":false}"#,
        )
        .unwrap();
        let events_file = folder.join("events.jsonl");
        std::fs::write(
            &events_file,
            r#"{"schema_version":3,"seq":1,"timestamp_ms":1727400010000,"event":{"user":{"text":"CONV_FIRST","images":[]}}}
{"schema_version":3,"seq":2,"timestamp_ms":1727400020000,"event":{"assistant":{"text":"Done."}}}"#,
        )
        .unwrap();
        let session = parse(&events_file).unwrap().unwrap();
        assert_eq!(session.title, "CONV_FIRST");
        assert_eq!(session.cwd, "/p/old");
        assert_eq!(session.updated_ms, 1727400060000);
        assert!(session.parent.is_none());
    }
}
