//! Hermes Agent, from Nous Research. Its sessions are in one SQLite database,
//! `state.db`, in Hermes' folder: `~/.hermes` on macOS and Linux,
//! `%LOCALAPPDATA%\hermes` on Windows, or `$HERMES_HOME`. Each named profile
//! has its own database at `profiles/<name>/state.db`.
//!
//! - `sessions`: one row per session with its title, folder, branch, model,
//!   times (seconds), token counts, and `parent_session_id`. A sub-agent's
//!   row has `source = 'subagent'`; a parent link alone can also mean the
//!   session was split when its context was compacted, which is not a
//!   sub-agent.
//! - `messages`: the conversation, with `role`, `content`, and tool calls in
//!   OpenAI's function-call shape. Rows a compaction replaced have
//!   `active = 0` and are skipped.
//!
//! Older versions kept a JSONL file per session in `sessions/`; those are
//! not read.

use crate::model::{Harness, Session};
use crate::sources::{tool_target, typed_prompt, Found};
use crate::transcript::Event;
use anyhow::Result;
use rusqlite::{Connection, OpenFlags, Row};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    super::location(Harness::Hermes)
}

/// Every Hermes database: the main one and one per profile.
pub fn databases() -> Vec<PathBuf> {
    let root = root();
    let mut found = vec![root.join("state.db")];
    if let Ok(profiles) = std::fs::read_dir(root.join("profiles")) {
        found.extend(profiles.flatten().map(|entry| entry.path().join("state.db")));
    }
    found.into_iter().filter(|path| path.is_file()).collect()
}

pub fn present() -> bool {
    !databases().is_empty()
}

fn open(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?)
}

/// Every session in every database.
pub fn scan() -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for path in databases() {
        let db = open(&path)?;
        let mut stmt = db.prepare("select * from sessions")?;
        let rows: Vec<Meta> = stmt.query_map([], |row| Ok(Meta::from(row)))?.filter_map(Result::ok).collect();
        for meta in rows {
            let mut found = messages(&db, &meta.id)?;
            found.title = meta.title.clone();
            found.cwd = meta.cwd.clone();
            found.branch = meta.branch.clone();
            found.model = meta.model.clone();
            found.started_ms = meta.started_ms;
            found.updated_ms = meta.ended_ms;
            found.parent = meta.subagent.then(|| meta.parent.clone().unwrap_or_else(|| "hermes".to_string()));
            found.tokens_in = meta.tokens_in;
            found.tokens_out = meta.tokens_out;
            found.tokens_cached = meta.tokens_cached;
            if found.events.is_empty() && meta.title.is_none() {
                continue;
            }
            let mut session = found.session(Harness::Hermes, &meta.id, &path.to_string_lossy());
            session.cost_usd = meta.cost;
            sessions.push(session);
        }
    }
    Ok(sessions)
}

pub fn events(session: &Session) -> Result<Vec<Event>> {
    let db = open(Path::new(&session.source))?;
    Ok(messages(&db, &session.id)?.into_events())
}

/// One row of `sessions`, reading only the columns this version has.
struct Meta {
    id: String,
    title: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    model: Option<String>,
    parent: Option<String>,
    subagent: bool,
    started_ms: Option<i64>,
    ended_ms: Option<i64>,
    tokens_in: u64,
    tokens_out: u64,
    tokens_cached: u64,
    cost: Option<f64>,
}

impl Meta {
    fn from(row: &Row) -> Meta {
        let text = |name: &str| row.get::<_, Option<String>>(name).ok().flatten().filter(|t| !t.is_empty());
        let seconds = |name: &str| row.get::<_, Option<f64>>(name).ok().flatten().map(|s| (s * 1000.0) as i64);
        let count = |name: &str| row.get::<_, Option<i64>>(name).ok().flatten().unwrap_or(0).max(0) as u64;
        let delegated = text("model_config").is_some_and(|config| config.contains("_delegate_from"));
        Meta {
            id: text("id").unwrap_or_default(),
            title: text("title"),
            cwd: text("cwd").or_else(|| text("git_repo_root")),
            branch: text("git_branch"),
            model: text("model"),
            parent: text("parent_session_id"),
            subagent: text("source").as_deref() == Some("subagent") || delegated,
            started_ms: seconds("started_at"),
            ended_ms: seconds("ended_at"),
            tokens_in: count("input_tokens") + count("cache_write_tokens"),
            tokens_out: count("output_tokens"),
            tokens_cached: count("cache_read_tokens"),
            cost: row.get::<_, Option<f64>>("estimated_cost_usd").ok().flatten(),
        }
    }
}

fn messages(db: &Connection, id: &str) -> Result<Found> {
    let mut found = Found::default();
    // Rows a compaction replaced are kept with `active = 0`; older
    // databases have no such column.
    let query = |active: bool| {
        format!(
            "select role, content, tool_calls, tool_name, timestamp from messages where session_id = ?1 {} order by timestamp, id",
            if active { "and coalesce(active, 1) = 1" } else { "" }
        )
    };
    let mut stmt = match db.prepare(&query(true)) {
        Ok(stmt) => stmt,
        Err(_) => db.prepare(&query(false))?,
    };
    let rows = stmt.query_map([id], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<f64>>(4)?,
        ))
    })?;
    for row in rows {
        let (role, content, calls, _tool, at) = row?;
        let ts = at.map_or(0, |s| (s * 1000.0) as i64);
        match role.as_deref() {
            Some("user") => {
                if let Some(text) = content.as_deref().and_then(typed_prompt) {
                    found.push(ts, Event::Prompt(text.to_string()));
                }
            }
            Some("assistant") => {
                if let Some(text) = content.filter(|t| !t.trim().is_empty()) {
                    found.push(ts, Event::Reply(text));
                }
                let calls: Value = calls.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or(Value::Null);
                for call in calls.as_array().into_iter().flatten() {
                    let function = &call["function"];
                    let name = function["name"].as_str().or_else(|| call["name"].as_str()).unwrap_or("tool");
                    found.push(ts, Event::Tool { name: name.to_string(), target: tool_target(&function["arguments"]) });
                }
            }
            _ => {}
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sessions_and_skips_replaced_messages() {
        let dir = std::env::temp_dir().join(format!("scrollback-hermes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        let _ = std::fs::remove_file(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "create table sessions (id text, source text, model text, model_config text, parent_session_id text,
                started_at real, ended_at real, title text, cwd text, git_branch text, input_tokens int,
                output_tokens int, cache_read_tokens int, cache_write_tokens int, estimated_cost_usd real);
             create table messages (id integer primary key, session_id text, role text, content text,
                tool_call_id text, tool_calls text, tool_name text, timestamp real, active int);
             insert into sessions values ('20250305_091523_a1b2c3d4', 'cli', 'hermes-4', null, null,
                1741165000, 1741165600, 'Tidy the parser', '/p/app', 'main', 1000, 200, 5000, 0, 0.12);
             insert into messages (session_id, role, content, tool_calls, timestamp, active) values
                ('20250305_091523_a1b2c3d4', 'user', 'Tidy the parser', null, 1741165001, 1),
                ('20250305_091523_a1b2c3d4', 'assistant', 'Old reply', null, 1741165002, 0),
                ('20250305_091523_a1b2c3d4', 'assistant', '', '[{\"id\":\"c\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"src/parse.rs\\\"}\"}}]', 1741165003, 1),
                ('20250305_091523_a1b2c3d4', 'assistant', 'Tidied.', null, 1741165004, 1);",
        )
        .unwrap();
        drop(db);
        let db = open(&path).unwrap();
        let found = messages(&db, "20250305_091523_a1b2c3d4").unwrap();
        let events = found.into_events();
        assert_eq!(events.len(), 3, "the replaced reply is left out");
        assert!(matches!(&events[1], Event::Tool { name, target } if name == "read_file" && target.as_deref() == Some("src/parse.rs")));
        let mut stmt = db.prepare("select * from sessions").unwrap();
        let meta: Vec<Meta> = stmt.query_map([], |row| Ok(Meta::from(row))).unwrap().flatten().collect();
        assert_eq!(meta[0].title.as_deref(), Some("Tidy the parser"));
        assert_eq!(meta[0].tokens_cached, 5000);
        assert!(!meta[0].subagent);
    }
}
