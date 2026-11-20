//! The local index: one SQLite file holding a row per session, plus the
//! modification time and size of every source file so unchanged files are
//! never parsed twice.

use crate::model::{Harness, Origin, Session, Summary};
use crate::sources::{claude_code, codex, cursor, opencode, pi, t3code};
use crate::transcript::{self, Block};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub struct Store {
    db: Connection,
}

/// The paragraph at the top of the activity page.
#[derive(Debug, Clone, PartialEq)]
pub struct Digest {
    pub text: String,
    /// `Activity::fingerprint` of the period it was written from.
    pub fingerprint: u64,
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
    /// Sessions whose message text was (re)indexed for search this time.
    pub messages_indexed: usize,
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
const PARSER_VERSION: i64 = 6;

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
create table if not exists digests (
    scope text primary key,
    fingerprint text not null,
    text text not null,
    written_ms integer not null
);
";

const UPSERT: &str = "
insert into sessions (key, harness, id, title, first_prompt, cwd, branch, model,
    started_ms, updated_ms, user_turns, assistant_turns, tool_calls,
    tokens_in, tokens_out, cost_usd, source, parent, active_ms, tokens_cached, origin)
values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
on conflict (key) do update set
    title = excluded.title, first_prompt = excluded.first_prompt, cwd = excluded.cwd,
    branch = excluded.branch, model = excluded.model, started_ms = excluded.started_ms,
    updated_ms = excluded.updated_ms, user_turns = excluded.user_turns,
    assistant_turns = excluded.assistant_turns, tool_calls = excluded.tool_calls,
    tokens_in = excluded.tokens_in, tokens_out = excluded.tokens_out,
    cost_usd = excluded.cost_usd, source = excluded.source, parent = excluded.parent,
    active_ms = excluded.active_ms, tokens_cached = excluded.tokens_cached,
    origin = excluded.origin
";

impl Store {
    /// Open the index in Scrollback's data folder, creating it if needed.
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
        // Added after the first release of the schema.
        let has_summary_time = db
            .prepare("select 1 from pragma_table_info('sessions') where name = 'summary_for_ms'")?
            .exists([])?;
        if !has_summary_time {
            db.execute("alter table sessions add column summary_for_ms integer", [])?;
        }
        // Message text, searchable by substring. The trigram tokenizer matches
        // any run of three or more characters, in any case.
        db.execute_batch(
            "create virtual table if not exists messages using fts5(
                 text, key unindexed, block unindexed, role unindexed,
                 tokenize = 'trigram'
             )",
        )?;
        let has_messages_time = db
            .prepare("select 1 from pragma_table_info('sessions') where name = 'messages_for_ms'")?
            .exists([])?;
        if !has_messages_time {
            db.execute("alter table sessions add column messages_for_ms integer", [])?;
        }
        let has_active = db
            .prepare("select 1 from pragma_table_info('sessions') where name = 'active_ms'")?
            .exists([])?;
        if !has_active {
            db.execute(
                "alter table sessions add column active_ms integer not null default 0",
                [],
            )?;
        }
        let has_cached = db
            .prepare("select 1 from pragma_table_info('sessions') where name = 'tokens_cached'")?
            .exists([])?;
        if !has_cached {
            db.execute(
                "alter table sessions add column tokens_cached integer not null default 0",
                [],
            )?;
        }
        // `origin` is what the session's own files say about where it was
        // run; `via` and `via_title` are set when another app, such as
        // T3 Code, says it started the session.
        for (column, kind) in [("origin", "text"), ("via", "text"), ("via_title", "text")] {
            let present = db
                .prepare("select 1 from pragma_table_info('sessions') where name = ?1")?
                .exists([column])?;
            if !present {
                db.execute(&format!("alter table sessions add column {column} {kind}"), [])?;
            }
        }
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
                    tokens_out, cost_usd, source, parent, summary, summary_for_ms, active_ms,
                    tokens_cached, origin, via, via_title
             from sessions {filter} order by updated_ms desc"
        ))?;
        let rows = stmt.query_map([], |row| {
            let slug: String = row.get(0)?;
            let updated_ms: i64 = row.get(8)?;
            let summary: Option<Summary> = row
                .get::<_, Option<String>>(17)?
                .and_then(|json| serde_json::from_str(&json).ok());
            let summary_for_ms: Option<i64> = row.get(18)?;
            let harness = Harness::from_slug(&slug).unwrap_or(Harness::ClaudeCode);
            let slug_of = |column: usize| -> rusqlite::Result<Option<Origin>> {
                Ok(row
                    .get::<_, Option<String>>(column)?
                    .and_then(|slug| Origin::from_slug(&slug)))
            };
            // An app that says it started the session wins over the files.
            let origin = slug_of(22)?.or(slug_of(21)?).unwrap_or(harness.origin());
            let own_title: String = row.get(2)?;
            let given_title = row
                .get::<_, Option<String>>(23)?
                .filter(|title| !title.trim().is_empty());
            Ok(Session {
                harness,
                origin,
                id: row.get(1)?,
                title: given_title.unwrap_or(own_title),
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
                tokens_cached: row.get::<_, i64>(20)? as u64,
                cost_usd: row.get(14)?,
                source: row.get(15)?,
                parent: row.get(16)?,
                active_ms: row.get(19)?,
                summary_stale: summary.is_some() && summary_for_ms.unwrap_or(0) < updated_ms,
                summary_for_ms: summary.as_ref().and(summary_for_ms),
                summary,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Save a summary, recording which state of the session it describes.
    pub fn set_summary(&self, session: &Session, summary: &Summary) -> Result<()> {
        self.db.execute(
            "update sessions set summary = ?1, summary_for_ms = ?2 where key = ?3",
            params![serde_json::to_string(summary)?, session.updated_ms, session.key()],
        )?;
        Ok(())
    }

    /// The paragraph last written for an activity period. `scope` names the
    /// period and filters it covers.
    pub fn digest(&self, scope: &str) -> Result<Option<Digest>> {
        let found = self
            .db
            .query_row(
                "select text, fingerprint from digests where scope = ?1",
                params![scope],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(found.map(|(text, fingerprint)| Digest {
            text,
            fingerprint: fingerprint.parse().unwrap_or(0),
        }))
    }

    /// Save an activity digest, with the fingerprint of what it was written from.
    pub fn set_digest(&self, scope: &str, digest: &Digest) -> Result<()> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        self.db.execute(
            "insert into digests (scope, fingerprint, text, written_ms) values (?1, ?2, ?3, ?4)
             on conflict (scope) do update set fingerprint = excluded.fingerprint,
                 text = excluded.text, written_ms = excluded.written_ms",
            params![scope, digest.fingerprint.to_string(), digest.text, now_ms],
        )?;
        Ok(())
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
            .chain(codex::files().into_iter().map(|p| (Harness::Codex, p)))
            .chain(pi::files().into_iter().map(|p| (Harness::Pi, p)));
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
        // Cursor's store is one large file, so it is only read again when it
        // (or its write-ahead log) has changed. `None` means "leave as is".
        let cursor_key = cursor::db_path().to_string_lossy().into_owned();
        let cursor_stamp = stamp_of(&cursor::db_path()).map(|main| {
            let log = stamp_of(Path::new(&format!("{cursor_key}-wal")));
            Stamp {
                mtime_ms: main.mtime_ms.max(log.map_or(0, |l| l.mtime_ms)),
                size: main.size + log.map_or(0, |l| l.size),
            }
        });
        let cursor_sessions = match cursor_stamp {
            Some(stamp) if known.get(&cursor_key) == Some(&stamp) => {
                current.insert(cursor_key.clone());
                None
            }
            Some(_) => match cursor::scan() {
                Ok(sessions) => {
                    current.insert(cursor_key.clone());
                    Some(sessions)
                }
                Err(err) => {
                    report.warnings.push(format!("Cursor: {err:#}"));
                    current.insert(cursor_key.clone());
                    None
                }
            },
            None => None,
        };
        let started_elsewhere = t3code::bindings();

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

            // Cursor, when its store changed: the same, for its conversations.
            if let (Some(sessions), Some(stamp)) = (&cursor_sessions, cursor_stamp) {
                let keep: HashSet<String> = sessions.iter().map(Session::key).collect();
                for session in sessions {
                    upsert.execute(rusqlite::params_from_iter(row_params(session)))?;
                }
                let stale: Vec<String> = existing
                    .query_map([Harness::Cursor.slug()], |row| row.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .filter(|key| !keep.contains(key))
                    .collect();
                for key in stale {
                    tx.execute("delete from sessions where key = ?1", [key])?;
                }
                set_stamp.execute(params![cursor_key, stamp.mtime_ms, stamp.size])?;
            }

            // Sessions another app says it started. Worked out afresh each
            // time, since a thread can be deleted in that app.
            tx.execute(
                "update sessions set via = null, via_title = null where via is not null",
                [],
            )?;
            let mut mark = tx.prepare(
                "update sessions set via = ?1, via_title = ?2 where id = ?3 and parent is null",
            )?;
            for binding in &started_elsewhere {
                mark.execute(params![Origin::T3Code.slug(), binding.title, binding.native_id])?;
            }
        }
        tx.commit()?;

        self.index_messages(&mut report)?;
        report.elapsed = started.elapsed();
        Ok(report)
    }

    /// Bring the message index up to date: re-read the text of every session
    /// that has changed since its messages were last indexed.
    fn index_messages(&mut self, report: &mut Report) -> Result<()> {
        let stale_keys: HashSet<String> = {
            let mut stmt = self.db.prepare(
                "select key from sessions
                 where parent is null
                   and (messages_for_ms is null or messages_for_ms != updated_ms)",
            )?;
            let keys = stmt.query_map([], |row| row.get::<_, String>(0))?;
            keys.collect::<rusqlite::Result<_>>()?
        };
        let stale: Vec<Session> = if stale_keys.is_empty() {
            Vec::new()
        } else {
            self.sessions(false)?
                .into_iter()
                .filter(|s| stale_keys.contains(&s.key()))
                .collect()
        };

        let read = read_all_messages(&stale);
        let tx = self.db.transaction()?;
        {
            let mut clear = tx.prepare("delete from messages where key = ?1")?;
            let mut add =
                tx.prepare("insert into messages (text, key, block, role) values (?1, ?2, ?3, ?4)")?;
            let mut done =
                tx.prepare("update sessions set messages_for_ms = updated_ms where key = ?1")?;
            for (i, outcome) in read {
                let session = &stale[i];
                let key = session.key();
                match outcome {
                    Ok(texts) => {
                        clear.execute([&key])?;
                        for (block, role, text) in texts {
                            add.execute(params![text, key, block as i64, role])?;
                        }
                        done.execute([&key])?;
                        report.messages_indexed += 1;
                    }
                    Err(err) => report
                        .warnings
                        .push(format!("{}: {err:#}", session.display_title())),
                }
            }
            // Messages of sessions that no longer exist.
            tx.execute(
                "delete from messages where key not in (select key from sessions)",
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Messages containing every word of `query` (words of three or more
    /// characters; shorter ones are ignored), best matches first.
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageHit>> {
        let terms: Vec<String> = query
            .split_whitespace()
            .filter(|term| term.chars().count() >= 3)
            .map(str::to_lowercase)
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let expression = terms
            .iter()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut stmt = self.db.prepare(
            "select key, block, role, text from messages
             where messages match ?1 order by rank limit ?2",
        )?;
        let rows = stmt.query_map(params![expression, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (key, block, role, text) = row?;
            let (snippet, matches) = snippet(&text, &terms);
            hits.push(MessageHit {
                key,
                block: block as usize,
                from_user: role == "you",
                snippet,
                matches,
            });
        }
        Ok(hits)
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

/// One message that matched a search.
pub struct MessageHit {
    /// The key of the session it belongs to.
    pub key: String,
    /// Its position among the session's transcript blocks.
    pub block: usize,
    pub from_user: bool,
    /// A short stretch of the message around the first match.
    pub snippet: String,
    /// Byte ranges in `snippet` that matched the query.
    pub matches: Vec<Range<usize>>,
}

/// Cut a window of `text` around the first match of any term, and report
/// where the terms appear inside it.
fn snippet(text: &str, terms: &[String]) -> (String, Vec<Range<usize>>) {
    const BEFORE: usize = 60;
    const LENGTH: usize = 220;
    // Work in characters, with whitespace flattened one-for-one so positions
    // in the lowercased copy line up with the original.
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let terms: Vec<Vec<char>> = terms.iter().map(|t| t.chars().collect()).collect();
    let find_all = |from: usize, to: usize| -> Vec<Range<usize>> {
        let mut found = Vec::new();
        for term in &terms {
            if term.is_empty() || term.len() > to.saturating_sub(from) {
                continue;
            }
            let mut i = from;
            while i + term.len() <= to {
                if lower[i..i + term.len()] == term[..] {
                    found.push(i..i + term.len());
                    i += term.len();
                } else {
                    i += 1;
                }
            }
        }
        found.sort_by_key(|r| r.start);
        found
    };

    let first = find_all(0, chars.len()).first().map_or(0, |r| r.start);
    let mut start = first.saturating_sub(BEFORE);
    // Begin on a word boundary when there is one nearby.
    if start > 0 {
        if let Some(space) = chars[start..first].iter().position(|c| *c == ' ') {
            start += space + 1;
        }
    }
    let end = (start + LENGTH).min(chars.len());

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let found = find_all(start, end);
    let mut next = found.iter().peekable();
    let mut open: Option<usize> = None;
    for (i, ch) in chars[start..end].iter().enumerate() {
        let at = start + i;
        while next.peek().is_some_and(|r| r.end <= at) {
            next.next();
        }
        let inside = next.peek().is_some_and(|r| r.start <= at && at < r.end);
        match (inside, open) {
            (true, None) => open = Some(out.len()),
            (false, Some(began)) => {
                ranges.push(began..out.len());
                open = None;
            }
            _ => {}
        }
        out.push(*ch);
    }
    if let Some(began) = open {
        ranges.push(began..out.len());
    }
    if end < chars.len() {
        out.push('…');
    }
    (out, ranges)
}

type MessageText = (usize, &'static str, String);

/// Read the prompts and replies of each session, across all cores. Returns
/// the session's position in `sessions` with its texts.
fn read_all_messages(sessions: &[Session]) -> Vec<(usize, Result<Vec<MessageText>>)> {
    const LIMIT: usize = 20_000;
    if sessions.is_empty() {
        return Vec::new();
    }
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(sessions.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out = std::sync::Mutex::new(Vec::with_capacity(sessions.len()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(session) = sessions.get(i) else { break };
                let texts = transcript::events(session).map(|events| {
                    transcript::blocks(events)
                        .into_iter()
                        .enumerate()
                        .filter_map(|(block, content)| match content {
                            Block::Prompt(text) => Some((block, "you", text)),
                            Block::Reply(text) => Some((block, "agent", text)),
                            _ => None,
                        })
                        .map(|(block, role, text)| {
                            let capped: String = text.chars().take(LIMIT).collect();
                            (block, role, capped)
                        })
                        .collect()
                });
                out.lock().unwrap().push((i, texts));
            });
        }
    });
    out.into_inner().unwrap()
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
                    Harness::Pi => pi::parse(path),
                    // Database-backed; read whole by their own `scan`.
                    Harness::OpenCode | Harness::Cursor => Ok(None),
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
        Box::new(s.active_ms),
        Box::new(s.tokens_cached as i64),
        Box::new(s.origin.slug()),
    ]
}
