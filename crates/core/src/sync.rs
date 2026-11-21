//! Sessions from your other machines, through a folder they all keep in
//! sync: iCloud Drive on macOS, or any synced folder (OneDrive, Dropbox,
//! Syncthing) elsewhere.
//!
//! Each machine writes only its own files there, and reads everyone
//! else's. Nothing in the folder is a database and no two machines write to
//! the same file, which is what makes it safe on iCloud.
//!
//! - `<machine-id>.scrollback.json`: the machine's name and its sessions
//!   with their summaries. Rewritten when they change, and every few
//!   minutes while the app runs, so other machines can tell it is awake.
//! - `<machine-id>.messages/`: with "Include full messages" on, one file per
//!   session holding its readable transcript (prompts, replies, tool names),
//!   rewritten only when that session changes.

use crate::config::Config;
use crate::model::{Harness, Origin, Session, Summary};
use crate::transcript::Event;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: u32 = 1;
/// How often a running machine rewrites its file even when nothing
/// changed, so the others know it is awake.
const HEARTBEAT_MS: i64 = 5 * 60 * 1000;
/// A machine whose file is older than this is shown as asleep.
pub const AWAY_MS: i64 = 15 * 60 * 1000;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// A machine sessions come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MachineRef {
    pub id: String,
    pub name: String,
    /// `macos`, `windows` or `linux`, so commands for it suit its shell.
    #[serde(default)]
    pub os: String,
    /// For a machine reached over Tailscale, its address there; `None` for
    /// one read from the synced folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Another machine found in the synced folder.
#[derive(Debug, Clone)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub os: String,
    /// When it last wrote its file.
    pub written_ms: i64,
    pub sessions: usize,
    /// Whether its full messages are in the folder.
    pub has_messages: bool,
    /// Its Tailscale address, for a machine reached that way.
    pub address: Option<String>,
    /// For a machine on Tailscale: it did not answer the last time it was
    /// asked.
    pub unreachable: bool,
}

impl Machine {
    /// Whether it is out of touch: on Tailscale, not answering; in the
    /// synced folder, not written for a while.
    pub fn away(&self) -> bool {
        self.unreachable || now_ms() - self.written_ms > AWAY_MS
    }
}

/// This machine: a stable id kept beside the index, and its name.
pub fn this_machine() -> MachineRef {
    MachineRef { id: machine_id(), name: machine_name(), os: std::env::consts::OS.to_string(), address: None }
}

fn machine_id() -> String {
    let path = crate::data_dir().join("machine-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if id.len() >= 8 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return id.to_string();
        }
    }
    // Random enough to tell a handful of machines apart; std's hasher seeds
    // itself randomly for each process.
    let state = std::collections::hash_map::RandomState::new();
    let mut hasher = state.build_hasher();
    now_ms().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let id = format!("{:016x}", hasher.finish());
    let _ = std::fs::create_dir_all(crate::data_dir());
    let _ = std::fs::write(&path, &id);
    id
}

/// The machine's network name, without a `.local` ending.
pub fn machine_name() -> String {
    // Development aid: pose as another machine.
    if let Ok(name) = std::env::var("SCROLLBACK_MACHINE_NAME") {
        return name;
    }
    let raw = host_name().unwrap_or_default();
    let name = raw.trim().trim_end_matches(".local").trim_end_matches(".lan").to_string();
    if name.is_empty() {
        "this computer".to_string()
    } else {
        name
    }
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its length, and gethostname writes at
    // most that many bytes.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8(buffer[..end].to_vec()).ok()
}

#[cfg(windows)]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

/// The iCloud Drive folder, on macOS when iCloud Drive is on.
pub fn icloud_folder() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let drive = crate::home().join("Library/Mobile Documents/com~apple~CloudDocs");
    drive.is_dir().then(|| drive.join("Scrollback"))
}

/// The folder to sync through, when syncing is on: the one chosen, else
/// iCloud Drive on macOS.
pub fn folder(config: &Config) -> Option<PathBuf> {
    if !config.sync_enabled {
        return None;
    }
    match config.sync_folder.trim() {
        "" => icloud_folder(),
        chosen => Some(PathBuf::from(chosen)),
    }
}

/// One session as it travels between machines.
#[derive(Serialize, Deserialize)]
pub(crate) struct Shared {
    harness: String,
    origin: String,
    id: String,
    title: String,
    first_prompt: String,
    cwd: String,
    branch: Option<String>,
    model: Option<String>,
    started_ms: i64,
    updated_ms: i64,
    active_ms: i64,
    #[serde(default)]
    active_days: BTreeMap<String, i64>,
    user_turns: u32,
    assistant_turns: u32,
    tool_calls: u32,
    tokens_in: u64,
    tokens_out: u64,
    tokens_cached: u64,
    summary: Option<Summary>,
    #[serde(default)]
    summary_stale: bool,
    /// The activity the summary covers.
    #[serde(default)]
    summary_for_ms: Option<i64>,
    /// The session's file on its own machine.
    #[serde(default)]
    source: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub v: u32,
    pub machine: MachineRef,
    pub written_ms: i64,
    #[serde(default)]
    pub has_messages: bool,
    pub sessions: Vec<Shared>,
}

/// What this machine last wrote, kept beside the index.
#[derive(Serialize, Deserialize, Default)]
struct State {
    /// A hash of the sessions last written, to tell whether they changed.
    content: u64,
    written_ms: i64,
    folder: String,
    /// For each session whose messages are in the folder, the activity
    /// they cover.
    #[serde(default)]
    messages: HashMap<String, i64>,
}

fn state_path() -> PathBuf {
    crate::data_dir().join("sync-state.json")
}

fn load_state() -> State {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_state(state: &State) {
    if let Ok(text) = serde_json::to_string(state) {
        let _ = std::fs::write(state_path(), text);
    }
}

/// Write a file so that a reader never sees it half written.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    let folder = path.parent().context("a file with no folder")?;
    std::fs::create_dir_all(folder)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let partial = folder.join(format!(".{name}.partial"));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}

/// A session's messages file name: its key, made safe for any system.
fn messages_name(key: &str) -> String {
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    format!("{safe}.json")
}

/// This machine's own top-level sessions, as they travel.
fn share(sessions: &[Session]) -> Vec<Shared> {
    sessions
        .iter()
        .filter(|s| s.machine.is_none() && s.parent.is_none())
        .map(|s| Shared {
            harness: s.harness.slug().to_string(),
            origin: s.origin.slug().to_string(),
            id: s.id.clone(),
            title: s.title.clone(),
            first_prompt: s.first_prompt.clone(),
            cwd: s.cwd.clone(),
            branch: s.branch.clone(),
            model: s.model.clone(),
            started_ms: s.started_ms,
            updated_ms: s.updated_ms,
            active_ms: s.active_ms,
            active_days: s.active_days.clone(),
            user_turns: s.user_turns,
            assistant_turns: s.assistant_turns,
            tool_calls: s.tool_calls,
            tokens_in: s.tokens_in,
            tokens_out: s.tokens_out,
            tokens_cached: s.tokens_cached,
            summary: s.summary.clone(),
            summary_stale: s.summary_stale,
            summary_for_ms: s.summary_for_ms,
            source: s.source.clone(),
        })
        .collect()
}

/// This machine's sessions as a snapshot, for answering over Tailscale,
/// where messages are always to be had by asking.
pub(crate) fn snapshot_now(sessions: &[Session]) -> Snapshot {
    Snapshot { v: VERSION, machine: this_machine(), written_ms: now_ms(), has_messages: true, sessions: share(sessions) }
}

/// A session's messages in the compact form they travel in.
pub(crate) fn compact_events(session: &Session) -> Result<Vec<[String; 3]>> {
    Ok(crate::transcript::events(session)?.into_iter().map(compact_event).collect())
}

/// A machine and its sessions, from its snapshot. `address` is set for a
/// machine reached over Tailscale.
pub(crate) fn unpack(snapshot: Snapshot, address: Option<&str>, config: &Config) -> (Machine, Vec<Session>) {
    let mut machine = snapshot.machine.clone();
    machine.address = address.map(str::to_string);
    let found = Machine {
        id: machine.id.clone(),
        name: machine.name.clone(),
        os: machine.os.clone(),
        written_ms: snapshot.written_ms,
        sessions: snapshot.sessions.len(),
        has_messages: snapshot.has_messages,
        address: machine.address.clone(),
        unreachable: false,
    };
    if config.hidden_machines.contains(&machine.id) {
        return (found, Vec::new());
    }
    let mut sessions = Vec::new();
    for shared in snapshot.sessions {
        let Some(harness) = Harness::from_slug(&shared.harness) else { continue };
        let mut session = Session::new(harness, shared.id, "");
        session.origin = Origin::from_slug(&shared.origin).unwrap_or(harness.origin());
        session.title = shared.title;
        session.first_prompt = shared.first_prompt;
        session.cwd = shared.cwd;
        session.branch = shared.branch;
        session.model = shared.model;
        session.started_ms = shared.started_ms;
        session.updated_ms = shared.updated_ms;
        session.active_ms = shared.active_ms;
        session.active_days = shared.active_days;
        session.user_turns = shared.user_turns;
        session.assistant_turns = shared.assistant_turns;
        session.tool_calls = shared.tool_calls;
        session.tokens_in = shared.tokens_in;
        session.tokens_out = shared.tokens_out;
        session.tokens_cached = shared.tokens_cached;
        session.summary = shared.summary;
        session.summary_stale = shared.summary_stale;
        session.summary_for_ms = shared.summary_for_ms;
        session.source = shared.source;
        session.machine = Some(machine.clone());
        sessions.push(session);
    }
    (found, sessions)
}

/// Write this machine's sessions to the synced folder when they changed, or
/// when the last write is getting old. Returns whether anything was written.
pub fn publish(config: &Config, sessions: &[Session]) -> Result<bool> {
    let Some(folder) = folder(config) else { return Ok(false) };
    let me = this_machine();
    let mut state = load_state();
    let shared = share(sessions);
    let body = serde_json::to_string(&shared)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    me.name.hash(&mut hasher);
    config.sync_full_messages.hash(&mut hasher);
    let content = hasher.finish();

    let folder_text = folder.to_string_lossy().into_owned();
    let moved = state.folder != folder_text;
    let mut wrote = false;

    // Messages first, so the snapshot never promises what is not there.
    let messages_dir = folder.join(format!("{}.messages", me.id));
    if config.sync_full_messages {
        if moved {
            state.messages.clear();
        }
        for session in sessions.iter().filter(|s| s.machine.is_none() && s.parent.is_none()) {
            let key = session.key();
            if state.messages.get(&key) == Some(&session.updated_ms) {
                continue;
            }
            let Ok(events) = crate::transcript::events(session) else { continue };
            let compact: Vec<[String; 3]> = events.into_iter().map(compact_event).collect();
            write_whole(&messages_dir.join(messages_name(&key)), &serde_json::to_vec(&compact)?)?;
            state.messages.insert(key, session.updated_ms);
            wrote = true;
        }
    } else if messages_dir.is_dir() {
        // Turned off: take them back out of the folder.
        let _ = std::fs::remove_dir_all(&messages_dir);
        state.messages.clear();
    }

    let due = moved || content != state.content || now_ms() - state.written_ms >= HEARTBEAT_MS;
    if due {
        let snapshot = Snapshot {
            v: VERSION,
            machine: me.clone(),
            written_ms: now_ms(),
            has_messages: config.sync_full_messages,
            sessions: shared,
        };
        write_whole(&folder.join(format!("{}.scrollback.json", me.id)), &serde_json::to_vec(&snapshot)?)?;
        state.content = content;
        state.written_ms = snapshot.written_ms;
        state.folder = folder_text;
        wrote = true;
    }
    if wrote {
        save_state(&state);
    }
    Ok(wrote)
}

/// The other machines in the synced folder, and their sessions.
pub fn read(config: &Config) -> (Vec<Machine>, Vec<Session>) {
    let Some(folder) = folder(config) else { return (Vec::new(), Vec::new()) };
    let Ok(entries) = std::fs::read_dir(&folder) else { return (Vec::new(), Vec::new()) };
    let me = machine_id();
    let mut machines = Vec::new();
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // iCloud leaves `.<name>.icloud` in place of a file not yet
        // downloaded; it is read once it arrives.
        let Some(id) = name.strip_suffix(".scrollback.json") else { continue };
        if id == me || id.starts_with('.') {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let Ok(snapshot) = serde_json::from_str::<Snapshot>(&text) else { continue };
        if snapshot.v > VERSION {
            continue;
        }
        let (machine, theirs) = unpack(snapshot, None, config);
        machines.push(machine);
        sessions.extend(theirs);
    }
    machines.sort_by_key(|m| m.name.to_lowercase());
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    (machines, sessions)
}

/// Summaries this machine wrote earlier and shared, put back into an index
/// that lost them, such as one rebuilt after its folder was deleted, so they
/// are not written and paid for again. Must run before this machine's file
/// is next written, which would otherwise drop them. Returns how many were
/// put back.
pub fn restore(store: &mut crate::Store, config: &Config) -> usize {
    let Some(folder) = folder(config) else { return 0 };
    let mut path = folder.join(format!("{}.scrollback.json", machine_id()));
    if !path.is_file() {
        match earlier_self(&folder) {
            Some(earlier) => path = earlier,
            None => return 0,
        }
    }
    let Ok(text) = std::fs::read_to_string(path) else { return 0 };
    let Ok(snapshot) = serde_json::from_str::<Snapshot>(&text) else { return 0 };
    let kept: Vec<(String, Summary, i64)> = snapshot
        .sessions
        .into_iter()
        .filter_map(|shared| {
            let summary = shared.summary?;
            // Files from before the covered time was shared: a current
            // summary covers the session as it stood.
            let covered = shared.summary_for_ms.unwrap_or(if shared.summary_stale { 0 } else { shared.updated_ms });
            Some((format!("{}:{}", shared.harness, shared.id), summary, covered))
        })
        .collect();
    store.restore_summaries(&kept).unwrap_or(0)
}

/// This machine's file from before its id was lost with its data folder:
/// the only file in the folder with this machine's name and system. Its id
/// is taken back, so the machine carries on as itself rather than leaving
/// a second, ghostly copy of itself behind.
fn earlier_self(folder: &Path) -> Option<PathBuf> {
    let me = this_machine();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".scrollback.json") || name.starts_with('.') {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let Ok(snapshot) = serde_json::from_str::<Snapshot>(&text) else { continue };
        if snapshot.machine.name == me.name && snapshot.machine.os == me.os {
            found.push((entry.path(), snapshot.machine.id));
        }
    }
    let [(path, id)] = <[_; 1]>::try_from(found).ok()?;
    let valid = id.len() >= 8 && id.chars().all(|c| c.is_ascii_alphanumeric());
    if !valid || std::fs::write(crate::data_dir().join("machine-id"), &id).is_err() {
        return None;
    }
    Some(path)
}

/// One round with the synced folder: write this machine's sessions there
/// when they changed, read the other machines', and bring the index of
/// their messages up to date. `mine` is this machine's sessions from the
/// index, less any sources switched off. With syncing off, nothing is
/// written and the other machines' messages are dropped from the index.
pub fn exchange(store: &mut crate::Store, mine: &[Session], config: &Config) -> (Vec<Machine>, Vec<Session>) {
    let (mut machines, mut others) = (Vec::new(), Vec::new());
    let mut shared = Vec::new();
    if let Some(folder) = folder(config) {
        if let Err(err) = publish(config, mine) {
            eprintln!("could not write to the synced folder: {err:#}");
        }
        (machines, others) = read(config);
        shared = others
            .iter()
            .filter(|s| messages_path_in(&folder, s).is_some_and(|path| path.is_file()))
            .cloned()
            .collect();
    }
    // Machines on Tailscale; one also in the synced folder is taken from
    // there, which keeps working while it sleeps.
    if !config.tailnet_machines.is_empty() {
        crate::tailnet::poll(config);
        let (found, theirs) = crate::tailnet::read(config);
        let me = machine_id();
        let known: Vec<String> = machines.iter().map(|m| m.id.clone()).chain([me]).collect();
        others.extend(theirs.into_iter().filter(|s| s.machine.as_ref().is_some_and(|m| !known.contains(&m.id))));
        machines.extend(found.into_iter().filter(|m| !known.contains(&m.id)));
        machines.sort_by_key(|m| m.name.to_lowercase());
        others.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    }
    // Messages from the folder are indexed here; those on Tailscale are
    // searched where they are.
    if let Err(err) = store.index_remote(&shared) {
        eprintln!("could not index other machines' messages: {err:#}");
    }
    (machines, others)
}

/// The sessions here and on every other machine, newest first.
pub fn merge(mut here: Vec<Session>, others: Vec<Session>) -> Vec<Session> {
    here.extend(others);
    here.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    here
}

/// Where another machine's session keeps its messages in the synced
/// folder, whether or not they are there.
fn messages_path(session: &Session) -> Option<PathBuf> {
    messages_path_in(&folder(&Config::load())?, session)
}

fn messages_path_in(folder: &Path, session: &Session) -> Option<PathBuf> {
    let machine = session.machine.as_ref()?;
    let key = format!("{}:{}", session.harness.slug(), session.id);
    Some(folder.join(format!("{}.messages", machine.id)).join(messages_name(&key)))
}

/// A remote session's transcript, from its messages file.
pub fn events(session: &Session) -> Result<Vec<Event>> {
    if let Some(address) = session.machine.as_ref().and_then(|m| m.address.as_deref()) {
        let key = format!("{}:{}", session.harness.slug(), session.id);
        let compact = crate::tailnet::messages(address, &key)?;
        return Ok(compact.into_iter().filter_map(expand_event).collect());
    }
    let path = messages_path(session).context("syncing is off")?;
    let text = std::fs::read_to_string(&path).with_context(|| {
        let name = session.machine.as_ref().map_or("the other machine", |m| m.name.as_str());
        format!("{name} has not shared this session's messages")
    })?;
    let compact: Vec<[String; 3]> = serde_json::from_str(&text)?;
    Ok(compact.into_iter().filter_map(expand_event).collect())
}

/// Whether a remote session's messages are on this machine.
pub fn has_events(session: &Session) -> bool {
    // A machine on Tailscale is asked for them when they are wanted.
    if session.machine.as_ref().is_some_and(|m| m.address.is_some()) {
        return true;
    }
    messages_path(session).is_some_and(|path| path.is_file())
}

pub(crate) fn compact_event(event: Event) -> [String; 3] {
    match event {
        Event::Prompt(text) => ["p".into(), text, String::new()],
        Event::Reply(text) => ["r".into(), text, String::new()],
        Event::Tool { name, target } => ["t".into(), name, target.unwrap_or_default()],
        Event::Note(text) => ["n".into(), text, String::new()],
    }
}

pub(crate) fn expand_event([kind, text, target]: [String; 3]) -> Option<Event> {
    Some(match kind.as_str() {
        "p" => Event::Prompt(text),
        "r" => Event::Reply(text),
        "t" => Event::Tool { name: text, target: (!target.is_empty()).then_some(target) },
        "n" => Event::Note(text),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_survive_the_trip() {
        let events = vec![
            Event::Prompt("Fix the build".into()),
            Event::Tool { name: "Bash".into(), target: Some("cargo build".into()) },
            Event::Tool { name: "Read".into(), target: None },
            Event::Reply("Fixed.".into()),
        ];
        let back: Vec<Event> = events.clone().into_iter().map(compact_event).filter_map(expand_event).collect();
        assert_eq!(format!("{back:?}"), format!("{events:?}"));
    }

    #[test]
    fn message_file_names_are_safe() {
        assert_eq!(messages_name("claude-code:4f2c/../x"), "claude-code_4f2c____x.json");
    }
}
