//! Antigravity, Google's agent IDE. It keeps each conversation in
//! `~/.gemini/antigravity/conversations/<id>.pb` (and `antigravity-ide` for
//! the IDE build), but those files are encrypted, so the conversation itself
//! cannot be read.
//!
//! What is readable is enough for an entry per conversation:
//!
//! - The app's own index of conversations, kept under the key
//!   `antigravityUnifiedStateSync.trajectorySummaries` in its `state.vscdb`:
//!   for each one a title, the folder it worked in, when it started and was
//!   last used, the agent's latest message, and its current task.
//! - The documents the agent writes as it works, in
//!   `~/.gemini/antigravity/brain/<id>/`: a task list, an implementation
//!   plan and a walkthrough, in Markdown, each with a short summary beside it.
//!
//! The index is protocol buffers wrapped in base64, decoded here by field
//! number. Fields not understood are skipped.

use crate::model::{Harness, Session};
use crate::transcript::Event;
use anyhow::{Context, Result};
use base64::Engine;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The agent's working folders: one per build of Antigravity.
pub fn roots() -> Vec<PathBuf> {
    let base = crate::sources::location(Harness::Antigravity);
    // The IDE build keeps a sibling folder of the same shape.
    let mut roots = vec![base.clone()];
    if let Some(parent) = base.parent() {
        let ide = parent.join("antigravity-ide");
        if ide != base {
            roots.push(ide);
        }
    }
    roots.into_iter().filter(|root| root.is_dir()).collect()
}

/// Whether Antigravity has left anything to read.
pub fn present() -> bool {
    !roots().is_empty()
}

/// Every app storage database Antigravity may have written its index to.
fn index_databases() -> Vec<PathBuf> {
    let support = crate::app_support();
    let Ok(entries) = std::fs::read_dir(&support) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("Antigravity"))
        .map(|entry| entry.path().join("User/globalStorage/state.vscdb"))
        .filter(|path| path.is_file())
        .collect()
}

/// What the app's index says about one conversation.
#[derive(Debug, Default, Clone, PartialEq)]
struct Entry {
    title: String,
    steps: u64,
    folder: Option<String>,
    started_ms: i64,
    updated_ms: i64,
    /// The agent's latest message to the user.
    message: Option<String>,
    /// The task the agent last said it was on, and where it stood.
    task: Option<(String, String)>,
}

/// Every conversation Antigravity has, newest information first.
pub fn scan() -> Result<Vec<Session>> {
    let mut index: HashMap<String, Entry> = HashMap::new();
    for path in index_databases() {
        // One unreadable database should not hide the others.
        if let Ok(entries) = read_index(&path) {
            for (id, entry) in entries {
                let newer = index.get(&id).is_none_or(|old| entry.updated_ms > old.updated_ms);
                if newer {
                    index.insert(id, entry);
                }
            }
        }
    }

    let mut sessions = Vec::new();
    for root in roots() {
        let Ok(files) = std::fs::read_dir(root.join("conversations")) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("pb") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            let modified_ms = file
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            let entry = index.get(&id).cloned().unwrap_or_default();
            sessions.push(session(&root, &id, &path, entry, modified_ms));
        }
    }
    Ok(sessions)
}

fn session(root: &Path, id: &str, path: &Path, entry: Entry, modified_ms: i64) -> Session {
    let mut session = Session::new(Harness::Antigravity, id, path.to_string_lossy());
    let documents = documents(root, id);
    session.title = if entry.title.is_empty() {
        documents
            .iter()
            .find_map(|doc| doc.heading.clone())
            .unwrap_or_else(|| "Antigravity conversation".to_string())
    } else {
        entry.title.clone()
    };
    session.cwd = entry.folder.as_deref().and_then(folder_path).unwrap_or_default();
    // The agent's own summary of its task list stands in for a first prompt.
    session.first_prompt = documents
        .iter()
        .find(|doc| doc.name == "task")
        .or(documents.first())
        .and_then(|doc| doc.summary.clone())
        .unwrap_or_default();
    let updated_ms = entry.updated_ms.max(modified_ms);
    let started_ms = if entry.started_ms > 0 { entry.started_ms } else { updated_ms };
    session.started_ms = started_ms;
    session.updated_ms = updated_ms;
    session.assistant_turns = documents.len() as u32 + u32::from(entry.message.is_some());
    session.tool_calls = entry.steps as u32;
    session
}

/// One document the agent wrote: `task.md`, `implementation_plan.md` or
/// `walkthrough.md`, as last resolved.
struct Document {
    name: String,
    heading: Option<String>,
    summary: Option<String>,
    text: String,
}

const DOCUMENTS: [&str; 3] = ["task", "implementation_plan", "walkthrough"];

fn documents(root: &Path, id: &str) -> Vec<Document> {
    let folder = root.join("brain").join(id);
    let mut found = Vec::new();
    for name in DOCUMENTS {
        let resolved = folder.join(format!("{name}.md.resolved"));
        let plain = folder.join(format!("{name}.md"));
        let Ok(text) = std::fs::read_to_string(&resolved).or_else(|_| std::fs::read_to_string(&plain)) else {
            continue;
        };
        let summary = std::fs::read_to_string(folder.join(format!("{name}.md.metadata.json")))
            .ok()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .and_then(|value| value["summary"].as_str().map(str::to_string));
        let heading = text
            .lines()
            .find_map(|line| line.strip_prefix("# "))
            .map(|h| h.trim().to_string());
        found.push(Document { name: name.to_string(), heading, summary, text });
    }
    found
}

/// What there is to read of a conversation: the documents the agent wrote,
/// and its latest task and message from the app's index.
pub fn events(session: &Session) -> Result<Vec<Event>> {
    let path = Path::new(&session.source);
    let root = path
        .parent()
        .and_then(Path::parent)
        .context("an Antigravity conversation outside its folder")?;
    let mut out = vec![Event::Note(
        "Antigravity keeps its conversations encrypted. What follows is what it leaves \
         readable: the documents the agent wrote, and its latest task and message."
            .to_string(),
    )];
    for doc in documents(root, &session.id) {
        let title = match doc.name.as_str() {
            "task" => "Task list",
            "implementation_plan" => "Implementation plan",
            _ => "Walkthrough",
        };
        out.push(Event::Tool { name: "write".to_string(), target: Some(format!("{}.md", doc.name)) });
        out.push(Event::Reply(format!("**{title}**\n\n{}", doc.text.trim())));
    }
    let entry = index_databases()
        .iter()
        .filter_map(|db| read_index(db).ok())
        .flatten()
        .find(|(id, _)| *id == session.id)
        .map(|(_, entry)| entry);
    if let Some(entry) = entry {
        if let Some((name, summary)) = entry.task {
            out.push(Event::Reply(format!("**{name}**\n\n{summary}")));
        }
        if let Some(message) = entry.message {
            out.push(Event::Reply(message));
        }
    }
    Ok(out)
}

fn read_index(path: &Path) -> Result<Vec<(String, Entry)>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let raw: String = db.query_row(
        "select value from ItemTable where key = 'antigravityUnifiedStateSync.trajectorySummaries'",
        [],
        |row| row.get(0),
    )?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(raw.trim())?;
    Ok(parse_index(&bytes))
}

/// The index: repeated `{1: id, 2: {1: base64 of the summary}}`.
fn parse_index(bytes: &[u8]) -> Vec<(String, Entry)> {
    let mut out = Vec::new();
    for field in fields(bytes) {
        let Field::Bytes(1, item) = field else { continue };
        let mut id = None;
        let mut entry = None;
        for part in fields(item) {
            match part {
                Field::Bytes(1, text) => id = std::str::from_utf8(text).ok().map(str::to_string),
                Field::Bytes(2, wrapper) => {
                    for inner in fields(wrapper) {
                        if let Field::Bytes(1, encoded) = inner {
                            let decoded = std::str::from_utf8(encoded)
                                .ok()
                                .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text.trim()).ok());
                            entry = decoded.map(|summary| parse_summary(&summary));
                        }
                    }
                }
                _ => {}
            }
        }
        if let (Some(id), Some(entry)) = (id, entry) {
            out.push((id, entry));
        }
    }
    out
}

/// One conversation's summary record. Field numbers as Antigravity writes
/// them: 1 title, 2 steps, 3 and 10 recent times, 7 created, 9 workspace,
/// 12 the latest message step, 14 the latest task step.
fn parse_summary(bytes: &[u8]) -> Entry {
    let mut entry = Entry::default();
    let mut latest = 0;
    for field in fields(bytes) {
        match field {
            Field::Bytes(1, text) => entry.title = String::from_utf8_lossy(text).trim().to_string(),
            Field::Varint(2, steps) => entry.steps = steps,
            Field::Bytes(3 | 10, stamp) => latest = latest.max(timestamp_ms(stamp)),
            Field::Bytes(7, stamp) => entry.started_ms = timestamp_ms(stamp),
            Field::Bytes(9, workspace) => {
                entry.folder = fields(workspace).into_iter().find_map(|f| match f {
                    Field::Bytes(1, uri) => std::str::from_utf8(uri).ok().map(str::to_string),
                    _ => None,
                });
            }
            Field::Bytes(12, step) => entry.message = find_text(step, 94, 2),
            Field::Bytes(14, step) => {
                let name = find_text(step, 93, 1);
                let summary = find_text(step, 93, 3);
                if let (Some(name), Some(summary)) = (name, summary) {
                    entry.task = Some((name, summary));
                }
            }
            _ => {}
        }
    }
    entry.updated_ms = latest.max(entry.started_ms);
    entry
}

/// Inside a step record (`{1: {…, <container>: {<field>: text}}}`), the text.
fn find_text(step: &[u8], container: u32, field: u32) -> Option<String> {
    for outer in fields(step) {
        let Field::Bytes(1, body) = outer else { continue };
        for part in fields(body) {
            let Field::Bytes(number, inner) = part else { continue };
            if number != container {
                continue;
            }
            for item in fields(inner) {
                if let Field::Bytes(n, text) = item {
                    if n == field {
                        if let Ok(text) = std::str::from_utf8(text) {
                            return Some(text.trim().to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// A `google.protobuf.Timestamp`: seconds and nanoseconds.
fn timestamp_ms(bytes: &[u8]) -> i64 {
    let mut seconds = 0;
    let mut nanos = 0;
    for field in fields(bytes) {
        match field {
            Field::Varint(1, s) => seconds = s as i64,
            Field::Varint(2, n) => nanos = n as i64,
            _ => {}
        }
    }
    seconds * 1000 + nanos / 1_000_000
}

/// Turn `file:///Users/me/My%20Project` into a path.
fn folder_path(uri: &str) -> Option<String> {
    let encoded = uri.strip_prefix("file://")?;
    let mut out = Vec::with_capacity(encoded.len());
    let bytes = encoded.as_bytes();
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
    let path = path.trim_end_matches('/').to_string();
    (!path.is_empty()).then_some(path)
}

/// One field of a protocol buffer message.
#[derive(Debug, PartialEq)]
enum Field<'a> {
    Varint(u32, u64),
    Bytes(u32, &'a [u8]),
    Fixed,
}

/// The fields of a message, stopping quietly at anything malformed.
fn fields(mut bytes: &[u8]) -> Vec<Field<'_>> {
    fn varint(bytes: &mut &[u8]) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = bytes.split_first()?;
            *bytes = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Some(value);
            }
        }
        None
    }
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let Some(key) = varint(&mut bytes) else { break };
        let number = (key >> 3) as u32;
        match key & 7 {
            0 => match varint(&mut bytes) {
                Some(value) => out.push(Field::Varint(number, value)),
                None => break,
            },
            1 | 5 => {
                let width = if key & 7 == 1 { 8 } else { 4 };
                if bytes.len() < width {
                    break;
                }
                bytes = &bytes[width..];
                out.push(Field::Fixed);
            }
            2 => {
                let Some(length) = varint(&mut bytes) else { break };
                let length = length as usize;
                if bytes.len() < length {
                    break;
                }
                out.push(Field::Bytes(number, &bytes[..length]));
                bytes = &bytes[length..];
            }
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }

    fn bytes(number: u32, body: &[u8], out: &mut Vec<u8>) {
        varint(u64::from(number) << 3 | 2, out);
        varint(body.len() as u64, out);
        out.extend_from_slice(body);
    }

    fn number(field: u32, value: u64, out: &mut Vec<u8>) {
        varint(u64::from(field) << 3, out);
        varint(value, out);
    }

    fn stamp(seconds: u64) -> Vec<u8> {
        let mut out = Vec::new();
        number(1, seconds, &mut out);
        out
    }

    #[test]
    fn reads_a_conversation_from_the_index() {
        let mut summary = Vec::new();
        bytes(1, b"Monorepo Build Planning", &mut summary);
        number(2, 483, &mut summary);
        bytes(7, &stamp(1_768_265_310), &mut summary);
        bytes(10, &stamp(1_768_266_995), &mut summary);
        bytes(3, &stamp(1_768_267_200), &mut summary);
        let mut workspace = Vec::new();
        bytes(1, b"file:///Users/me/My%20Project", &mut workspace);
        bytes(9, &workspace, &mut summary);
        // The latest message: {1: {94: {2: text}}}
        let mut text = Vec::new();
        bytes(2, b"The dev server is running.", &mut text);
        let mut holder = Vec::new();
        bytes(94, &text, &mut holder);
        let mut step = Vec::new();
        bytes(1, &holder, &mut step);
        bytes(12, &step, &mut summary);

        let encoded = base64::engine::general_purpose::STANDARD.encode(&summary);
        let mut wrapper = Vec::new();
        bytes(1, encoded.as_bytes(), &mut wrapper);
        let mut item = Vec::new();
        bytes(1, b"b7795fae", &mut item);
        bytes(2, &wrapper, &mut item);
        let mut index = Vec::new();
        bytes(1, &item, &mut index);

        let entries = parse_index(&index);
        assert_eq!(entries.len(), 1);
        let (id, entry) = &entries[0];
        assert_eq!(id, "b7795fae");
        assert_eq!(entry.title, "Monorepo Build Planning");
        assert_eq!(entry.steps, 483);
        assert_eq!(entry.started_ms, 1_768_265_310_000);
        assert_eq!(entry.updated_ms, 1_768_267_200_000);
        assert_eq!(entry.folder.as_deref().and_then(folder_path).as_deref(), Some("/Users/me/My Project"));
        assert_eq!(entry.message.as_deref(), Some("The dev server is running."));
    }

    #[test]
    fn malformed_bytes_end_quietly() {
        assert!(fields(&[0x0a, 0xff]).is_empty());
        assert_eq!(fields(&[0x08, 0x05]), vec![Field::Varint(1, 5)]);
    }
}
