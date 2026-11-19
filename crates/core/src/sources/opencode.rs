//! OpenCode: a SQLite database at `~/.local/share/opencode/opencode.db`.
//! It is opened read-only; OpenCode may have it open at the same time.

use crate::model::{Harness, Session};
use crate::sources::typed_prompt;
use crate::{home, squash};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn db_path() -> PathBuf {
    home().join(".local/share/opencode/opencode.db")
}

/// Read every session. Returns an empty list when OpenCode is not installed.
pub fn scan() -> Result<Vec<Session>> {
    let path = db_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let source = path.to_string_lossy().into_owned();

    let mut sessions: Vec<Session> = Vec::new();
    {
        let mut stmt = db.prepare(
            "select id, parent_id, directory, title, time_created, time_updated,
                    model, cost, tokens_input, tokens_output
             from session",
        )?;
        let rows = stmt.query_map([], |row| {
            let mut s = Session::new(Harness::OpenCode, row.get::<_, String>(0)?, source.clone());
            s.parent = row.get(1)?;
            s.cwd = row.get(2)?;
            s.title = row.get(3)?;
            s.started_ms = row.get(4)?;
            s.updated_ms = row.get(5)?;
            s.model = row.get::<_, Option<String>>(6)?.and_then(|m| model_id(&m));
            let cost: f64 = row.get(7)?;
            s.cost_usd = (cost > 0.0).then_some(cost);
            s.tokens_in = row.get::<_, i64>(8)?.max(0) as u64;
            s.tokens_out = row.get::<_, i64>(9)?.max(0) as u64;
            Ok(s)
        })?;
        for row in rows {
            sessions.push(row?);
        }
    }

    // Turn counts per session and role.
    let mut turns: HashMap<(String, String), u32> = HashMap::new();
    {
        let mut stmt = db.prepare(
            "select session_id, json_extract(data, '$.role'), count(*)
             from message group by 1, 2",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, u32>(2)?,
            ))
        })?;
        for row in rows {
            let (session, role, count) = row?;
            turns.insert((session, role), count);
        }
    }

    let mut tools: HashMap<String, u32> = HashMap::new();
    {
        let mut stmt = db.prepare(
            "select session_id, count(*) from part
             where json_extract(data, '$.type') = 'tool' group by 1",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)))?;
        for row in rows {
            let (session, count) = row?;
            tools.insert(session, count);
        }
    }

    // The first typed prompt of each session.
    let mut prompts: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = db.prepare(
            "select p.session_id, json_extract(p.data, '$.text')
             from part p join message m on m.id = p.message_id
             where json_extract(m.data, '$.role') = 'user'
               and json_extract(p.data, '$.type') = 'text'
             order by p.time_created, p.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (session, text) = row?;
            let Some(text) = text else { continue };
            if let Some(typed) = typed_prompt(&text) {
                prompts.entry(session).or_insert_with(|| squash(typed, 600));
            }
        }
    }

    for s in &mut sessions {
        s.user_turns = turns.get(&(s.id.clone(), "user".into())).copied().unwrap_or(0);
        s.assistant_turns = turns
            .get(&(s.id.clone(), "assistant".into()))
            .copied()
            .unwrap_or(0);
        s.tool_calls = tools.get(&s.id).copied().unwrap_or(0);
        s.first_prompt = prompts.remove(&s.id).unwrap_or_default();
        if s.title.trim().is_empty() {
            s.title = squash(&s.first_prompt, 90);
        }
    }
    sessions.retain(|s| s.user_turns > 0 || s.assistant_turns > 0);
    Ok(sessions)
}

/// The session's `model` column is JSON such as `{"id": "...", "providerID": "..."}`.
fn model_id(raw: &str) -> Option<String> {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => value
            .get("id")
            .or_else(|| value.get("modelID"))
            .and_then(Value::as_str)
            .map(str::to_string),
        Err(_) => (!raw.is_empty()).then(|| raw.to_string()),
    }
}
