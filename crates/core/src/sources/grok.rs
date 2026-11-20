//! Grok Build, xAI's coding agent. Sessions live under `~/.grok/sessions`
//! (or `$GROK_HOME/sessions`), one folder per session inside a folder named
//! for the working directory: `<encoded-cwd>/<session-id>/`.
//!
//! - `summary.json`: the session's details: `info.id` and `info.cwd`, a
//!   generated title, times, the model, and for a sub-agent or fork its
//!   kind and parent.
//! - `updates.jsonl`: the conversation, as Agent Client Protocol updates.
//!   Messages arrive in chunks (`user_message_chunk`, `agent_message_chunk`)
//!   that are joined; tool calls are `tool_call` updates; each finished turn
//!   reports its token use.

use crate::model::{Harness, Session};
use crate::sources::{any_ms, json_lines, tool_target, typed_prompt, walk, Found};
use crate::transcript::Event;
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    super::location(Harness::Grok)
}

pub fn files() -> Vec<PathBuf> {
    walk(&root(), &|path: &Path| {
        // A sub-agent's own folder sits inside its parent's; it has its own
        // session folder elsewhere, which is the one read.
        let nested = path.components().any(|c| c.as_os_str() == "subagents");
        !nested && path.file_name().is_some_and(|name| name == "updates.jsonl")
    })
}

pub fn parse(path: &Path) -> Result<Option<Session>> {
    let found = read(path)?;
    if found.events.is_empty() {
        return Ok(None);
    }
    let summary = summary(path);
    let id = summary["info"]["id"]
        .as_str()
        .map(str::to_string)
        .or_else(|| path.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()).map(str::to_string))
        .unwrap_or_default();
    Ok(Some(found.session(Harness::Grok, &id, &path.to_string_lossy())))
}

pub fn events(session: &Session) -> Result<Vec<Event>> {
    Ok(read(Path::new(&session.source))?.into_events())
}

fn summary(updates: &Path) -> Value {
    std::fs::read_to_string(updates.with_file_name("summary.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The working directory, from the summary or the folder's name.
fn folder_of(updates: &Path, summary: &Value) -> Option<String> {
    if let Some(cwd) = summary["info"]["cwd"].as_str() {
        return Some(cwd.to_string());
    }
    let encoded = updates.parent()?.parent()?;
    if let Ok(real) = std::fs::read_to_string(encoded.join(".cwd")) {
        return Some(real.trim().to_string());
    }
    let name = encoded.file_name()?.to_str()?;
    super::file_uri_path(&format!("file://{name}"))
}

fn read(path: &Path) -> Result<Found> {
    let summary = summary(path);
    let mut found = Found {
        cwd: folder_of(path, &summary),
        title: summary["generated_title"]
            .as_str()
            .or_else(|| summary["session_summary"].as_str())
            .map(str::to_string),
        model: summary["current_model_id"].as_str().map(str::to_string),
        branch: summary["head_branch"].as_str().map(str::to_string),
        started_ms: any_ms(&summary["created_at"]),
        updated_ms: any_ms(&summary["last_active_at"]).or_else(|| any_ms(&summary["updated_at"])),
        ..Found::default()
    };
    let kind = summary["session_kind"].as_str().unwrap_or("");
    if kind.starts_with("subagent") {
        found.parent = summary["parent_session_id"].as_str().map(str::to_string).or(Some("grok".to_string()));
    }

    // Chunks of one message, joined until a different kind of update.
    let mut pending: Option<(bool, i64, String)> = None;
    let flush = |pending: &mut Option<(bool, i64, String)>, found: &mut Found| {
        if let Some((from_user, ts, text)) = pending.take() {
            if from_user {
                if let Some(typed) = typed_prompt(&text) {
                    found.push(ts, Event::Prompt(typed.to_string()));
                }
            } else if !text.trim().is_empty() {
                found.push(ts, Event::Reply(text));
            }
        }
    };
    for line in json_lines(path)? {
        let ts = any_ms(&line["timestamp"]).unwrap_or(0);
        let update = &line["params"]["update"];
        let meta = &update["_meta"];
        if meta["hideFromScrollback"].as_bool() == Some(true) || meta["syntheticPrompt"].as_bool() == Some(true) {
            continue;
        }
        match update["sessionUpdate"].as_str() {
            Some(kind @ ("user_message_chunk" | "agent_message_chunk")) => {
                let from_user = kind == "user_message_chunk";
                let text = update["content"]["text"].as_str().unwrap_or("");
                match &mut pending {
                    Some((user, _, joined)) if *user == from_user => joined.push_str(text),
                    _ => {
                        flush(&mut pending, &mut found);
                        pending = Some((from_user, ts, text.to_string()));
                    }
                }
                if let Some(model) = meta["modelId"].as_str() {
                    found.model = Some(model.to_string());
                }
            }
            Some("agent_thought_chunk") => {}
            Some("tool_call") => {
                flush(&mut pending, &mut found);
                let name = meta["x.ai/tool"]["name"]
                    .as_str()
                    .or_else(|| update["title"].as_str())
                    .unwrap_or("tool")
                    .to_string();
                found.push(ts, Event::Tool { name, target: tool_target(&update["rawInput"]) });
            }
            Some("turn_completed") => {
                flush(&mut pending, &mut found);
                let usage = &update["usage"];
                let input = usage["inputTokens"].as_u64().unwrap_or(0);
                let cached = usage["cachedReadTokens"].as_u64().unwrap_or(0);
                found.tokens_in += input.saturating_sub(cached);
                found.tokens_cached += cached;
                found.tokens_out += usage["outputTokens"].as_u64().unwrap_or(0);
                if let Some(reason) = update["stop_reason"].as_str().filter(|r| *r != "end_turn") {
                    found.push(ts, Event::Note(format!("the turn ended early ({reason})")));
                }
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut found);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_chunks_into_messages() {
        let folder = std::env::temp_dir()
            .join(format!("scrollback-grok-{}", std::process::id()))
            .join("%2Fworkspace%2Ffetchkit")
            .join("019f0000-0000-7000-8000-000000000020");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("summary.json"),
            r#"{"info":{"id":"019f0000-0000-7000-8000-000000000020"},"generated_title":"Retry for fetch","current_model_id":"grok-4.7","created_at":"2026-07-20T10:00:00Z"}"#,
        )
        .unwrap();
        let updates = folder.join("updates.jsonl");
        std::fs::write(
            &updates,
            [
                r#"{"timestamp":1784800001,"method":"session/update","params":{"sessionId":"x","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"Add a retry "},"_meta":{"modelId":"grok-4.7"}}}}"#,
                r#"{"timestamp":1784800001,"method":"session/update","params":{"sessionId":"x","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"to fetch."}}}}"#,
                r#"{"timestamp":1784800002,"method":"session/update","params":{"sessionId":"x","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I'll read it first."}}}}"#,
                r#"{"timestamp":1784800003,"method":"session/update","params":{"sessionId":"x","update":{"sessionUpdate":"tool_call","toolCallId":"c1","title":"read_file","rawInput":{"target_file":"/workspace/fetchkit/fetch.go"},"_meta":{"x.ai/tool":{"name":"read_file"}}}}}"#,
                r#"{"timestamp":1784800004,"method":"_x.ai/session/update","params":{"sessionId":"x","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn","usage":{"inputTokens":48210,"outputTokens":1630,"cachedReadTokens":40960}}}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse(&updates).unwrap().unwrap();
        assert_eq!(session.title, "Retry for fetch");
        assert_eq!(session.cwd, "/workspace/fetchkit");
        assert_eq!(session.first_prompt, "Add a retry to fetch.");
        assert_eq!((session.tokens_in, session.tokens_cached, session.tokens_out), (7250, 40960, 1630));
        let events = events(&session).unwrap();
        assert!(matches!(&events[2], Event::Tool { name, target } if name == "read_file" && target.as_deref() == Some("/workspace/fetchkit/fetch.go")));
    }
}
