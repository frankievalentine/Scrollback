//! T3 Code: a desktop app that drives other harnesses (Codex, Claude Code and
//! more) rather than keeping conversations of its own. The harness still
//! writes its usual session files, so those sessions are already read by
//! their own readers. What T3 Code's database adds is which of them were
//! started from it, and the title it gave each thread.
//!
//! The database is `userdata/statev2.sqlite` under `~/.t3` (or
//! `T3CODE_HOME`); the desktop app keeps the same file under Application
//! Support. It is opened read-only.

use crate::home;
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::PathBuf;

/// A harness session that was started from T3 Code.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// The harness's own session id.
    pub native_id: String,
    /// T3 Code's title for the thread.
    pub title: String,
}

/// T3 Code's default data folder, for showing where it was looked for.
pub fn root() -> PathBuf {
    std::env::var_os("T3CODE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".t3"))
}

/// Every database file T3 Code may have written on this machine.
pub fn databases() -> Vec<PathBuf> {
    let mut folders = vec![root().join("userdata")];
    // The desktop app's own folder, whatever release channel named it.
    let support = home().join("Library/Application Support");
    if let Ok(entries) = std::fs::read_dir(&support) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("T3 Code") {
                folders.push(entry.path());
                folders.push(entry.path().join("userdata"));
            }
        }
    }
    folders
        .into_iter()
        .flat_map(|folder| [folder.join("statev2.sqlite"), folder.join("state.sqlite")])
        .filter(|path| path.is_file())
        .collect()
}

/// Whether T3 Code has left anything to read.
pub fn present() -> bool {
    !databases().is_empty()
}

/// Sessions started from T3 Code, across every database found.
pub fn bindings() -> Vec<Binding> {
    let mut all = Vec::new();
    for path in databases() {
        if let Ok(found) = bindings_at(&path) {
            all.extend(found);
        }
    }
    all
}

fn bindings_at(path: &std::path::Path) -> Result<Vec<Binding>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let has = |table: &str| -> bool {
        db.prepare("select 1 from sqlite_master where type = 'table' and name = ?1")
            .and_then(|mut stmt| stmt.exists([table]))
            .unwrap_or(false)
    };

    let mut out = Vec::new();
    // Current layout: each provider thread records the harness's own id.
    if has("orchestration_v2_projection_provider_threads") && has("orchestration_v2_projection_threads") {
        let mut stmt = db.prepare(
            "select pt.payload_json, t.title
             from orchestration_v2_projection_provider_threads pt
             join orchestration_v2_projection_threads t on t.thread_id = pt.thread_id
             where t.deleted_at is null",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        for row in rows {
            let (payload, title) = row?;
            let native = serde_json::from_str::<Value>(&payload)
                .ok()
                .and_then(|p| p["nativeThreadRef"]["nativeId"].as_str().map(str::to_string));
            if let Some(native_id) = native.filter(|id| !id.is_empty()) {
                out.push(Binding { native_id, title });
            }
        }
    }
    // Earlier layout: the id is somewhere inside each session's resume cursor.
    if has("provider_session_runtime") && has("projection_threads") {
        let mut stmt = db.prepare(
            "select r.resume_cursor_json, t.title
             from provider_session_runtime r
             join projection_threads t on t.thread_id = r.thread_id
             where t.deleted_at is null and r.resume_cursor_json is not null",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        for row in rows {
            let (cursor, title) = row?;
            let Ok(cursor) = serde_json::from_str::<Value>(&cursor) else { continue };
            let mut ids = Vec::new();
            strings(&cursor, &mut ids);
            for native_id in ids {
                out.push(Binding { native_id, title: title.clone() });
            }
        }
    }
    Ok(out)
}

/// Every string inside a JSON value that could be a session id.
fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) if text.len() >= 8 && !text.contains(char::is_whitespace) => {
            out.push(text.clone())
        }
        Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
        Value::Object(fields) => fields.values().for_each(|field| strings(field, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(test: &str, sql: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scrollback-t3-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("statev2.sqlite");
        Connection::open(&path).unwrap().execute_batch(sql).unwrap();
        path
    }

    #[test]
    fn reads_bindings_from_the_current_layout() {
        // The table definitions are T3 Code's own, from its migrations.
        let path = database(
            "v2",
            r#"
            CREATE TABLE orchestration_v2_projection_threads (
              thread_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL,
              default_provider TEXT NOT NULL, runtime_mode TEXT NOT NULL, interaction_mode TEXT NOT NULL,
              active_provider_thread_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
              archived_at TEXT, deleted_at TEXT, payload_json TEXT NOT NULL);
            CREATE TABLE orchestration_v2_projection_provider_threads (
              provider_thread_id TEXT PRIMARY KEY, thread_id TEXT, owner_node_id TEXT, provider TEXT NOT NULL,
              provider_session_id TEXT, status TEXT NOT NULL, first_run_ordinal INTEGER, last_run_ordinal INTEGER,
              updated_at TEXT NOT NULL, payload_json TEXT NOT NULL);
            INSERT INTO orchestration_v2_projection_threads VALUES
              ('t1','p1','Fix the flaky test','codex','full','default',NULL,'2026-11-01','2026-11-01',NULL,NULL,'{}'),
              ('t2','p1','Deleted thread','codex','full','default',NULL,'2026-11-01','2026-11-01',NULL,'2026-11-02','{}'),
              ('t3','p1','Not started yet','claudeAgent','full','default',NULL,'2026-11-01','2026-11-01',NULL,NULL,'{}');
            INSERT INTO orchestration_v2_projection_provider_threads VALUES
              ('pt1','t1',NULL,'codex',NULL,'idle',1,1,'2026-11-01',
               '{"id":"pt1","driver":"codex","nativeThreadRef":{"driver":"codex","nativeId":"019db192-a1f4-7bd1-81c6-fa04fcde0a35","strength":"strong"}}'),
              ('pt2','t2',NULL,'codex',NULL,'idle',1,1,'2026-11-01',
               '{"id":"pt2","driver":"codex","nativeThreadRef":{"driver":"codex","nativeId":"deleted-thread-id","strength":"strong"}}'),
              ('pt3','t3',NULL,'claudeAgent',NULL,'not_loaded',NULL,NULL,'2026-11-01',
               '{"id":"pt3","driver":"claudeAgent","nativeThreadRef":null}');
            "#,
        );
        assert_eq!(
            bindings_at(&path).unwrap(),
            [Binding {
                native_id: "019db192-a1f4-7bd1-81c6-fa04fcde0a35".to_string(),
                title: "Fix the flaky test".to_string(),
            }]
        );
    }

    #[test]
    fn reads_bindings_from_the_earlier_layout() {
        let path = database(
            "v1",
            r#"
            CREATE TABLE projection_threads (thread_id TEXT PRIMARY KEY, title TEXT NOT NULL, deleted_at TEXT);
            CREATE TABLE provider_session_runtime (thread_id TEXT PRIMARY KEY, provider_name TEXT NOT NULL, resume_cursor_json TEXT);
            INSERT INTO projection_threads VALUES ('t1', 'Port the selector', NULL);
            INSERT INTO provider_session_runtime VALUES ('t1', 'codex', '{"threadId":"019e6d09-aaaa-bbbb-cccc-000000000001","n":3}');
            "#,
        );
        assert_eq!(
            bindings_at(&path).unwrap(),
            [Binding {
                native_id: "019e6d09-aaaa-bbbb-cccc-000000000001".to_string(),
                title: "Port the selector".to_string(),
            }]
        );
    }

    #[test]
    fn a_database_with_neither_layout_gives_nothing() {
        let path = database("empty", "CREATE TABLE unrelated (x TEXT);");
        assert!(bindings_at(&path).unwrap().is_empty());
    }
}
