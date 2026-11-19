//! pi: one JSONL file per session under
//! `~/.pi/agent/sessions/--<path>--/<timestamp>_<session-id>.jsonl`.
//!
//! The first line is a header. Every later line is an entry with an `id` and
//! a `parentId`, so a session is a tree: going back and trying again adds a
//! branch instead of rewriting the file. The branch pi is on is the one that
//! ends at the last entry in the file, and that is the branch read here as
//! the conversation. Tokens, cost and time count every entry, because the
//! work on an abandoned branch still happened.
//!
//! Sessions written before pi had branches have no ids; they are read
//! top to bottom.

use crate::model::{Harness, Session};
use crate::sources::{typed_prompt, walk};
use crate::transcript::Event;
use crate::{home, parse_ts, squash};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Where pi keeps its sessions. `PI_CODING_AGENT_DIR` moves pi's whole
/// folder, as it does for pi itself.
pub fn root() -> PathBuf {
    let agent = std::env::var_os("PI_CODING_AGENT_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".pi/agent"));
    agent.join("sessions")
}

pub fn files() -> Vec<PathBuf> {
    walk(&root(), &|p: &Path| p.extension().is_some_and(|e| e == "jsonl"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    parent_id: Option<String>,
    /// An ISO string on entries; tolerated as a number too.
    timestamp: Option<Value>,
    /// On the header.
    cwd: Option<String>,
    /// On `message` entries.
    message: Option<Message>,
    /// On `session_info` entries: the name the user gave the session.
    name: Option<String>,
    /// On `model_change` entries, and on the header of older sessions.
    model_id: Option<String>,
    /// On `usage`, `compaction` and `branch_summary` entries.
    usage: Option<Usage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    role: Option<String>,
    content: Option<Content>,
    model: Option<String>,
    usage: Option<Usage>,
    stop_reason: Option<String>,
    error_message: Option<String>,
    /// On `bashExecution` messages: a shell command the user ran directly.
    command: Option<String>,
}

/// Message content is a plain string or a list of blocks. Anything else an
/// extension may have stored is accepted and ignored.
#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
    Other(serde::de::IgnoredAny),
}

/// A text, thinking, image or tool-call block. Only the fields used here are
/// read; thinking text and image data are skipped.
#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    name: Option<String>,
    arguments: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Usage {
    /// Input that was not read from the cache.
    #[serde(default)]
    input: f64,
    /// Includes reasoning.
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: f64,
    #[serde(default)]
    cache_write: f64,
    cost: Option<Cost>,
}

#[derive(Deserialize)]
struct Cost {
    total: Option<f64>,
}

/// One entry of the tree, reduced to what is needed.
struct Node {
    id: Option<String>,
    parent: Option<String>,
    message: Option<Message>,
}

/// A session file read into memory.
struct Loaded {
    session: Session,
    nodes: Vec<Node>,
    /// Indexes into `nodes` along the current branch, oldest first.
    path: Vec<usize>,
}

fn timestamp_ms(value: &Option<Value>) -> Option<i64> {
    match value.as_ref()? {
        Value::String(text) => parse_ts(text),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

fn load(path: &Path) -> Result<Loaded> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    // <timestamp>_<session-id>: the id is everything after the underscore.
    let fallback_id = stem.split_once('_').map_or(stem, |(_, id)| id);
    let mut session = Session::new(Harness::Pi, fallback_id, path.to_string_lossy());
    let mut nodes: Vec<Node> = Vec::new();
    let mut name: Option<String> = None;
    let mut selected_model: Option<String> = None;
    let mut cost = 0.0;

    let reader = BufReader::new(std::fs::File::open(path)?);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let Ok(mut entry) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        if let Some(ts) = timestamp_ms(&entry.timestamp) {
            session.touch(ts);
        }
        let mut count = |usage: &Usage| {
            session.tokens_in += (usage.input + usage.cache_write).max(0.0) as u64;
            session.tokens_out += usage.output.max(0.0) as u64;
            session.tokens_cached += usage.cache_read.max(0.0) as u64;
            cost += usage.cost.as_ref().and_then(|c| c.total).unwrap_or(0.0);
        };
        if let Some(usage) = &entry.usage {
            count(usage);
        }
        if let Some(usage) = entry.message.as_ref().and_then(|m| m.usage.as_ref()) {
            count(usage);
        }

        match entry.kind.as_deref() {
            Some("session") => {
                if let Some(id) = entry.id.take().filter(|id| !id.is_empty()) {
                    session.id = id;
                }
                if let Some(cwd) = entry.cwd.take() {
                    session.cwd = cwd;
                }
                selected_model = entry.model_id.take().or(selected_model);
                // The header is not part of the tree.
                continue;
            }
            Some("session_info") => {
                // The latest name wins; an empty one clears it.
                name = entry.name.take().filter(|n| !n.trim().is_empty());
            }
            Some("model_change") => {
                selected_model = entry.model_id.take().or(selected_model);
            }
            _ => {}
        }
        nodes.push(Node {
            id: entry.id,
            parent: entry.parent_id,
            message: entry.message,
        });
    }

    let path_through = current_branch(&nodes);
    // A session often opens with a slash command such as `/mode`, which says
    // nothing about it; the first real request is kept in preference.
    let mut first_command: Option<String> = None;
    for &i in &path_through {
        let Some(message) = &nodes[i].message else { continue };
        match message.role.as_deref() {
            Some("user") => {
                if let Some(prompt) = typed_text(&message.content) {
                    session.user_turns += 1;
                    let text = prompt.trim();
                    let bare_command = text.starts_with('/') && !text.contains(char::is_whitespace);
                    if bare_command {
                        first_command.get_or_insert_with(|| text.to_string());
                    } else if session.first_prompt.is_empty() {
                        session.first_prompt = squash(text, 600);
                    }
                }
            }
            Some("assistant") => {
                session.assistant_turns += 1;
                if let Some(Content::Blocks(blocks)) = &message.content {
                    session.tool_calls += blocks
                        .iter()
                        .filter(|b| b.kind.as_deref() == Some("toolCall"))
                        .count() as u32;
                }
                if let Some(model) = message.model.as_ref().filter(|m| !m.is_empty()) {
                    session.model = Some(model.clone());
                }
            }
            _ => {}
        }
    }
    if session.first_prompt.is_empty() {
        session.first_prompt = first_command.unwrap_or_default();
    }
    if session.model.is_none() {
        session.model = selected_model;
    }
    session.cost_usd = (cost > 0.0).then_some(cost);
    session.title = name.unwrap_or_else(|| squash(&session.first_prompt, 90));
    Ok(Loaded {
        session,
        nodes,
        path: path_through,
    })
}

/// The entries from the root to the last entry in the file, which is where
/// pi resumes. Files without ids are one straight line.
fn current_branch(nodes: &[Node]) -> Vec<usize> {
    if !nodes.iter().any(|node| node.id.is_some()) {
        return (0..nodes.len()).collect();
    }
    let by_id: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, node)| node.id.as_deref().map(|id| (id, i)))
        .collect();
    let mut path = Vec::new();
    let mut at = nodes.len().checked_sub(1);
    while let Some(i) = at {
        path.push(i);
        // A parent always comes earlier in the file; anything else is a
        // damaged file, and following it could loop.
        at = nodes[i]
            .parent
            .as_deref()
            .and_then(|parent| by_id.get(parent).copied())
            .filter(|&parent| parent < i);
    }
    path.reverse();
    path
}

/// What the user typed in a message, with injected wrappers removed.
fn typed_text(content: &Option<Content>) -> Option<String> {
    let typed: Vec<&str> = match content.as_ref()? {
        Content::Text(text) => typed_prompt(text).into_iter().collect(),
        Content::Blocks(blocks) => blocks
            .iter()
            .filter(|b| b.kind.as_deref() == Some("text"))
            .filter_map(|b| b.text.as_deref())
            .filter_map(typed_prompt)
            .collect(),
        Content::Other(_) => Vec::new(),
    };
    (!typed.is_empty()).then(|| typed.join("\n"))
}

/// Parse one session file. Returns `None` for files with no conversation.
pub fn parse(path: &Path) -> Result<Option<Session>> {
    let session = load(path)?.session;
    if session.user_turns == 0 && session.assistant_turns == 0 {
        return Ok(None);
    }
    Ok(Some(session))
}

/// The conversation on the session's current branch, in order.
pub fn events(session: &Session) -> Result<Vec<Event>> {
    let loaded = load(Path::new(&session.source))?;
    let mut out = Vec::new();
    for &i in &loaded.path {
        let Some(message) = &loaded.nodes[i].message else { continue };
        match message.role.as_deref() {
            Some("user") => {
                if let Some(prompt) = typed_text(&message.content) {
                    out.push(Event::Prompt(prompt));
                }
            }
            Some("assistant") => {
                if let Some(Content::Blocks(blocks)) = &message.content {
                    for block in blocks {
                        match block.kind.as_deref() {
                            Some("text") => {
                                if let Some(text) =
                                    block.text.as_deref().filter(|t| !t.trim().is_empty())
                                {
                                    out.push(Event::Reply(text.to_string()));
                                }
                            }
                            Some("toolCall") => {
                                let target = block.arguments.as_ref().and_then(|arguments| {
                                    ["path", "file_path", "command", "pattern"]
                                        .iter()
                                        .find_map(|key| arguments[*key].as_str())
                                        .map(|t| squash(t, 120))
                                });
                                out.push(Event::Tool {
                                    name: block.name.clone().unwrap_or_else(|| "tool".to_string()),
                                    target,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                match message.stop_reason.as_deref() {
                    Some("aborted") => out.push(Event::Note("you interrupted the agent".to_string())),
                    Some("error") => out.push(Event::Note(match &message.error_message {
                        Some(error) => format!("the turn failed: {}", squash(error, 200)),
                        None => "the turn failed".to_string(),
                    })),
                    _ => {}
                }
            }
            // A shell command the user ran themselves with `!`.
            Some("bashExecution") => {
                if let Some(command) = message.command.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push(Event::Prompt(format!("!{command}")));
                }
            }
            // Tool results, system prompts and extension messages are not
            // something either side said.
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `lines` as a session file in a folder of its own.
    fn file(test: &str, lines: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scrollback-pi-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-11-19T10-00-00-000Z_0f0e0d0c-aaaa-bbbb-cccc-000000000001.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    fn user(id: &str, parent: &str, at: &str, text: &str) -> String {
        format!(
            r#"{{"type":"message","id":"{id}","parentId":{parent},"timestamp":"2026-11-19T10:{at}.000Z","message":{{"role":"user","content":"{text}","timestamp":1}}}}"#
        )
    }

    fn assistant(id: &str, parent: &str, at: &str, content: &str, stop: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"message","id":"{id}","parentId":"{parent}","timestamp":"2026-11-19T10:{at}.000Z","message":{{"role":"assistant","content":{content},"provider":"anthropic","model":"claude-sonnet-4-5","usage":{usage},"stopReason":"{stop}","timestamp":1}}}}"#
        )
    }

    const HEADER: &str = r#"{"type":"session","version":3,"id":"0f0e0d0c-aaaa-bbbb-cccc-000000000001","timestamp":"2026-11-19T10:00:00.000Z","cwd":"/work/api"}"#;
    const USAGE: &str = r#"{"input":100,"output":40,"cacheRead":9000,"cacheWrite":500,"reasoning":10,"totalTokens":9640,"cost":{"input":0.1,"output":0.2,"cacheRead":0.3,"cacheWrite":0.4,"total":1.0}}"#;

    fn describe(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                Event::Prompt(text) => format!("you: {text}"),
                Event::Reply(text) => format!("agent: {text}"),
                Event::Tool { name, target } => {
                    format!("tool: {name} {}", target.as_deref().unwrap_or("-"))
                }
                Event::Note(text) => format!("note: {text}"),
            })
            .collect()
    }

    #[test]
    fn reads_a_session_with_tools_usage_and_a_name() {
        let lines = [
            HEADER.to_string(),
            r#"{"type":"message","id":"s1","parentId":null,"timestamp":"2026-11-19T10:00:00.000Z","message":{"role":"system","content":"","sections":{"preamble":"You are an expert"},"timestamp":1}}"#.to_string(),
            user("u1", "\"s1\"", "00:10", "add a health check"),
            assistant(
                "a1",
                "u1",
                "00:20",
                r#"[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Adding it."},{"type":"toolCall","id":"c1","name":"edit","arguments":{"path":"/work/api/src/health.rs","oldText":"a","newText":"b"}},{"type":"toolCall","id":"c2","name":"bash","arguments":{"command":"cargo test"}}]"#,
                "toolUse",
                USAGE,
            ),
            r#"{"type":"message","id":"t1","parentId":"a1","timestamp":"2026-11-19T10:00:30.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"edit","content":[{"type":"text","text":"ok"},{"type":"image","data":"AAAA","mimeType":"image/png"}],"isError":false,"timestamp":1}}"#.to_string(),
            assistant("a2", "t1", "00:40", r#"[{"type":"text","text":"Done."}]"#, "stop", USAGE),
            r#"{"type":"usage","id":"g1","parentId":"a2","timestamp":"2026-11-19T10:00:50.000Z","kind":"cache_warm","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":0,"output":0,"cacheRead":50000,"cacheWrite":0,"totalTokens":50000,"cost":{"input":0,"output":0,"cacheRead":0.5,"cacheWrite":0,"total":0.5}}}"#.to_string(),
            r#"{"type":"session_info","id":"n1","parentId":"g1","timestamp":"2026-11-19T10:01:00.000Z","name":"Health check endpoint"}"#.to_string(),
        ];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = file("basic", &lines);

        let session = parse(&path).unwrap().expect("a session");
        assert_eq!(session.harness, Harness::Pi);
        assert_eq!(session.id, "0f0e0d0c-aaaa-bbbb-cccc-000000000001");
        assert_eq!(session.cwd, "/work/api");
        assert_eq!(session.title, "Health check endpoint");
        assert_eq!(session.first_prompt, "add a health check");
        assert_eq!(session.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!((session.user_turns, session.assistant_turns, session.tool_calls), (1, 2, 2));
        // Two assistant messages plus the cache-warming entry.
        assert_eq!(session.tokens_in, 2 * (100 + 500));
        assert_eq!(session.tokens_out, 2 * 40);
        assert_eq!(session.tokens_cached, 2 * 9000 + 50000);
        assert!((session.cost_usd.unwrap() - 2.5).abs() < 1e-9);
        assert_eq!(session.active_ms, 60_000);

        let events = events(&session).unwrap();
        assert_eq!(
            describe(&events),
            [
                "you: add a health check",
                "agent: Adding it.",
                "tool: edit /work/api/src/health.rs",
                "tool: bash cargo test",
                "agent: Done.",
            ]
        );
    }

    #[test]
    fn follows_the_branch_that_ends_the_file() {
        // u1 -> a1 -> u2 -> a2 was abandoned; the user went back to a1 and
        // asked something else, so u3 -> a3 is the current branch.
        let lines = [
            HEADER.to_string(),
            user("u1", "null", "00:10", "first question"),
            assistant("a1", "u1", "00:20", r#"[{"type":"text","text":"first answer"}]"#, "stop", USAGE),
            user("u2", "\"a1\"", "00:30", "abandoned question"),
            assistant("a2", "u2", "00:40", r#"[{"type":"text","text":"abandoned answer"}]"#, "stop", USAGE),
            r#"{"type":"branch_summary","id":"b1","parentId":"a1","timestamp":"2026-11-19T10:00:50.000Z","fromId":"a2","summary":"Tried the other way.","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15,"cost":{"total":0.25}}}"#.to_string(),
            user("u3", "\"b1\"", "01:00", "second question"),
            assistant("a3", "u3", "01:10", r#"[{"type":"text","text":"second answer"}]"#, "stop", USAGE),
        ];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = file("branch", &lines);

        let session = parse(&path).unwrap().expect("a session");
        assert_eq!((session.user_turns, session.assistant_turns), (2, 2));
        // The abandoned branch still cost tokens, and so did its summary.
        assert_eq!(session.tokens_out, 3 * 40 + 5);
        assert!((session.cost_usd.unwrap() - 3.25).abs() < 1e-9);
        assert_eq!(
            describe(&events(&session).unwrap()),
            [
                "you: first question",
                "agent: first answer",
                "you: second question",
                "agent: second answer",
            ]
        );
    }

    #[test]
    fn notes_failed_and_interrupted_turns_and_direct_shell_commands() {
        let lines = [
            HEADER.to_string(),
            user("u1", "null", "00:10", "run the migration"),
            r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-11-19T10:00:20.000Z","message":{"role":"assistant","content":[],"model":"gpt-5.1-codex","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"cost":{"total":0}},"stopReason":"aborted","errorMessage":"Request was aborted","timestamp":1}}"#.to_string(),
            r#"{"type":"message","id":"x1","parentId":"a1","timestamp":"2026-11-19T10:00:30.000Z","message":{"role":"bashExecution","command":"git status","output":"clean","exitCode":0,"cancelled":false,"truncated":false,"timestamp":1}}"#.to_string(),
            r#"{"type":"message","id":"a2","parentId":"x1","timestamp":"2026-11-19T10:00:40.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Trying."}],"model":"gpt-5.1-codex","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"cost":{"total":0}},"stopReason":"error","errorMessage":"terminated","timestamp":1}}"#.to_string(),
            r#"{"type":"custom_message","id":"c1","parentId":"a2","timestamp":"2026-11-19T10:00:50.000Z","customType":"ext","content":"Injected context","display":true}"#.to_string(),
            r#"{"type":"message","id":"c2","parentId":"c1","timestamp":"2026-11-19T10:00:55.000Z","message":{"role":"custom","customType":"ext","content":{"odd":true},"display":false,"timestamp":1}}"#.to_string(),
        ];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = file("notes", &lines);

        let session = parse(&path).unwrap().expect("a session");
        assert_eq!(session.cost_usd, None);
        assert_eq!(session.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(
            describe(&events(&session).unwrap()),
            [
                "you: run the migration",
                "note: you interrupted the agent",
                "you: !git status",
                "agent: Trying.",
                "note: the turn failed: terminated",
            ]
        );
    }

    #[test]
    fn reads_sessions_from_before_branches_top_to_bottom() {
        let lines = [
            r#"{"type":"session","id":"old-1","timestamp":"2025-11-20T23:33:50.805Z","cwd":"/work/old","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}"#,
            r#"{"type":"message","timestamp":"2025-11-20T23:34:00.000Z","message":{"role":"user","content":[{"type":"text","text":"hello"}],"timestamp":1}}"#,
            r#"{"type":"thinking_level_change","timestamp":"2025-11-20T23:34:05.000Z","thinkingLevel":"high"}"#,
            r#"{"type":"message","timestamp":"2025-11-20T23:34:10.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"}],"usage":{"input":3,"output":2,"cacheRead":0,"cacheWrite":0,"cost":{"total":0.01}},"stopReason":"stop","timestamp":1}}"#,
        ];
        let path = file("legacy", &lines);
        let session = parse(&path).unwrap().expect("a session");
        assert_eq!(session.id, "old-1");
        assert_eq!(session.title, "hello");
        // No assistant message names a model, so the header's choice is used.
        assert_eq!(session.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(describe(&events(&session).unwrap()), ["you: hello", "agent: hi"]);
    }

    #[test]
    fn an_opening_slash_command_is_not_the_title() {
        let lines = [
            HEADER.to_string(),
            user("u1", "null", "00:10", "/mode"),
            user("u2", "\"u1\"", "00:20", "port the selector to the theme system"),
            assistant("a1", "u2", "00:30", r#"[{"type":"text","text":"On it."}]"#, "stop", USAGE),
        ];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let session = parse(&file("slash", &lines)).unwrap().expect("a session");
        assert_eq!(session.user_turns, 2);
        assert_eq!(session.title, "port the selector to the theme system");

        // With nothing else to go on, the command is still better than nothing.
        let only = [HEADER.to_string(), user("u1", "null", "00:10", "/mode")];
        let only: Vec<&str> = only.iter().map(String::as_str).collect();
        let session = parse(&file("slash-only", &only)).unwrap().expect("a session");
        assert_eq!(session.title, "/mode");
    }

    #[test]
    fn a_file_with_no_conversation_is_not_a_session() {
        let path = file("empty", &[HEADER]);
        assert!(parse(&path).unwrap().is_none());
    }
}
