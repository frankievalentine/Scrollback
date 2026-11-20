//! GitHub Copilot Chat in Visual Studio Code. Each chat is a file in VS
//! Code's user folder (`~/Library/Application Support/Code/User` on macOS,
//! `~/.config/Code/User` on Linux, `%APPDATA%\Code\User` on Windows, and
//! `Code - Insiders` beside it):
//!
//! - `workspaceStorage/<hash>/chatSessions/<id>.json` or `.jsonl`, with the
//!   folder that workspace opened in `workspaceStorage/<hash>/workspace.json`.
//! - `globalStorage/emptyWindowChatSessions/<id>.json(l)` for chats in a
//!   window with no folder.
//!
//! A `.json` file is the chat whole: `{sessionId, creationDate,
//! lastMessageDate, customTitle, requests: [{message: {text}, response:
//! [...], modelId, timestamp, result: {metadata: {promptTokens,
//! outputTokens}}}]}`. A `.jsonl` file is a log of changes to that same
//! object (`{kind, k, v, i}`: 0 sets the whole, 1 sets the value at path
//! `k`, 2 adds to the list at `k`, 3 deletes at `k`), replayed here. When
//! both exist, the log is newer.

use crate::model::{Harness, Session};
use crate::sources::{file_uri_path, tool_target, typed_prompt, walk, Found};
use crate::transcript::Event;
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// VS Code's user folders, stable and Insiders, that exist.
pub fn roots() -> Vec<PathBuf> {
    let base = super::location(Harness::CopilotChat);
    let mut roots = vec![base.clone()];
    // `…/Code/User` → `…/Code - Insiders/User`.
    if let (Some(app), Some(user)) = (base.parent(), base.file_name()) {
        if let (Some(support), Some(name)) = (app.parent(), app.file_name().and_then(|n| n.to_str())) {
            roots.push(support.join(format!("{name} - Insiders")).join(user));
        }
    }
    roots.into_iter().filter(|root| root.is_dir()).collect()
}

pub fn files() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots() {
        let chats = |path: &Path| {
            let folder = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("");
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            matches!(folder, "chatSessions" | "emptyWindowChatSessions")
                && (ext == "jsonl" || (ext == "json" && !path.with_extension("jsonl").exists()))
        };
        found.extend(walk(&root.join("workspaceStorage"), &chats));
        found.extend(walk(&root.join("globalStorage").join("emptyWindowChatSessions"), &chats));
    }
    found
}

pub fn parse(path: &Path) -> Result<Option<Session>> {
    let chat = load(path)?;
    let found = read(path, &chat);
    if found.events.is_empty() {
        return Ok(None);
    }
    let id = chat["sessionId"]
        .as_str()
        .map(str::to_string)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .unwrap_or_default();
    Ok(Some(found.session(Harness::CopilotChat, &id, &path.to_string_lossy())))
}

pub fn events(session: &Session) -> Result<Vec<Event>> {
    let path = Path::new(&session.source);
    Ok(read(path, &load(path)?).into_events())
}

/// The chat as one object, replaying the change log when there is one.
fn load(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)?;
    if path.extension().is_some_and(|e| e == "json") {
        return Ok(serde_json::from_str(&text)?);
    }
    let mut state = Value::Null;
    for line in text.lines() {
        let Ok(change) = serde_json::from_str::<Value>(line) else { continue };
        apply(&mut state, &change);
    }
    Ok(state)
}

/// Apply one change from the log.
fn apply(state: &mut Value, change: &Value) {
    let keys: Vec<Value> = change["k"].as_array().cloned().unwrap_or_default();
    let value = change["v"].clone();
    match change["kind"].as_u64() {
        Some(0) => *state = value,
        Some(1) => {
            if let Some(slot) = slot(state, &keys, true) {
                *slot = value;
            }
        }
        Some(2) => {
            if let Some(slot) = slot(state, &keys, true) {
                if !slot.is_array() {
                    *slot = Value::Array(Vec::new());
                }
                let list = slot.as_array_mut().expect("made a list above");
                if let Some(at) = change["i"].as_u64() {
                    list.truncate(at as usize);
                }
                match value {
                    Value::Array(items) => list.extend(items),
                    other => list.push(other),
                }
            }
        }
        Some(3) => {
            if let Some((last, parents)) = keys.split_last() {
                if let Some(parent) = slot(state, parents, false) {
                    match (parent, last) {
                        (Value::Object(map), Value::String(key)) => {
                            map.remove(key);
                        }
                        (Value::Array(list), Value::Number(n)) => {
                            if let Some(i) = n.as_u64().map(|i| i as usize).filter(|i| *i < list.len()) {
                                list.remove(i);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

/// The value at a path of keys and list positions, made on the way when
/// `create` is set.
fn slot<'a>(mut at: &'a mut Value, keys: &[Value], create: bool) -> Option<&'a mut Value> {
    for key in keys {
        at = match key {
            Value::String(name) => {
                if !at.is_object() {
                    if !create {
                        return None;
                    }
                    *at = Value::Object(Default::default());
                }
                let map = at.as_object_mut()?;
                if !map.contains_key(name) && !create {
                    return None;
                }
                map.entry(name.clone()).or_insert(Value::Null)
            }
            Value::Number(n) => {
                let i = n.as_u64()? as usize;
                let list = at.as_array_mut()?;
                if i >= list.len() {
                    if !create {
                        return None;
                    }
                    list.resize(i + 1, Value::Null);
                }
                &mut list[i]
            }
            _ => return None,
        };
    }
    Some(at)
}

fn read(path: &Path, chat: &Value) -> Found {
    let mut found = Found {
        title: chat["customTitle"].as_str().map(str::to_string),
        started_ms: chat["creationDate"].as_i64(),
        updated_ms: chat["lastMessageDate"].as_i64(),
        cwd: workspace_folder(path),
        ..Found::default()
    };
    for request in chat["requests"].as_array().into_iter().flatten() {
        let ts = request["timestamp"].as_i64().unwrap_or(0);
        if let Some(text) = request["message"]["text"].as_str().and_then(typed_prompt) {
            found.push(ts, Event::Prompt(text.to_string()));
        }
        let mut reply = String::new();
        for item in request["response"].as_array().into_iter().flatten() {
            match item["kind"].as_str() {
                None | Some("markdownContent") => {
                    let text = item["value"].as_str().or_else(|| item["content"]["value"].as_str());
                    if let Some(text) = text {
                        reply.push_str(text);
                    }
                }
                Some("toolInvocationSerialized" | "toolInvocation") => {
                    if !reply.trim().is_empty() {
                        found.push(ts, Event::Reply(std::mem::take(&mut reply)));
                    }
                    let name = item["toolId"].as_str().unwrap_or("tool").to_string();
                    let target = item["toolSpecificData"]["commandLine"]["original"]
                        .as_str()
                        .map(|c| crate::squash(c, 120))
                        .or_else(|| tool_target(&item["toolSpecificData"]))
                        .or_else(|| {
                            item["invocationMessage"]["value"]
                                .as_str()
                                .or_else(|| item["invocationMessage"].as_str())
                                .map(|m| crate::squash(m, 120))
                        });
                    found.push(ts, Event::Tool { name, target });
                }
                _ => {}
            }
        }
        if !reply.trim().is_empty() {
            found.push(ts, Event::Reply(reply));
        }
        if let Some(model) = request["result"]["metadata"]["resolvedModel"].as_str().or_else(|| request["modelId"].as_str()) {
            found.model = Some(model.trim_start_matches("copilot/").to_string());
        }
        let metadata = &request["result"]["metadata"];
        found.tokens_in += metadata["promptTokens"].as_u64().unwrap_or(0);
        found.tokens_out += metadata["outputTokens"].as_u64().unwrap_or(0);
    }
    found
}

/// The folder the chat's VS Code window had open.
fn workspace_folder(path: &Path) -> Option<String> {
    let workspace = path.parent()?.parent()?.join("workspace.json");
    let text = std::fs::read_to_string(workspace).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let uri = value["folder"].as_str().or_else(|| value["workspace"].as_str())?;
    let path = file_uri_path(uri)?;
    // A saved workspace file names the workspace, not a folder in it.
    Some(match path.strip_suffix(".code-workspace") {
        Some(_) => Path::new(&path).parent().map_or(path.clone(), |p| p.to_string_lossy().into_owned()),
        None => path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_the_change_log() {
        let mut state = Value::Null;
        for change in [
            r#"{"kind":0,"v":{"sessionId":"s1","creationDate":1,"requests":[]}}"#,
            r#"{"kind":2,"k":["requests"],"v":[{"message":{"text":"Explain the loader"},"response":[]}]}"#,
            r#"{"kind":2,"k":["requests",0,"response"],"v":[{"value":"It reads "},{"kind":"markdownContent","value":"YAML first."}]}"#,
            r#"{"kind":1,"k":["customTitle"],"v":"Loader order"}"#,
            r#"{"kind":1,"k":["requests",0,"timestamp"],"v":1700000000000}"#,
        ] {
            apply(&mut state, &serde_json::from_str(change).unwrap());
        }
        let found = read(Path::new("/nowhere/x.jsonl"), &state);
        assert_eq!(found.title.as_deref(), Some("Loader order"));
        let events = found.into_events();
        assert!(matches!(&events[0], Event::Prompt(text) if text == "Explain the loader"));
        assert!(matches!(&events[1], Event::Reply(text) if text == "It reads YAML first."));
    }

    #[test]
    fn deletes_and_splices() {
        let mut state: Value = serde_json::from_str(r#"{"a":{"b":1,"c":2},"list":[1,2,3]}"#).unwrap();
        apply(&mut state, &serde_json::from_str(r#"{"kind":3,"k":["a","b"]}"#).unwrap());
        apply(&mut state, &serde_json::from_str(r#"{"kind":2,"k":["list"],"v":[9],"i":1}"#).unwrap());
        assert_eq!(state, serde_json::json!({"a":{"c":2},"list":[1,9]}));
    }
}
