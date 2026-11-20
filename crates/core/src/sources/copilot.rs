//! GitHub Copilot's command-line agent. Each session is a folder in
//! `~/.copilot/session-state/<id>/` (or under `$COPILOT_HOME`) with:
//!
//! - `events.jsonl`: the session as events, each `{type, data, timestamp}`:
//!   `session.start` (id, folder, branch), `user.message`,
//!   `assistant.message` (its text and the tools it asked for),
//!   `session.model_change`, and `session.usage_record` with token counts.
//! - `workspace.yaml`: a `name:` line with the session's title.
//!
//! Older releases wrote `session-state/<id>.jsonl` directly; those are read
//! the same way.

use crate::model::{Harness, Session};
use crate::sources::{any_ms, json_lines, tool_target, typed_prompt, walk, Found};
use crate::transcript::Event;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    super::location(Harness::Copilot)
}

pub fn files() -> Vec<PathBuf> {
    let state = root().join("session-state");
    walk(&state, &|path: &Path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let directly_inside = path.parent() == Some(state.as_path());
        name == "events.jsonl" || (directly_inside && name.ends_with(".jsonl"))
    })
}

pub fn parse(path: &Path) -> Result<Option<Session>> {
    let (id, found) = read(path)?;
    if found.events.is_empty() {
        return Ok(None);
    }
    Ok(Some(found.session(Harness::Copilot, &id, &path.to_string_lossy())))
}

pub fn events(session: &Session) -> Result<Vec<Event>> {
    Ok(read(Path::new(&session.source))?.1.into_events())
}

fn read(path: &Path) -> Result<(String, Found)> {
    let mut found = Found::default();
    let in_folder = path.file_name().is_some_and(|n| n == "events.jsonl");
    let mut id = match in_folder {
        true => path.parent().and_then(|d| d.file_name()),
        false => path.file_stem(),
    }
    .and_then(|n| n.to_str())
    .unwrap_or("")
    .to_string();
    if in_folder {
        found.title = std::fs::read_to_string(path.with_file_name("workspace.yaml"))
            .ok()
            .and_then(|yaml| {
                yaml.lines().find_map(|line| {
                    line.strip_prefix("name:").map(|name| name.trim().trim_matches(['"', '\'']).to_string())
                })
            });
    }
    for line in json_lines(path)? {
        let ts = any_ms(&line["timestamp"]).unwrap_or(0);
        let data = &line["data"];
        match line["type"].as_str() {
            Some("session.start") => {
                if let Some(session_id) = data["sessionId"].as_str() {
                    id = session_id.to_string();
                }
                found.started_ms = Some(ts);
                found.cwd = data["context"]["cwd"].as_str().map(str::to_string);
                found.branch = data["context"]["branch"].as_str().map(str::to_string);
            }
            Some("user.message") => {
                if let Some(text) = data["content"].as_str().and_then(typed_prompt) {
                    found.push(ts, Event::Prompt(text.to_string()));
                }
            }
            Some("assistant.message") => {
                if let Some(text) = data["content"].as_str().filter(|t| !t.trim().is_empty()) {
                    found.push(ts, Event::Reply(text.to_string()));
                }
                for request in data["toolRequests"].as_array().into_iter().flatten() {
                    let name = request["name"].as_str().unwrap_or("tool").to_string();
                    found.push(ts, Event::Tool { name, target: tool_target(&request["arguments"]) });
                }
            }
            Some("session.model_change") => {
                found.model = data["newModel"].as_str().map(str::to_string);
            }
            Some("session.usage_record") => {
                let usage = &data["usage"];
                found.tokens_in += usage["inputTokens"].as_u64().unwrap_or(0) + usage["cacheWriteTokens"].as_u64().unwrap_or(0);
                found.tokens_out += usage["outputTokens"].as_u64().unwrap_or(0);
                found.tokens_cached += usage["cacheReadTokens"].as_u64().unwrap_or(0);
                if found.model.is_none() {
                    found.model = usage["model"].as_str().map(str::to_string);
                }
            }
            _ => {}
        }
    }
    Ok((id, found))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_session_folder() {
        let folder = std::env::temp_dir()
            .join(format!("scrollback-copilot-{}", std::process::id()))
            .join("session-state")
            .join("3f2b1c4e");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("workspace.yaml"), "name: Fix the config loader\n").unwrap();
        let events_file = folder.join("events.jsonl");
        std::fs::write(
            &events_file,
            [
                r#"{"type":"session.start","data":{"sessionId":"3f2b1c4e","context":{"cwd":"/p/app","branch":"main"}},"timestamp":"2025-01-15T10:00:00Z"}"#,
                r#"{"type":"user.message","data":{"content":"Why is config.json ignored?"},"timestamp":"2025-01-15T10:00:01Z"}"#,
                r#"{"type":"assistant.message","data":{"content":"","toolRequests":[{"toolCallId":"tc-1","name":"view","arguments":"{\"path\":\"config.json\"}"}]},"timestamp":"2025-01-15T10:00:02Z"}"#,
                r#"{"type":"assistant.message","data":{"content":"The loader reads config.yaml first."},"timestamp":"2025-01-15T10:00:03Z"}"#,
                r#"{"type":"session.usage_record","data":{"usage":{"model":"gpt-5","inputTokens":900,"outputTokens":80,"cacheReadTokens":300}},"timestamp":"2025-01-15T10:00:03Z"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse(&events_file).unwrap().unwrap();
        assert_eq!(session.id, "3f2b1c4e");
        assert_eq!(session.title, "Fix the config loader");
        assert_eq!(session.cwd, "/p/app");
        assert_eq!(session.branch.as_deref(), Some("main"));
        assert_eq!(session.model.as_deref(), Some("gpt-5"));
        assert_eq!((session.user_turns, session.assistant_turns, session.tool_calls), (1, 1, 1));
    }
}
