//! Reads one session back from its harness as a flat list of events, and
//! condenses that list into the text a summary is written from.
//!
//! Only what the user typed, what the agent said, and the names and targets of
//! tool calls are kept. Tool output and file contents are never read into an
//! event, so they cannot end up in a summary request.

use crate::model::{Harness, Session};
use crate::sources::typed_prompt;
use crate::squash;
use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};

#[derive(Debug, Clone)]
pub enum Event {
    /// Something the user typed.
    Prompt(String),
    /// Text the agent wrote to the user.
    Reply(String),
    /// A tool call: its name and, when there is an obvious one, the file or
    /// command it was aimed at.
    Tool { name: String, target: Option<String> },
    /// Something the harness recorded about the turn itself: it failed, was
    /// aborted, or the user interrupted it.
    Note(String),
}

/// The marker Claude Code writes into the user role when a turn is stopped.
const INTERRUPTED: &str = "[Request interrupted";

/// Every event of a session, in order.
pub fn events(session: &Session) -> Result<Vec<Event>> {
    // Another machine's session is read from what it shared.
    if session.machine.is_some() {
        return crate::sync::events(session);
    }
    match session.harness {
        Harness::ClaudeCode => claude_code(session),
        Harness::Codex => codex(session),
        Harness::OpenCode => opencode(session),
        Harness::Pi => crate::sources::pi::events(session),
        Harness::Cursor => crate::sources::cursor::events(session),
        Harness::Fx => crate::sources::fx::events(session),
        Harness::Grok => crate::sources::grok::events(session),
        Harness::Hermes => crate::sources::hermes::events(session),
        Harness::Copilot => crate::sources::copilot::events(session),
        Harness::CopilotChat => crate::sources::vscode::events(session),
    }
}

fn claude_code(session: &Session) -> Result<Vec<Event>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Line {
        #[serde(rename = "type")]
        kind: Option<String>,
        is_meta: Option<Value>,
        message: Option<Message>,
    }
    #[derive(Deserialize)]
    struct Message {
        content: Option<Value>,
    }

    let mut out = Vec::new();
    let reader = BufReader::new(std::fs::File::open(&session.source)?);
    for line in reader.lines().map_while(Result::ok) {
        let is_user = line.contains("\"type\":\"user\"");
        if !is_user && !line.contains("\"type\":\"assistant\"") {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        let Some(content) = parsed.message.and_then(|m| m.content) else {
            continue;
        };
        match parsed.kind.as_deref() {
            Some("user") => {
                if parsed.is_meta.as_ref().and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                match &content {
                    Value::String(text) => {
                        if text.trim_start().starts_with(INTERRUPTED) {
                            out.push(Event::Note("you interrupted the agent".to_string()));
                        } else if let Some(typed) = typed_prompt(text) {
                            out.push(Event::Prompt(typed.to_string()));
                        }
                    }
                    Value::Array(blocks) => {
                        // Tool results travel in the user role; skip them.
                        if blocks.iter().any(|b| b["type"] == "tool_result") {
                            continue;
                        }
                        let texts: Vec<&str> = blocks
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect();
                        if texts.iter().any(|t| t.trim_start().starts_with(INTERRUPTED)) {
                            out.push(Event::Note("you interrupted the agent".to_string()));
                        }
                        let typed: Vec<&str> =
                            texts.into_iter().filter_map(typed_prompt).collect();
                        if !typed.is_empty() {
                            out.push(Event::Prompt(typed.join("\n")));
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                let Value::Array(blocks) = &content else {
                    continue;
                };
                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") => {
                            if let Some(text) = block["text"].as_str().filter(|t| !t.trim().is_empty()) {
                                out.push(Event::Reply(text.to_string()));
                            }
                        }
                        Some("tool_use") => {
                            let input = &block["input"];
                            let target = ["file_path", "path", "notebook_path", "command", "pattern"]
                                .iter()
                                .find_map(|key| input[*key].as_str())
                                .map(|t| squash(t, 120));
                            out.push(Event::Tool {
                                name: block["name"].as_str().unwrap_or("tool").to_string(),
                                target,
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn codex(session: &Session) -> Result<Vec<Event>> {
    #[derive(Deserialize)]
    struct Line {
        #[serde(rename = "type")]
        kind: Option<String>,
        payload: Option<Payload>,
    }
    #[derive(Deserialize)]
    struct Payload {
        #[serde(rename = "type")]
        kind: Option<String>,
        message: Option<Value>,
        name: Option<Value>,
        arguments: Option<Value>,
        input: Option<Value>,
        reason: Option<Value>,
    }

    let mut out = Vec::new();
    let reader = BufReader::new(std::fs::File::open(&session.source)?);
    for line in reader.lines().map_while(Result::ok) {
        let Ok(parsed) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        let Some(payload) = parsed.payload else {
            continue;
        };
        let message = payload.message.as_ref().and_then(Value::as_str);
        let name = payload.name.as_ref().and_then(Value::as_str).unwrap_or("tool");
        match (parsed.kind.as_deref(), payload.kind.as_deref()) {
            (Some("event_msg"), Some("user_message")) => {
                if let Some(typed) = message.and_then(typed_prompt) {
                    out.push(Event::Prompt(typed.to_string()));
                }
            }
            (Some("event_msg"), Some("agent_message")) => {
                if let Some(text) = message.filter(|t| !t.trim().is_empty()) {
                    out.push(Event::Reply(text.to_string()));
                }
            }
            (Some("event_msg"), Some("turn_aborted")) => {
                let reason = payload.reason.as_ref().and_then(Value::as_str);
                out.push(Event::Note(match reason {
                    Some(reason) => format!("the turn was aborted ({reason})"),
                    None => "the turn was aborted".to_string(),
                }));
            }
            (Some("response_item"), Some("function_call" | "local_shell_call")) => {
                // Arguments arrive as a JSON string, e.g. {"command": ["bash", "-lc", "..."]}.
                let args: Value = payload
                    .arguments
                    .as_ref()
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or(Value::Null);
                let target = match &args["command"] {
                    Value::String(command) => Some(squash(command, 120)),
                    Value::Array(parts) => parts
                        .last()
                        .and_then(Value::as_str)
                        .map(|command| squash(command, 120)),
                    _ => args["cmd"].as_str().map(|command| squash(command, 120)),
                };
                out.push(Event::Tool {
                    name: name.to_string(),
                    target,
                });
            }
            (Some("response_item"), Some("custom_tool_call")) => {
                // apply_patch names its files in "*** Update File: path" lines.
                let patch = payload.input.as_ref().and_then(Value::as_str).unwrap_or("");
                let files: Vec<&str> = patch
                    .lines()
                    .filter_map(|l| {
                        l.strip_prefix("*** Update File: ")
                            .or_else(|| l.strip_prefix("*** Add File: "))
                            .or_else(|| l.strip_prefix("*** Delete File: "))
                    })
                    .collect();
                if files.is_empty() {
                    out.push(Event::Tool {
                        name: name.to_string(),
                        target: None,
                    });
                }
                for file in files {
                    out.push(Event::Tool {
                        name: name.to_string(),
                        target: Some(file.trim().to_string()),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn opencode(session: &Session) -> Result<Vec<Event>> {
    let db = Connection::open_with_flags(&session.source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    // Left join: a turn that failed outright has a message row with an error
    // and no parts at all.
    let mut stmt = db.prepare(
        "select m.id,
                json_extract(m.data, '$.role'),
                json_extract(m.data, '$.error.name'),
                json_extract(m.data, '$.error.data.message'),
                json_extract(p.data, '$.type'),
                json_extract(p.data, '$.text'),
                json_extract(p.data, '$.synthetic'),
                json_extract(p.data, '$.tool'),
                coalesce(json_extract(p.data, '$.state.input.filePath'),
                         json_extract(p.data, '$.state.input.path'),
                         json_extract(p.data, '$.state.input.command'),
                         json_extract(p.data, '$.state.input.pattern'))
         from message m left join part p on p.message_id = m.id
         where m.session_id = ?1
         order by m.time_created, m.id, p.time_created, p.id",
    )?;
    let rows = stmt.query_map([&session.id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;

    let mut out = Vec::new();
    let mut current = String::new();
    let mut failure: Option<String> = None;
    for row in rows {
        let (message, role, error, detail, kind, text, synthetic, tool, target) = row?;
        if message != current {
            // The previous message is complete; report how it ended.
            if let Some(note) = failure.take() {
                out.push(Event::Note(note));
            }
            current = message;
            failure = error.map(|name| match (name.as_str(), detail) {
                ("MessageAbortedError", _) => "the turn was aborted before it finished".to_string(),
                (_, Some(detail)) => format!("the turn failed: {}", squash(&detail, 160)),
                (name, None) => format!("the turn failed: {name}"),
            });
        }
        match (role.as_str(), kind.as_str()) {
            ("user", "text") if !synthetic => {
                if let Some(typed) = text.as_deref().and_then(typed_prompt) {
                    out.push(Event::Prompt(typed.to_string()));
                }
            }
            ("assistant", "text") => {
                if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                    out.push(Event::Reply(text));
                }
            }
            (_, "tool") => out.push(Event::Tool {
                name: tool.unwrap_or_else(|| "tool".to_string()),
                target: target.map(|t| squash(&t, 120)),
            }),
            _ => {}
        }
    }
    if let Some(note) = failure {
        out.push(Event::Note(note));
    }
    Ok(out)
}

/// One tool call: its name and what it was aimed at.
pub type Call = (String, Option<String>);

/// Events grouped for reading: a run of tool calls becomes one block.
#[derive(Debug, Clone)]
pub enum Block {
    Prompt(String),
    Reply(String),
    Tools(Vec<Call>),
    Note(String),
}

/// Group a session's events into the blocks a reader sees.
pub fn blocks(events: Vec<Event>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for event in events {
        match event {
            Event::Prompt(text) => out.push(Block::Prompt(text)),
            Event::Reply(text) => out.push(Block::Reply(text)),
            Event::Note(note) => out.push(Block::Note(note)),
            Event::Tool { name, target } => match out.last_mut() {
                Some(Block::Tools(calls)) => calls.push((name, target)),
                _ => out.push(Block::Tools(vec![(name, target)])),
            },
        }
    }
    out
}

/// How many blocks `blocks` would make of these events.
pub fn block_count(events: &[Event]) -> usize {
    let mut count = 0;
    let mut in_tools = false;
    for event in events {
        let is_tool = matches!(event, Event::Tool { .. });
        if !(is_tool && in_tools) {
            count += 1;
        }
        in_tools = is_tool;
    }
    count
}

/// "6 calls · Read ×4, Grep ×2"
pub fn tools_line(calls: &[Call]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for (name, _) in calls {
        match counts.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 += 1,
            None => counts.push((name, 1)),
        }
    }
    let names: Vec<String> = counts
        .iter()
        .map(|(name, count)| match count {
            1 => name.to_string(),
            _ => format!("{name} ×{count}"),
        })
        .collect();
    let noun = if calls.len() == 1 { "call" } else { "calls" };
    format!("{} {noun} · {}", calls.len(), names.join(", "))
}

/// True for tools that change files, whatever the harness calls them.
fn edits_files(tool: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    ["edit", "write", "patch", "replace"].iter().any(|word| tool.contains(word))
}

/// Project files the session's edit tools were aimed at, relative to the
/// session's working directory, in first-touched order. Files outside the
/// project (scratch files, the agent's own notes) are left out.
pub fn files_edited(session: &Session, events: &[Event]) -> Vec<String> {
    let prefix = format!("{}/", session.cwd.trim_end_matches('/'));
    let mut files: Vec<String> = Vec::new();
    for event in events {
        let Event::Tool { name, target: Some(target) } = event else {
            continue;
        };
        if !edits_files(name) {
            continue;
        }
        let path = match target.strip_prefix(&prefix) {
            Some(relative) => relative,
            None if !target.starts_with('/') && !target.starts_with('~') => target,
            None => continue,
        };
        if !files.iter().any(|f| f == path) {
            files.push(path.to_string());
        }
    }
    files
}

const PROMPT_LIMIT: usize = 1600;
const REPLY_LIMIT: usize = 900;
/// The last reply usually says where things were left, so it gets more room.
const FINAL_REPLY_LIMIT: usize = 2400;

/// Collapse whitespace and, if `text` is longer than `limit` characters, keep
/// its start and its end with a note in between saying how much was removed.
/// The note matters: a bare ellipsis reads as the speaker trailing off.
fn shorten(text: &str, limit: usize) -> String {
    let flat = squash(text, usize::MAX);
    let total = flat.chars().count();
    if total <= limit {
        return flat;
    }
    let head: String = flat.chars().take(limit * 6 / 10).collect();
    let tail_len = limit * 3 / 10;
    let tail: String = flat.chars().skip(total - tail_len).collect();
    let removed = total - head.chars().count() - tail_len;
    format!("{head} [trimmed: {removed} characters removed here] {tail}")
}

/// The text a summary is written from: a short header, then the conversation
/// with runs of tool calls collapsed to one bracketed line. When that is
/// longer than `budget` characters the middle is dropped and marked.
///
/// Each block is numbered from 1, and block `#n` here is block `n - 1` of
/// `blocks`, so a summary can say where in the transcript a step begins.
pub fn condense(session: &Session, events: &[Event], budget: usize) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut tools: Vec<(String, usize)> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    fn flush(blocks: &mut Vec<String>, tools: &mut Vec<(String, usize)>, touched: &mut Vec<String>) {
        if tools.is_empty() {
            return;
        }
        let names: Vec<String> = tools
            .iter()
            .map(|(name, count)| match count {
                1 => name.clone(),
                _ => format!("{name} ×{count}"),
            })
            .collect();
        let mut line = format!("[tools: {}", names.join(", "));
        if !touched.is_empty() {
            line.push_str(&format!("; edited {}", touched.join(", ")));
        }
        line.push(']');
        blocks.push(line);
        tools.clear();
        touched.clear();
    }

    let prefix = format!("{}/", session.cwd.trim_end_matches('/'));
    let final_reply = events.iter().rposition(|e| matches!(e, Event::Reply(_)));
    for (i, event) in events.iter().enumerate() {
        match event {
            Event::Prompt(text) => {
                flush(&mut blocks, &mut tools, &mut touched);
                blocks.push(format!("USER: {}", shorten(text, PROMPT_LIMIT)));
            }
            Event::Reply(text) => {
                flush(&mut blocks, &mut tools, &mut touched);
                let limit = if Some(i) == final_reply {
                    FINAL_REPLY_LIMIT
                } else {
                    REPLY_LIMIT
                };
                blocks.push(format!("AGENT: {}", shorten(text, limit)));
            }
            Event::Note(note) => {
                flush(&mut blocks, &mut tools, &mut touched);
                blocks.push(format!("[{note}]"));
            }
            Event::Tool { name, target } => {
                match tools.iter_mut().find(|(n, _)| n == name) {
                    Some(entry) => entry.1 += 1,
                    None => tools.push((name.clone(), 1)),
                }
                if let Some(target) = target.as_deref().filter(|_| edits_files(name)) {
                    let path = target.strip_prefix(&prefix).unwrap_or(target).to_string();
                    if !touched.contains(&path) && touched.len() < 8 {
                        touched.push(path);
                    }
                }
            }
        }
    }
    flush(&mut blocks, &mut tools, &mut touched);
    for (i, block) in blocks.iter_mut().enumerate() {
        block.insert_str(0, &format!("#{} ", i + 1));
    }

    let started = Local
        .timestamp_millis_opt(session.started_ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    // A session written to in the last few minutes is still being worked in.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let state = if now_ms - session.updated_ms < crate::model::IDLE_GAP_MS {
        "in progress"
    } else {
        "finished"
    };
    let mut text = format!(
        "Harness: {}\nProject: {}\nBranch: {}\nStarted: {}, active for {} minutes\nState: {}\n\n",
        session.harness.name(),
        session.project(),
        session.branch.as_deref().unwrap_or("unknown"),
        started,
        session.duration_ms() / 60_000,
        state,
    );

    let total: usize = blocks.iter().map(|b| b.len() + 2).sum();
    if total <= budget {
        text.push_str(&blocks.join("\n\n"));
        return text;
    }

    // Too long: keep the opening and the ending, which say what was asked
    // and where it was left, and drop the middle.
    let head_budget = budget * 3 / 10;
    let tail_budget = budget * 6 / 10;
    let mut head_end = 0;
    let mut used = 0;
    while head_end < blocks.len() && used + blocks[head_end].len() <= head_budget {
        used += blocks[head_end].len() + 2;
        head_end += 1;
    }
    let head_end = head_end.max(1);
    let mut tail_start = blocks.len();
    let mut used = 0;
    while tail_start > head_end && used + blocks[tail_start - 1].len() <= tail_budget {
        used += blocks[tail_start - 1].len() + 2;
        tail_start -= 1;
    }
    text.push_str(&blocks[..head_end].join("\n\n"));
    text.push_str(&format!(
        "\n\n[trimmed: {} blocks from the middle of the session removed here]\n\n",
        tail_start - head_end
    ));
    text.push_str(&blocks[tail_start..].join("\n\n"));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Event {
        Event::Tool { name: name.to_string(), target: None }
    }

    #[test]
    fn condensed_numbers_line_up_with_blocks() {
        let events = vec![
            Event::Prompt("fix the bug".into()),
            Event::Reply("Looking.".into()),
            tool("read"),
            tool("read"),
            tool("edit"),
            Event::Note("the turn failed: timeout".into()),
            tool("bash"),
            Event::Reply("Fixed.".into()),
        ];
        assert_eq!(block_count(&events), 6);
        assert_eq!(blocks(events.clone()).len(), 6);

        let session = Session::new(Harness::ClaudeCode, "s", "test");
        let text = condense(&session, &events, 10_000);
        let numbered: Vec<&str> = text.split("\n\n").filter(|b| b.starts_with('#')).collect();
        assert_eq!(numbered.len(), 6);
        assert!(numbered[0].starts_with("#1 USER: fix the bug"));
        assert!(numbered[2].starts_with("#3 [tools: read ×2, edit"));
        assert!(numbered[3].starts_with("#4 [the turn failed"));
        assert!(numbered[5].starts_with("#6 AGENT: Fixed."));
    }

    #[test]
    fn numbers_survive_the_middle_being_trimmed() {
        let mut events = Vec::new();
        for i in 0..40 {
            events.push(Event::Prompt(format!("request {i} {}", "x".repeat(200))));
            events.push(Event::Reply(format!("reply {i} {}", "y".repeat(200))));
        }
        let session = Session::new(Harness::ClaudeCode, "s", "test");
        let text = condense(&session, &events, 4_000);
        assert!(text.contains("#1 USER: request 0"));
        assert!(text.contains("blocks from the middle of the session removed here"));
        // The last block keeps its true number, not its position in the text.
        assert!(text.contains("#80 AGENT: reply 39"));
    }
}
