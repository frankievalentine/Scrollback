//! Cursor: the editor's agent conversations ("composers"), kept in one SQLite
//! file at `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
//! on macOS, and under `%APPDATA%` or `~/.config` on Windows and Linux.
//! It is opened read-only; Cursor may have it open at the same time.
//!
//! The file is a key-value table. `composerData:<id>` holds a conversation's
//! name, times and the order of its messages; `bubbleId:<id>:<message>` holds
//! one message: something the user typed, text the agent wrote, or a tool
//! call. Which folder a conversation belongs to is recorded on some messages,
//! and otherwise in each workspace's own small state file.

use crate::model::{Harness, Session};
use crate::sources::typed_prompt;
use crate::transcript::Event;
use crate::{parse_ts, squash};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn user_dir() -> PathBuf {
    super::location(Harness::Cursor)
}

pub fn db_path() -> PathBuf {
    user_dir().join("globalStorage/state.vscdb")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Composer {
    composer_id: Option<String>,
    name: Option<String>,
    created_at: Option<f64>,
    last_updated_at: Option<f64>,
    /// The messages in order, by id. Their content is stored separately.
    #[serde(default)]
    full_conversation_headers_only: Vec<Header>,
    /// Older versions kept the messages inside the conversation itself.
    #[serde(default)]
    conversation: Vec<Bubble>,
    model_config: Option<Model>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    bubble_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    model_name: Option<String>,
}

/// One message. Type 1 is the user, type 2 the agent.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bubble {
    #[serde(rename = "type")]
    kind: Option<i64>,
    text: Option<String>,
    tool_former_data: Option<Tool>,
    token_count: Option<Tokens>,
    /// An ISO string in current versions; tolerated as a number.
    created_at: Option<Value>,
    /// Cursor's own bookkeeping folder for the project, not the project.
    workspace_project_dir: Option<String>,
    /// The folders open in the window, as `file://` addresses.
    #[serde(default)]
    workspace_uris: Vec<String>,
    model_info: Option<Model>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tool {
    name: Option<String>,
    /// The call's arguments, as a JSON string.
    raw_args: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Tokens {
    #[serde(default)]
    input_tokens: f64,
    #[serde(default)]
    output_tokens: f64,
}

impl Bubble {
    fn written(&self) -> Option<&str> {
        self.text.as_deref().filter(|t| !t.trim().is_empty())
    }

    fn at_ms(&self) -> Option<i64> {
        match self.created_at.as_ref()? {
            Value::String(text) => parse_ts(text),
            Value::Number(n) => n.as_f64().map(|ms| ms as i64),
            _ => None,
        }
    }

    /// The tool this message called, with the file or command it was aimed at.
    fn call(&self) -> Option<(String, Option<String>)> {
        let tool = self.tool_former_data.as_ref()?;
        let name = tool.name.clone().filter(|n| !n.is_empty())?;
        let arguments: Option<Value> = tool
            .raw_args
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
        let target = arguments.as_ref().and_then(|arguments| {
            [
                "target_file",
                "file_path",
                "path",
                "relative_workspace_path",
                "command",
                "query",
                "pattern",
            ]
            .iter()
            .find_map(|key| arguments[*key].as_str())
            .map(|t| squash(t, 120))
        });
        Some((name, target))
    }
}

fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    Ok(db)
}

/// A stored value's bytes. Cursor writes text in some versions and blobs in
/// others; an empty slot is `None`.
fn bytes(row: &rusqlite::Row, column: usize) -> rusqlite::Result<Option<Vec<u8>>> {
    use rusqlite::types::ValueRef;
    Ok(match row.get_ref(column)? {
        ValueRef::Text(raw) | ValueRef::Blob(raw) => Some(raw.to_vec()),
        _ => None,
    })
}

/// Every stored row whose key starts with `prefix`, parsed as `T`. Rows that
/// are empty or do not parse are skipped.
fn rows<T: serde::de::DeserializeOwned>(db: &Connection, prefix: &str) -> Result<Vec<(String, T)>> {
    let mut stmt = db.prepare("select key, value from cursorDiskKV where key like ?1")?;
    let found = stmt.query_map([format!("{prefix}%")], |row| {
        Ok((row.get::<_, String>(0)?, bytes(row, 1)?))
    })?;
    let mut out = Vec::new();
    for row in found {
        let (key, value) = row?;
        let Some(value) = value else { continue };
        if let Ok(parsed) = serde_json::from_slice::<T>(&value) {
            out.push((key, parsed));
        }
    }
    Ok(out)
}

/// A conversation's messages in the order they happened.
fn ordered(composer: Composer, mut stored: HashMap<String, Bubble>) -> Vec<Bubble> {
    if composer.full_conversation_headers_only.is_empty() {
        return composer.conversation;
    }
    composer
        .full_conversation_headers_only
        .iter()
        .filter_map(|header| header.bubble_id.as_deref())
        .filter_map(|id| stored.remove(id))
        .collect()
}

/// Turn `file:///Users/me/My%20Project` into a path, and
/// `file:///c%3A/Users/me/project` into `c:\Users\me\project`.
fn folder_path(uri: &str) -> Option<String> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| encoded.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    let path = String::from_utf8(out).ok()?;
    // Windows folders arrive as `/c:/Users/me/project`.
    let bytes = path.as_bytes();
    let drive = bytes.len() > 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    Some(if drive { path[1..].replace('/', "\\") } else { path })
}

/// Conversation id to project folder, from each workspace's own state file.
fn workspace_folders() -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct Workspace {
        folder: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Listed {
        #[serde(default)]
        all_composers: Vec<Entry>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Entry {
        composer_id: Option<String>,
    }

    let mut folders = HashMap::new();
    let Ok(entries) = std::fs::read_dir(user_dir().join("workspaceStorage")) else {
        return folders;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let folder = std::fs::read_to_string(dir.join("workspace.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Workspace>(&text).ok())
            .and_then(|workspace| workspace.folder)
            .and_then(|uri| folder_path(&uri));
        let Some(folder) = folder else { continue };
        let listed = open(&dir.join("state.vscdb")).ok().and_then(|db| {
            db.query_row(
                "select value from ItemTable where key = 'composer.composerData'",
                [],
                |row| bytes(row, 0),
            )
            .ok()
            .flatten()
        });
        let Some(listed) = listed.and_then(|raw| serde_json::from_slice::<Listed>(&raw).ok()) else {
            continue;
        };
        for id in listed.all_composers.into_iter().filter_map(|e| e.composer_id) {
            folders.insert(id, folder.clone());
        }
    }
    folders
}

/// Read every conversation. Returns an empty list when Cursor is not installed.
pub fn scan() -> Result<Vec<Session>> {
    let path = db_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    scan_at(&path, &workspace_folders())
}

fn scan_at(path: &Path, folders: &HashMap<String, String>) -> Result<Vec<Session>> {
    let db = open(path)?;
    let source = path.to_string_lossy().into_owned();

    // Every message, grouped by the conversation it belongs to.
    let mut stored: HashMap<String, HashMap<String, Bubble>> = HashMap::new();
    for (key, bubble) in rows::<Bubble>(&db, "bubbleId:")? {
        let mut parts = key.splitn(3, ':');
        if let (Some(_), Some(composer), Some(id)) = (parts.next(), parts.next(), parts.next()) {
            stored
                .entry(composer.to_string())
                .or_default()
                .insert(id.to_string(), bubble);
        }
    }

    let mut sessions = Vec::new();
    for (key, composer) in rows::<Composer>(&db, "composerData:")? {
        let id = composer
            .composer_id
            .clone()
            .unwrap_or_else(|| key.trim_start_matches("composerData:").to_string());
        let mut session = Session::new(Harness::Cursor, id.clone(), source.clone());
        let name = composer.name.clone().filter(|n| !n.trim().is_empty());
        let chosen_model = composer
            .model_config
            .as_ref()
            .and_then(|m| m.model_name.clone())
            .filter(|m| !m.is_empty() && m != "default");
        if let Some(created) = composer.created_at {
            session.touch(created as i64);
        }
        let last_update = composer.last_updated_at;

        let bubbles = ordered(composer, stored.remove(&id).unwrap_or_default());
        let mut used_model = None;
        let mut bookkeeping_dir = None;
        for bubble in &bubbles {
            if let Some(at) = bubble.at_ms() {
                session.touch(at);
            }
            if let Some(tokens) = &bubble.token_count {
                session.tokens_in += tokens.input_tokens.max(0.0) as u64;
                session.tokens_out += tokens.output_tokens.max(0.0) as u64;
            }
            if session.cwd.is_empty() {
                if let Some(folder) = bubble.workspace_uris.iter().find_map(|uri| folder_path(uri)) {
                    session.cwd = folder;
                }
            }
            if bookkeeping_dir.is_none() {
                bookkeeping_dir = bubble.workspace_project_dir.clone().filter(|d| !d.is_empty());
            }
            if let Some(model) = bubble.model_info.as_ref().and_then(|m| m.model_name.clone()) {
                used_model = Some(model).filter(|m| !m.is_empty() && m != "default");
            }
            match bubble.kind {
                Some(1) => {
                    if let Some(prompt) = bubble.written().and_then(typed_prompt) {
                        session.user_turns += 1;
                        if session.first_prompt.is_empty() {
                            session.first_prompt = squash(prompt, 600);
                        }
                    }
                }
                Some(2) => {
                    if bubble.written().is_some() {
                        session.assistant_turns += 1;
                    }
                    if bubble.call().is_some() {
                        session.tool_calls += 1;
                    }
                }
                _ => {}
            }
        }
        // Cursor keeps an entry for every chat box ever opened; most are empty.
        if session.user_turns == 0 && session.assistant_turns == 0 {
            continue;
        }
        if let Some(updated) = last_update {
            session.touch(updated as i64);
        }
        // The folder open at the time, else the workspace the conversation
        // is filed under, else Cursor's own folder for the project.
        if session.cwd.is_empty() {
            session.cwd = folders.get(&id).cloned().or(bookkeeping_dir).unwrap_or_default();
        }
        session.model = chosen_model.or(used_model);
        session.title = name.unwrap_or_else(|| squash(&session.first_prompt, 90));
        sessions.push(session);
    }
    Ok(sessions)
}

/// One conversation's events, in order.
pub fn events(session: &Session) -> Result<Vec<Event>> {
    let db = open(Path::new(&session.source))?;
    let composer: Option<Composer> = db
        .query_row(
            "select value from cursorDiskKV where key = ?1",
            [format!("composerData:{}", session.id)],
            |row| bytes(row, 0),
        )
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_slice(&raw).ok());
    let Some(composer) = composer else {
        return Ok(Vec::new());
    };
    let prefix = format!("bubbleId:{}:", session.id);
    let stored: HashMap<String, Bubble> = rows::<Bubble>(&db, &prefix)?
        .into_iter()
        .map(|(key, bubble)| (key[prefix.len()..].to_string(), bubble))
        .collect();

    let mut out = Vec::new();
    for bubble in ordered(composer, stored) {
        match bubble.kind {
            Some(1) => {
                if let Some(prompt) = bubble.written().and_then(typed_prompt) {
                    out.push(Event::Prompt(prompt.to_string()));
                }
            }
            Some(2) => {
                if let Some(text) = bubble.written() {
                    out.push(Event::Reply(text.to_string()));
                }
                if let Some((name, target)) = bubble.call() {
                    out.push(Event::Tool { name, target });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store shaped like Cursor's, in a folder of its own.
    fn store(test: &str, rows: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scrollback-cursor-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.vscdb");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("create table cursorDiskKV (key text primary key, value blob)").unwrap();
        for (key, value) in rows {
            db.execute("insert into cursorDiskKV values (?1, ?2)", [key, value]).unwrap();
        }
        db.execute("insert into cursorDiskKV values ('composerData:gone', null)", []).unwrap();
        path
    }

    const COMPOSER: &str = r#"{"_v":10,"composerId":"c1","name":"Monorepo build plan","createdAt":1768003615563,"lastUpdatedAt":1768003675563,"modelConfig":{"modelName":"gpt-5.1","maxMode":false},"fullConversationHeadersOnly":[{"bubbleId":"b1","type":1},{"bubbleId":"b2","type":2},{"bubbleId":"b3","type":2},{"bubbleId":"missing","type":2},{"bubbleId":"b4","type":2}]}"#;

    fn rows_for_c1() -> Vec<(&'static str, &'static str)> {
        vec![
            ("composerData:c1", COMPOSER),
            // Stored out of order on purpose: the headers give the order.
            ("bubbleId:c1:b4", r#"{"type":2,"text":"Plan written.","tokenCount":{"inputTokens":100,"outputTokens":40},"createdAt":"2026-01-10T00:07:25.563Z"}"#),
            ("bubbleId:c1:b1", r#"{"type":1,"text":"Plan the monorepo build","tokenCount":{"inputTokens":0,"outputTokens":0},"createdAt":"2026-01-10T00:06:55.563Z","workspaceProjectDir":"/Users/me/.cursor/projects/work-mono","workspaceUris":["file:///work/mono"]}"#),
            ("bubbleId:c1:b2", r#"{"type":2,"text":"Looking at the layout.","tokenCount":{"inputTokens":900,"outputTokens":60},"createdAt":"2026-01-10T00:07:05.563Z"}"#),
            ("bubbleId:c1:b3", r#"{"type":2,"text":"","toolFormerData":{"tool":7,"name":"edit_file","rawArgs":"{\"target_file\": \"/work/mono/turbo.json\", \"instructions\": \"add\"}","status":"completed","result":"..."},"tokenCount":{"inputTokens":0,"outputTokens":0},"createdAt":"2026-01-10T00:07:15.563Z"}"#),
            // A chat box that was opened and never used.
            ("composerData:c2", r#"{"_v":10,"composerId":"c2","createdAt":1768003615563,"fullConversationHeadersOnly":[]}"#),
        ]
    }

    #[test]
    fn reads_a_conversation_in_header_order() {
        let path = store("basic", &rows_for_c1());
        let sessions = scan_at(&path, &HashMap::new()).unwrap();
        assert_eq!(sessions.len(), 1, "the unused chat box is not a session");
        let s = &sessions[0];
        assert_eq!((s.harness, s.id.as_str()), (Harness::Cursor, "c1"));
        assert_eq!(s.title, "Monorepo build plan");
        assert_eq!(s.first_prompt, "Plan the monorepo build");
        assert_eq!(s.cwd, "/work/mono");
        assert_eq!(s.model.as_deref(), Some("gpt-5.1"));
        assert_eq!((s.user_turns, s.assistant_turns, s.tool_calls), (1, 2, 1));
        assert_eq!((s.tokens_in, s.tokens_out), (1000, 100));
        assert_eq!(s.started_ms, 1768003615563);
        assert_eq!(s.updated_ms, 1768003675563);

        let described: Vec<String> = events(s)
            .unwrap()
            .iter()
            .map(|event| match event {
                Event::Prompt(text) => format!("you: {text}"),
                Event::Reply(text) => format!("agent: {text}"),
                Event::Tool { name, target } => format!("tool: {name} {}", target.as_deref().unwrap_or("-")),
                Event::Note(text) => format!("note: {text}"),
            })
            .collect();
        assert_eq!(
            described,
            [
                "you: Plan the monorepo build",
                "agent: Looking at the layout.",
                "tool: edit_file /work/mono/turbo.json",
                "agent: Plan written.",
            ]
        );
    }

    #[test]
    fn falls_back_to_the_workspace_for_the_folder_and_the_prompt_for_the_title() {
        let composer = r#"{"composerId":"c9","createdAt":1768003615563,"modelConfig":{"modelName":"default"},"fullConversationHeadersOnly":[{"bubbleId":"b1"},{"bubbleId":"b2"}]}"#;
        let path = store(
            "fallback",
            &[
                ("composerData:c9", composer),
                ("bubbleId:c9:b1", r#"{"type":1,"text":"why is the build slow"}"#),
                ("bubbleId:c9:b2", r#"{"type":2,"text":"Because of X.","modelInfo":{"modelName":"claude-4.5-sonnet"}}"#),
            ],
        );
        let folders = HashMap::from([("c9".to_string(), "/work/My Project".to_string())]);
        let sessions = scan_at(&path, &folders).unwrap();
        assert_eq!(sessions[0].title, "why is the build slow");
        assert_eq!(sessions[0].cwd, "/work/My Project");
        assert_eq!(sessions[0].model.as_deref(), Some("claude-4.5-sonnet"));
    }

    #[test]
    fn folder_uris_are_decoded() {
        assert_eq!(folder_path("file:///Users/me/My%20Project").as_deref(), Some("/Users/me/My Project"));
        assert_eq!(folder_path("file:///Users/me/caf%C3%A9").as_deref(), Some("/Users/me/café"));
        assert_eq!(folder_path("file:///c%3A/Users/me/My%20Project").as_deref(), Some(r"c:\Users\me\My Project"));
        assert_eq!(folder_path("vscode-remote://ssh/x"), None);
    }
}
