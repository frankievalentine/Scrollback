//! Amp, from Sourcegraph. Amp keeps its threads on its own servers; the
//! command-line tool and editor extensions also keep a copy of each thread
//! they work on, one JSON file per thread, in
//! `~/.local/share/amp/threads/T-<id>.json` (or under `$XDG_DATA_HOME`).
//! Threads only ever used on the web are not on this computer.
//!
//! A thread is `{id, created, title, env: {initial: {trees: [{uri}]}},
//! messages: [...]}`. Messages hold content blocks in Anthropic's shape:
//! `text`, `thinking`, `tool_use` (name and input), and `tool_result` in the
//! user's turn. An assistant message may carry `usage` with token counts.

use crate::model::{Harness, Session};
use crate::sources::{any_ms, file_uri_path, tool_target, typed_prompt, walk, Found};
use crate::transcript::Event;
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    super::location(Harness::Amp)
}

pub fn files() -> Vec<PathBuf> {
    walk(&root().join("threads"), &|path: &Path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        name.starts_with("T-") && name.ends_with(".json")
    })
}

pub fn parse(path: &Path) -> Result<Option<Session>> {
    let thread: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let found = read(&thread);
    if found.events.is_empty() {
        return Ok(None);
    }
    let id = thread["id"]
        .as_str()
        .map(str::to_string)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .unwrap_or_default();
    Ok(Some(found.session(Harness::Amp, &id, &path.to_string_lossy())))
}

pub fn events(session: &Session) -> Result<Vec<Event>> {
    let thread: Value = serde_json::from_str(&std::fs::read_to_string(&session.source)?)?;
    Ok(read(&thread).into_events())
}

fn read(thread: &Value) -> Found {
    let tree = &thread["env"]["initial"]["trees"][0];
    let mut found = Found {
        title: thread["title"].as_str().map(str::to_string),
        started_ms: thread["created"].as_i64(),
        cwd: tree["uri"].as_str().and_then(file_uri_path),
        branch: tree["repository"]["ref"].as_str().map(|r| r.trim_start_matches("refs/heads/").to_string()),
        ..Found::default()
    };
    let mut last = found.started_ms.unwrap_or(0);
    for trace in thread["meta"]["traces"].as_array().into_iter().flatten() {
        if let Some(end) = any_ms(&trace["endTime"]) {
            last = last.max(end);
        }
    }
    for message in thread["messages"].as_array().into_iter().flatten() {
        let ts = any_ms(&message["meta"]["sentAt"])
            .or_else(|| any_ms(&message["usage"]["timestamp"]))
            .unwrap_or(0);
        last = last.max(ts);
        let user = message["role"].as_str() == Some("user");
        let blocks: Vec<Value> = match &message["content"] {
            Value::String(text) => vec![serde_json::json!({"type": "text", "text": text})],
            Value::Array(blocks) => blocks.clone(),
            _ => Vec::new(),
        };
        for block in &blocks {
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("");
                    if user {
                        if let Some(typed) = typed_prompt(text) {
                            found.push(ts, Event::Prompt(typed.to_string()));
                        }
                    } else if !text.trim().is_empty() {
                        found.push(ts, Event::Reply(text.to_string()));
                    }
                }
                Some("tool_use") => {
                    let name = block["name"].as_str().unwrap_or("tool").to_string();
                    found.push(ts, Event::Tool { name, target: tool_target(&block["input"]) });
                }
                _ => {}
            }
        }
        let usage = &message["usage"];
        found.tokens_in += usage["inputTokens"].as_u64().unwrap_or(0) + usage["cacheCreationInputTokens"].as_u64().unwrap_or(0);
        found.tokens_out += usage["outputTokens"].as_u64().unwrap_or(0);
        found.tokens_cached += usage["cacheReadInputTokens"].as_u64().unwrap_or(0);
        if let Some(model) = usage["model"].as_str() {
            found.model = Some(model.to_string());
        }
    }
    found.updated_ms = Some(last);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_thread() {
        let thread: Value = serde_json::from_str(
            r#"{"v":1,"id":"T-019ca26f","created":1766525900000,"title":"Read main.go",
            "env":{"initial":{"trees":[{"displayName":"app","uri":"file:///Users/x/app","repository":{"ref":"refs/heads/main"}}]}},
            "messages":[
              {"role":"user","messageId":0,"content":[{"type":"text","text":"Read main.go and explain it"}],"meta":{"sentAt":1766525916428}},
              {"role":"assistant","content":[{"type":"thinking","thinking":"…"},{"type":"text","text":"Reading it now."},
                 {"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"/Users/x/app/main.go"}}],
               "usage":{"model":"claude-sonnet-5-5","inputTokens":10,"outputTokens":20,"cacheCreationInputTokens":5,"cacheReadInputTokens":100}},
              {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"package main"}]}]}"#,
        )
        .unwrap();
        let found = read(&thread);
        let session = found.session(Harness::Amp, "T-019ca26f", "t.json");
        assert_eq!(session.title, "Read main.go");
        assert_eq!(session.cwd, "/Users/x/app");
        assert_eq!(session.branch.as_deref(), Some("main"));
        assert_eq!((session.tokens_in, session.tokens_out, session.tokens_cached), (15, 20, 100));
        assert_eq!((session.user_turns, session.assistant_turns, session.tool_calls), (1, 1, 1));
    }
}
