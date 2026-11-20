//! Claude Code: one JSONL file per session under `~/.claude/projects/<dir>/`,
//! with sub-agent transcripts in `<session>/subagents/agent-*.jsonl`.

use crate::model::{Harness, Origin, Session};
use crate::sources::{typed_prompt, walk, working_folder};
use crate::{parse_ts, squash};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    super::location(Harness::ClaudeCode)
}

pub fn files() -> Vec<PathBuf> {
    walk(&root(), &|p| {
        p.extension().is_some_and(|e| e == "jsonl")
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    cwd: Option<String>,
    /// What started the session: `cli`, `claude-desktop`, and others.
    entrypoint: Option<String>,
    git_branch: Option<String>,
    timestamp: Option<String>,
    is_meta: Option<Value>,
    custom_title: Option<Value>,
    ai_title: Option<Value>,
    summary: Option<Value>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    content: Option<Content>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<Value>,
}

#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

/// Line types worth parsing. The files also hold large bookkeeping lines
/// (API request records and the like) that are skipped without parsing.
const WANTED: [&str; 5] = [
    "\"type\":\"user\"",
    "\"type\":\"assistant\"",
    "\"type\":\"custom-title\"",
    "\"type\":\"ai-title\"",
    "\"type\":\"summary\"",
];

/// Parse one session file. Returns `None` for files with no conversation.
pub fn parse(path: &Path) -> Result<Option<Session>> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut session = Session::new(Harness::ClaudeCode, stem, path.to_string_lossy());

    // Sub-agent files live at <session>/subagents/agent-<id>.jsonl.
    let parent_dir = path.parent().and_then(|p| p.file_name());
    if parent_dir.is_some_and(|d| d == "subagents") {
        session.parent = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
    }

    let mut seen_messages: HashSet<String> = HashSet::new();
    // How many lines were written in each folder; the session can move.
    let mut folders: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut custom_title = None;
    let mut other_title = None;

    let reader = BufReader::new(std::fs::File::open(path)?);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if !WANTED.iter().any(|w| line.contains(w)) {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        // The desktop app writes the same files as the command-line tool
        // and marks every line with where it came from.
        if parsed.entrypoint.as_deref() == Some("claude-desktop") {
            session.origin = Origin::ClaudeDesktop;
        }
        if let Some(ts) = parsed.timestamp.as_deref().and_then(parse_ts) {
            session.touch(ts);
        }
        if let Some(cwd) = parsed.cwd.filter(|cwd| !cwd.is_empty()) {
            *folders.entry(cwd).or_insert(0) += 1;
        }
        if let Some(branch) = parsed.git_branch.filter(|b| !b.is_empty()) {
            session.branch = Some(branch);
        }

        match parsed.kind.as_deref() {
            Some("custom-title") => {
                if let Some(title) = parsed.custom_title.as_ref().and_then(Value::as_str) {
                    custom_title = Some(title.to_string());
                }
            }
            Some("ai-title") | Some("summary") => {
                let title = parsed.ai_title.as_ref().or(parsed.summary.as_ref());
                if let Some(title) = title.and_then(Value::as_str) {
                    other_title = Some(title.to_string());
                }
            }
            Some("user") => {
                if parsed.is_meta.as_ref().and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let Some(prompt) = parsed.message.and_then(|m| m.content).and_then(prompt_text)
                else {
                    continue;
                };
                session.user_turns += 1;
                if session.first_prompt.is_empty() {
                    session.first_prompt = squash(&prompt, 600);
                }
            }
            Some("assistant") => {
                let Some(message) = parsed.message else {
                    continue;
                };
                if let Some(Content::Blocks(blocks)) = &message.content {
                    session.tool_calls += blocks
                        .iter()
                        .filter(|b| b.kind.as_deref() == Some("tool_use"))
                        .count() as u32;
                }
                if let Some(model) = message.model.filter(|m| !m.starts_with('<')) {
                    session.model = Some(model);
                }
                // One API message is written as several lines, one per content
                // block, each repeating the same usage. Count it once.
                let first_sighting = match message.id {
                    Some(id) => seen_messages.insert(id),
                    None => true,
                };
                if first_sighting {
                    session.assistant_turns += 1;
                    let usage = message.usage.unwrap_or_default();
                    session.tokens_in += usage.input_tokens + usage.cache_creation_input_tokens;
                    session.tokens_out += usage.output_tokens;
                    session.tokens_cached += usage.cache_read_input_tokens;
                }
            }
            _ => {}
        }
    }

    session.cwd = working_folder(&folders).unwrap_or_default();
    if session.user_turns == 0 && session.assistant_turns == 0 {
        return Ok(None);
    }
    session.title = custom_title
        .or(other_title)
        .unwrap_or_else(|| squash(&session.first_prompt, 90));
    Ok(Some(session))
}

/// The typed text of a user message, or `None` when the message only carries
/// tool results or harness-injected context back to the model.
fn prompt_text(content: Content) -> Option<String> {
    match content {
        Content::Text(text) => typed_prompt(&text).map(str::to_string),
        Content::Blocks(blocks) => {
            if blocks
                .iter()
                .any(|b| b.kind.as_deref() == Some("tool_result"))
            {
                return None;
            }
            let typed: Vec<&str> = blocks
                .iter()
                .filter(|b| b.kind.as_deref() == Some("text"))
                .filter_map(|b| b.text.as_ref().and_then(Value::as_str))
                .filter_map(typed_prompt)
                .collect();
            (!typed.is_empty()).then(|| typed.join("\n"))
        }
    }
}
