//! The local index: one SQLite file holding a row per session, plus the
//! modification time and size of every source file so unchanged files are
//! never parsed twice.

use crate::model::{Harness, Session};
use crate::sources::{claude_code, codex, opencode};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub struct Store {
    db: Connection,
}

/// What a refresh did.
#[derive(Debug, Default)]
pub struct Report {
    /// Source files read this time.
    pub parsed: usize,
    /// Source files skipped because they had not changed.
    pub unchanged: usize,
    /// Source files that disappeared since the last refresh.
    pub removed: usize,
    /// Problems that did not stop the refresh, such as an unreadable store.
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    mtime_ms: i64,
    size: i64,
}

/// Bump this when a parser changes what it extracts, so existing indexes
/// re-read every source file on the next refresh.
const PARSER_VERSION: i64 = 2;

const SCHEMA: &str = "
create table if not exists sessions (
    key text primary key,
    harness text not null,
    id text not null,
    title text not null,
    first_prompt text not null,
    cwd text not null,
    branch text,
    model text,
    started_ms integer not null,
    updated_ms integer not null,
    user_turns integer not null,
    assistant_turns integer not null,
    tool_calls integer not null,
    tokens_in integer not null,
    tokens_out integer not null,
    cost_usd real,
    source text not null,
    parent text,
    summary text
);
create index if not exists sessions_updated on sessions (updated_ms desc);
create index if not exists sessions_source on sessions (source);
create table if not exists files (
    path text primary key,
    mtime_ms integer not null,
    size integer not null
);
";

const UPSERT: &str = "
insert into sessions (key, harness, id, title, first_prompt, cwd, branch, model,
    started_ms, updated_ms, user_turns, assistant_turns, tool_calls,
    tokens_in, tokens_out, cost_usd, source, parent)
values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
on conflict (key) do update set
    title = excluded.title, first_prompt = excluded.first_prompt, cwd = excluded.cwd,
    branch = excluded.branch, model = excluded.model, started_ms = excluded.started_ms,
    updated_ms = excluded.updated_ms, user_turns = excluded.user_turns,
    assistant_turns = excluded.assistant_turns, tool_calls = excluded.tool_calls,
    tokens_in = excluded.tokens_in, tokens_out = excluded.tokens_out,
    cost_usd = excluded.cost_usd, source = excluded.source, parent = excluded.parent
";

impl Store {
    /// Open the index in scrollback's data folder, creating it if needed.
    pub fn open() -> Result<Store> {
        let dir = crate::data_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Store::open_at(&dir.join("index.db"))
    }

    pub fn open_at(path: &Path) -> Result<Store> {
        let db = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        db.pragma_update(None, "journal_mode", "wal")?;
        db.pragma_update(None, "synchronous", "normal")?;
        db.execute_batch(SCHEMA)?;
        let version: i64 = db.query_row("pragma user_version", [], |row| row.get(0))?;
        if version != PARSER_VERSION {
            db.execute("delete from files", [])?;
            db.pragma_update(None, "user_version", PARSER_VERSION)?;
        }
        Ok(Store { db })
    }

    /// Sessions, most recently active first. Sub-agent sessions are left out
    /// unless `include_children` is set.
    pub fn sessions(&self, include_children: bool) -> Result<Vec<Session>> {
        let filter = if include_children { "" } else { "where parent is null" };
        let mut stmt = self.db.prepare(&format!(
            "select harness, id, title, first_prompt, cwd, branch, model, started_ms,
                    updated_ms, user_turns, assistant_turns, tool_calls, tokens_in,
                    tokens_out, cost_usd, source, parent
             from sessions {filter} order by updated_ms desc"
        ))?;
        let rows = stmt.query_map([], |row| {
            let slug: String = row.get(0)?;
            Ok(Session {
                harness: Harness::from_slug(&slug).unwrap_or(Harness::ClaudeCode),
                id: row.get(1)?,
                title: row.get(2)?,
                first_prompt: row.get(3)?,
                cwd: row.get(4)?,
                branch: row.get(5)?,
                model: row.get(6)?,
                started_ms: row.get(7)?,
                updated_ms: row.get(8)?,
                user_turns: row.get(9)?,
                assistant_turns: row.get(10)?,
                tool_calls: row.get(11)?,
                tokens_in: row.get::<_, i64>(12)? as u64,
                tokens_out: row.get::<_, i64>(13)? as u64,
                cost_usd: row.get(14)?,
                source: row.get(15)?,
                parent: row.get(16)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Bring the index up to date with every harness on this machine.
    pub fn refresh(&mut self) -> Result<Report> {
        let started = Instant::now();
        let mut report = Report::default();

        let known = self.stamps()?;
        let names = codex::thread_names();

        // Decide which files need reading.
        let mut current: HashSet<String> = HashSet::new();
        let mut todo: Vec<(Harness, PathBuf, Stamp)> = Vec::new();
        let listed = claude_code::files()
            .into_iter()
            .map(|p| (Harness::ClaudeCode, p))
            .chain(codex::files().into_iter().map(|p| (Harness::Codex, p)));
        for (harness, path) in listed {
            let Some(stamp) = stamp_of(&path) else {
                continue;
            };
            let key = path.to_string_lossy().into_owned();
            if known.get(&key) == Some(&stamp) {
                report.unchanged += 1;
            } else {
                todo.push((harness, path, stamp));
            }
            current.insert(key);
        }

        // Parse them across all cores.
        let parsed = parse_all(&todo, &names);
        report.parsed = parsed.len();

        let opencode_sessions = match opencode::scan() {
            Ok(sessions) => sessions,
            Err(err) => {
                report.warnings.push(format!("OpenCode: {err:#}"));
                Vec::new()
            }
        };

        // Write everything in one transaction.
        let tx = self.db.transaction()?;
        {
            let mut upsert = tx.prepare(UPSERT)?;
            let mut set_stamp = tx.prepare(
                "insert into files (path, mtime_ms, size) values (?1, ?2, ?3)
                 on conflict (path) do update set mtime_ms = excluded.mtime_ms, size = excluded.size",
            )?;
            let mut drop_other = tx.prepare("delete from sessions where source = ?1 and key != ?2")?;

            for (path, stamp, outcome) in parsed {
                let source = path.to_string_lossy();
                match outcome {
                    Ok(Some(session)) => {
                        drop_other.execute(params![source, session.key()])?;
                        upsert.execute(rusqlite::params_from_iter(row_params(&session)))?;
                    }
                    Ok(None) => {
                        drop_other.execute(params![source, ""])?;
                    }
                    Err(err) => {
                        report.warnings.push(format!("{}: {err:#}", path.display()));
                        continue;
                    }
                }
                set_stamp.execute(params![source, stamp.mtime_ms, stamp.size])?;
            }

            // Files that were indexed before and are gone now.
            for path in known.keys().filter(|p| !current.contains(*p)) {
                tx.execute("delete from sessions where source = ?1", [path])?;
                tx.execute("delete from files where path = ?1", [path])?;
                report.removed += 1;
            }

            // Codex thread names can change without the rollout file changing.
            let mut retitle =
                tx.prepare("update sessions set title = ?1 where key = ?2 and title != ?1")?;
            for (id, name) in &names {
                retitle.execute(params![name, format!("{}:{id}", Harness::Codex.slug())])?;
            }

            // OpenCode is one database, so it is re-read whole each time.
            let keep: HashSet<String> = opencode_sessions.iter().map(Session::key).collect();
            for session in &opencode_sessions {
                upsert.execute(rusqlite::params_from_iter(row_params(session)))?;
            }
            let mut existing = tx.prepare("select key from sessions where harness = ?1")?;
            let stale: Vec<String> = existing
                .query_map([Harness::OpenCode.slug()], |row| row.get::<_, String>(0))?
                .filter_map(Result::ok)
                .filter(|key| !keep.contains(key))
                .collect();
            for key in stale {
                tx.execute("delete from sessions where key = ?1", [key])?;
            }
        }
        tx.commit()?;

        report.elapsed = started.elapsed();
        Ok(report)
    }

    fn stamps(&self) -> Result<HashMap<String, Stamp>> {
        let mut stmt = self.db.prepare("select path, mtime_ms, size from files")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Stamp {
                    mtime_ms: row.get(1)?,
                    size: row.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }
}

type Parsed = (PathBuf, Stamp, Result<Option<Session>>);

fn parse_all(todo: &[(Harness, PathBuf, Stamp)], names: &HashMap<String, String>) -> Vec<Parsed> {
    if todo.is_empty() {
        return Vec::new();
    }
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(todo.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out = std::sync::Mutex::new(Vec::with_capacity(todo.len()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((harness, path, stamp)) = todo.get(i) else {
                    break;
                };
                let outcome = match harness {
                    Harness::ClaudeCode => claude_code::parse(path),
                    Harness::Codex => codex::parse(path, names),
                    Harness::OpenCode | Harness::Pi => Ok(None),
                };
                out.lock().unwrap().push((path.clone(), *stamp, outcome));
            });
        }
    });
    out.into_inner().unwrap()
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(Stamp {
        mtime_ms: mtime.as_millis() as i64,
        size: meta.len() as i64,
    })
}

fn row_params(s: &Session) -> Vec<Box<dyn rusqlite::ToSql>> {
    vec![
        Box::new(s.key()),
        Box::new(s.harness.slug()),
        Box::new(s.id.clone()),
        Box::new(s.title.clone()),
        Box::new(s.first_prompt.clone()),
        Box::new(s.cwd.clone()),
        Box::new(s.branch.clone()),
        Box::new(s.model.clone()),
        Box::new(s.started_ms),
        Box::new(s.updated_ms),
        Box::new(s.user_turns),
        Box::new(s.assistant_turns),
        Box::new(s.tool_calls),
        Box::new(s.tokens_in as i64),
        Box::new(s.tokens_out as i64),
        Box::new(s.cost_usd),
        Box::new(s.source.clone()),
        Box::new(s.parent.clone()),
    ]
}
