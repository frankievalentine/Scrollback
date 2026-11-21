//! Sessions from your other machines over Tailscale, for machines with no
//! synced folder, such as a devbox reached over SSH.
//!
//! A machine shares by answering a few read-only questions on its Tailscale
//! address: its sessions, one session's messages, and a message search. It
//! answers only devices signed in to Tailscale as the same person, and only
//! on the Tailscale address, so it is never reachable from anywhere else.
//! Nothing it is asked can change anything on it.
//!
//! Other machines ask for its sessions every minute and keep the last
//! answer, so its sessions stay while it sleeps.

use crate::config::Config;
use crate::store::MessageHit;
use crate::sync::{Machine, Snapshot};
use crate::{Session, Store};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The port Scrollback answers on, on the Tailscale address only.
pub const PORT: u16 = 47_821;
/// How long a machine gets to send its sessions.
const POLL_TIMEOUT: Duration = Duration::from_secs(4);
/// How long a machine gets to answer a search before it is left out.
pub const SEARCH_TIMEOUT: Duration = Duration::from_millis(500);
const MESSAGES_TIMEOUT: Duration = Duration::from_secs(6);

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// The `tailscale` command: on the path, or where the apps put it.
pub fn binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "tailscale.exe" } else { "tailscale" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let known: &[&str] = if cfg!(target_os = "macos") {
        &["/Applications/Tailscale.app/Contents/MacOS/Tailscale", "/opt/homebrew/bin/tailscale", "/usr/local/bin/tailscale"]
    } else if cfg!(windows) {
        &[r"C:\Program Files\Tailscale\tailscale.exe"]
    } else {
        &["/usr/bin/tailscale", "/usr/sbin/tailscale", "/usr/local/bin/tailscale"]
    };
    known.iter().map(PathBuf::from).find(|path| path.is_file())
}

/// A device on the tailnet.
#[derive(Debug, Clone)]
pub struct Device {
    /// Its name, as shown in Tailscale.
    pub name: String,
    /// Its name on the tailnet, such as `devbox.tail1234.ts.net`, by which
    /// it is reached.
    pub address: String,
    pub ips: Vec<IpAddr>,
    /// Who it is signed in as.
    pub user: String,
    pub os: String,
    pub online: bool,
}

/// This device and the others on the tailnet.
#[derive(Debug, Clone)]
pub struct Status {
    pub me: Device,
    pub peers: Vec<Device>,
}

impl Status {
    /// The other devices signed in as the same person: the ones that may
    /// read this machine's sessions.
    fn own(&self) -> impl Iterator<Item = &Device> {
        self.peers.iter().filter(|peer| peer.user == self.me.user)
    }

    /// Those of them that could run Scrollback, to offer for adding.
    pub fn mine(&self) -> impl Iterator<Item = &Device> {
        self.own().filter(|peer| !matches!(peer.os.as_str(), "iOS" | "android" | "tvOS"))
    }

    /// Whether a connection from `ip` is this person's own device.
    fn allows(&self, ip: IpAddr) -> bool {
        self.me.ips.contains(&ip) || self.own().any(|peer| peer.ips.contains(&ip))
    }
}

/// Ask Tailscale about this device and the rest of the tailnet.
pub fn status() -> Result<Status> {
    let binary = binary().ok_or_else(|| anyhow!("Tailscale is not installed"))?;
    let output = std::process::Command::new(binary)
        .args(["status", "--json"])
        .output()
        .context("asking Tailscale for its status")?;
    let reply: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| anyhow!("Tailscale is not running"))?;
    if reply["BackendState"].as_str() != Some("Running") {
        bail!("Tailscale is not connected");
    }
    let device = |value: &Value| Device {
        name: value["HostName"].as_str().unwrap_or("").to_string(),
        address: value["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_string(),
        ips: value["TailscaleIPs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|ip| ip.as_str()?.parse().ok())
            .collect(),
        user: value["UserID"].to_string(),
        os: value["OS"].as_str().unwrap_or("").to_string(),
        online: value["Online"].as_bool().unwrap_or(false),
    };
    let me = device(&reply["Self"]);
    let mut peers: Vec<Device> = reply["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|peers| peers.values())
        .map(device)
        .collect();
    peers.sort_by_key(|peer| peer.name.to_lowercase());
    Ok(Status { me, peers })
}

// ----- Answering -----

/// Answering on the Tailscale address while it lives.
pub struct Server {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Who may ask, kept current as devices come and go.
struct Access {
    status: Mutex<(Instant, Status)>,
}

impl Access {
    fn allows(&self, ip: IpAddr) -> bool {
        let mut held = self.status.lock().unwrap();
        if held.1.allows(ip) {
            return true;
        }
        // A device that has just joined: look again, but not too often.
        if held.0.elapsed() > Duration::from_secs(5) {
            if let Ok(fresh) = status() {
                *held = (Instant::now(), fresh);
            }
        }
        held.1.allows(ip)
    }
}

/// Start answering on this machine's Tailscale address.
pub fn serve() -> Result<Server> {
    let status = status()?;
    let ip = status
        .me
        .ips
        .iter()
        .find(|ip| ip.is_ipv4())
        .or(status.me.ips.first())
        .copied()
        .ok_or_else(|| anyhow!("this device has no Tailscale address yet"))?;
    let listener = TcpListener::bind((ip, PORT)).with_context(|| format!("listening on {ip}:{PORT}"))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let access = Arc::new(Access { status: Mutex::new((Instant::now(), status)) });
    let stopping = stop.clone();
    std::thread::Builder::new().name("tailnet".into()).spawn(move || {
        while !stopping.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, peer)) => {
                    let access = access.clone();
                    std::thread::spawn(move || {
                        if access.allows(peer.ip()) {
                            let _ = handle(stream);
                        } else {
                            let _ = respond(stream, 403, b"{\"error\":\"not your device\"}");
                        }
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    })?;
    Ok(Server { address, stop })
}

fn handle(mut stream: TcpStream) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let read = stream.read(&mut buffer)?;
        if read == 0 || head.len() > 8192 {
            break;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut words = head.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    if method != "GET" {
        return respond(stream, 405, b"{\"error\":\"read only\"}");
    }
    let (status, body) = match answer(target) {
        Ok(Some(body)) => (200, body),
        Ok(None) => (404, b"{\"error\":\"not found\"}".to_vec()),
        Err(err) => (500, serde_json::to_vec(&serde_json::json!({"error": format!("{err:#}")}))?),
    };
    respond(stream, status, &body)
}

fn respond(mut stream: TcpStream, status: u16, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

/// This machine's own sessions, less sources switched off.
fn own_sessions(store: &Store) -> Result<Vec<Session>> {
    let hidden = Config::load().hidden_sources;
    let mut sessions = store.sessions(false)?;
    sessions.retain(|s| !hidden.iter().any(|slug| slug == s.origin.slug()));
    Ok(sessions)
}

/// What is asked for at `target`, or `None` if there is no such thing.
fn answer(target: &str) -> Result<Option<Vec<u8>>> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let param = |name: &str| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| decode(value))
    };
    let store = Store::open()?;
    Ok(Some(match path {
        "/v1/sessions" => serde_json::to_vec(&crate::sync::snapshot_now(&own_sessions(&store)?))?,
        "/v1/messages" => {
            let key = param("key").unwrap_or_default();
            let sessions = own_sessions(&store)?;
            let Some(session) = sessions.iter().find(|s| s.key() == key) else { return Ok(None) };
            serde_json::to_vec(&crate::sync::compact_events(session)?)?
        }
        "/v1/search" => {
            let query = param("q").unwrap_or_default();
            let limit = param("n").and_then(|n| n.parse().ok()).unwrap_or(20usize).min(100);
            let shown: std::collections::HashSet<String> = own_sessions(&store)?.iter().map(Session::key).collect();
            let mut hits = store.search_own_messages(&query, limit)?;
            hits.retain(|hit| shown.contains(&hit.key));
            serde_json::to_vec(&hits)?
        }
        _ => return Ok(None),
    }))
}

// ----- Asking -----

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent("Scrollback")
        .build()
        .into()
}

/// Tailscale's view of the tailnet, asked at most once a minute.
static KNOWN: Mutex<Option<(Instant, Status)>> = Mutex::new(None);

/// The Tailscale IP of a device named `address`, from Tailscale itself, so
/// it is found even with MagicDNS off. An address that is already an IP,
/// or a device Tailscale does not know, is used as it is.
fn resolve(address: &str) -> String {
    if address.parse::<IpAddr>().is_ok() {
        return address.to_string();
    }
    let mut known = KNOWN.lock().unwrap();
    let stale = known.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(60));
    if stale {
        if let Ok(fresh) = status() {
            *known = Some((Instant::now(), fresh));
        }
    }
    let found = known.as_ref().and_then(|(_, status)| {
        std::iter::once(&status.me)
            .chain(&status.peers)
            .find(|device| device.address.eq_ignore_ascii_case(address) || device.name.eq_ignore_ascii_case(address))
            .and_then(|device| device.ips.iter().find(|ip| ip.is_ipv4()).or(device.ips.first()))
            .map(IpAddr::to_string)
    });
    found.unwrap_or_else(|| address.to_string())
}

fn get(address: &str, path: &str, timeout: Duration) -> Result<Vec<u8>> {
    let host = match resolve(address) {
        ip if ip.contains(':') => format!("[{ip}]"),
        other => other,
    };
    let url = format!("http://{host}:{PORT}{path}");
    let mut response = agent(timeout).get(&url).call().with_context(|| format!("reaching {address}"))?;
    let status = response.status().as_u16();
    let body = response.body_mut().with_config().limit(256 * 1024 * 1024).read_to_vec()?;
    if status != 200 {
        let why: Value = serde_json::from_slice(&body).unwrap_or_default();
        bail!("{address} said: {}", why["error"].as_str().unwrap_or("no"));
    }
    Ok(body)
}

/// What was last heard from a machine.
#[derive(Serialize, Deserialize)]
struct Heard {
    /// When it last answered.
    answered_ms: i64,
    /// Whether the last time it was asked, it answered.
    #[serde(default)]
    reachable: bool,
    snapshot: Option<Snapshot>,
}

fn heard_path(address: &str) -> PathBuf {
    let safe: String = address.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    crate::data_dir().join("tailnet").join(format!("{safe}.json"))
}

fn load_heard(address: &str) -> Option<Heard> {
    serde_json::from_slice(&std::fs::read(heard_path(address)).ok()?).ok()
}

/// Ask each machine on Tailscale for its sessions, all at once, keeping the
/// answers. A machine that does not answer keeps its last answer, marked as
/// unreachable.
pub fn poll(config: &Config) {
    let _ = std::fs::create_dir_all(crate::data_dir().join("tailnet"));
    std::thread::scope(|scope| {
        for address in &config.tailnet_machines {
            scope.spawn(move || {
                let fresh = get(address, "/v1/sessions", POLL_TIMEOUT)
                    .and_then(|body| Ok(serde_json::from_slice::<Snapshot>(&body)?));
                let heard = match fresh {
                    Ok(snapshot) => Heard { answered_ms: now_ms(), reachable: true, snapshot: Some(snapshot) },
                    Err(err) => {
                        eprintln!("could not reach {address}: {err:#}");
                        let mut kept = load_heard(address)
                            .unwrap_or(Heard { answered_ms: 0, reachable: false, snapshot: None });
                        kept.reachable = false;
                        kept
                    }
                };
                if let Ok(bytes) = serde_json::to_vec(&heard) {
                    let _ = std::fs::write(heard_path(address), bytes);
                }
            });
        }
    });
}

/// The machines on Tailscale and their sessions, as last heard.
pub fn read(config: &Config) -> (Vec<Machine>, Vec<Session>) {
    let mut machines = Vec::new();
    let mut sessions = Vec::new();
    for address in &config.tailnet_machines {
        let heard = load_heard(address);
        match heard.as_ref().and_then(|h| h.snapshot.as_ref()) {
            Some(_) => {
                let heard = heard.unwrap();
                let mut snapshot = heard.snapshot.unwrap();
                // When it was last heard from, rather than when it wrote.
                snapshot.written_ms = heard.answered_ms;
                let (mut machine, theirs) = crate::sync::unpack(snapshot, Some(address), config);
                machine.unreachable = !heard.reachable;
                machines.push(machine);
                sessions.extend(theirs);
            }
            None => machines.push(Machine {
                id: format!("tailnet-{address}"),
                name: address.split('.').next().unwrap_or(address).to_string(),
                os: String::new(),
                written_ms: 0,
                sessions: 0,
                has_messages: false,
                address: Some(address.clone()),
                unreachable: true,
            }),
        }
    }
    (machines, sessions)
}

/// One session's messages, asked of the machine it ran on.
pub fn messages(address: &str, key: &str) -> Result<Vec<[String; 3]>> {
    let body = get(address, &format!("/v1/messages?key={}", encode(key)), MESSAGES_TIMEOUT)
        .with_context(|| format!("{} is not answering", address.split('.').next().unwrap_or(address)))?;
    Ok(serde_json::from_slice(&body)?)
}

/// Search the messages on every machine on Tailscale at once. A machine
/// that has not answered within half a second is left out.
pub fn search(config: &Config, query: &str, limit: usize) -> Vec<MessageHit> {
    let path = format!("/v1/search?q={}&n={limit}", encode(query));
    let asked: Vec<(String, String)> = config
        .tailnet_machines
        .iter()
        .filter_map(|address| {
            let heard = load_heard(address)?;
            let id = heard.snapshot?.machine.id;
            (!config.hidden_machines.contains(&id)).then(|| (address.clone(), id))
        })
        .collect();
    let found = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for (address, id) in &asked {
            let (path, found) = (&path, &found);
            scope.spawn(move || {
                let Ok(body) = get(address, path, SEARCH_TIMEOUT) else { return };
                let Ok(hits) = serde_json::from_slice::<Vec<MessageHit>>(&body) else { return };
                let mut found = found.lock().unwrap();
                for mut hit in hits {
                    hit.key = format!("{id}/{}", hit.key);
                    found.push(hit);
                }
            });
        }
    });
    found.into_inner().unwrap()
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                Some(byte) => {
                    out.push(byte);
                    i += 3;
                }
                None => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, ip: &str, user: &str) -> Device {
        Device {
            name: name.into(),
            address: format!("{name}.example.ts.net"),
            ips: vec![ip.parse().unwrap()],
            user: user.into(),
            os: "linux".into(),
            online: true,
        }
    }

    #[test]
    fn only_your_own_devices_may_ask() {
        let status = Status {
            me: device("laptop", "100.64.0.1", "1"),
            peers: vec![device("devbox", "100.64.0.2", "1"), device("friends-phone", "100.64.0.3", "2")],
        };
        assert!(status.allows("100.64.0.1".parse().unwrap()));
        assert!(status.allows("100.64.0.2".parse().unwrap()));
        assert!(!status.allows("100.64.0.3".parse().unwrap()));
        assert!(!status.allows("192.168.1.20".parse().unwrap()));
    }

    #[test]
    fn queries_survive_the_trip() {
        for text in ["shared temp", "claude-code:4f2c/ab", "naïve & 100%", "a+b"] {
            assert_eq!(decode(&encode(text)), text);
        }
        assert_eq!(decode("a+b%20c%"), "a b c%");
    }
}
