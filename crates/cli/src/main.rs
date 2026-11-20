use anyhow::{bail, Result};
use chrono::{Local, TimeZone};
use scrollback_core::search::Searcher;
use scrollback_core::summarize;
use scrollback_core::{Origin, Session, Store};
use std::sync::mpsc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const USAGE: &str = "\
Scrollback: every coding-agent session on this machine, in one place

Usage:
  scrollback index                 Update the index from every harness found
  scrollback list [-n N] [--all]   Recent sessions (--all includes sub-agents)
  scrollback search <query> [-n N] Fuzzy search titles, projects and prompts
  scrollback stats                 Totals per source
  scrollback summarize [-n N]      Write summaries for the N most recent sessions
                                   that lack one (default 5). --active includes
                                   sessions used in the last 10 minutes;
                                   --redo rewrites summaries that already exist;
                                   --ungrouped rewrites those written before
                                   steps were tied to the transcript
  scrollback summarize <query>     Write or rewrite the summary of the best match
  scrollback show <query>          Print the summary of the best match
  scrollback preview <query>       Print exactly what `summarize` would send
  scrollback export <query> [-o F] Write the best match out as Markdown, to F
                                   or to standard output
";

fn main() -> Result<()> {
    // Exit quietly when output is piped into something that stops reading,
    // such as `scrollback list | head`.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let limit = take_value(&mut args, "-n")?
        .map(|n| n.parse::<usize>())
        .transpose()?;
    let all = take_flag(&mut args, "--all");
    let active = take_flag(&mut args, "--active");
    let redo = take_flag(&mut args, "--redo");
    let ungrouped = take_flag(&mut args, "--ungrouped");
    let output = take_value(&mut args, "-o")?;

    let command = args.first().map(String::as_str).unwrap_or("");
    match command {
        "index" => index(),
        "list" => list(limit.unwrap_or(20), all),
        "search" => {
            let query = args[1..].join(" ");
            if query.trim().is_empty() {
                bail!("search needs a query, for example: scrollback search tokn rfrsh");
            }
            search(&query, limit.unwrap_or(15))
        }
        "stats" => stats(),
        "summarize" if args.len() > 1 => {
            let mut session = best_match(&args[1..].join(" "))?;
            let written = summarize::summarize(&session)?;
            Store::open()?.set_summary(&session, &written.summary)?;
            // Shown as written: its new title may no longer match the words
            // it was found by.
            session.summary = Some(written.summary);
            session.summary_stale = false;
            print_summary(&session);
            Ok(())
        }
        "summarize" => summarize_pending(limit.unwrap_or(5), active, redo, ungrouped),
        "show" => show(&args[1..].join(" ")),
        "preview" => {
            let session = best_match(&args[1..].join(" "))?;
            println!("{}", summarize::preview(&session)?);
            Ok(())
        }
        "export" => {
            if args.len() < 2 {
                bail!("give a few words to pick a session, for example: scrollback export runyard redesign");
            }
            let session = best_match(&args[1..].join(" "))?;
            let text = scrollback_core::export::markdown(&session)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, text)?;
                    eprintln!("Wrote {path}");
                }
                None => print!("{text}"),
            }
            Ok(())
        }
        "" | "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

fn index() -> Result<()> {
    let mut store = Store::open()?;
    let report = store.refresh()?;
    println!(
        "Read {} changed files, skipped {} unchanged, dropped {} removed, \
         indexed messages of {} sessions, in {:.0?}.",
        report.parsed, report.unchanged, report.removed, report.messages_indexed, report.elapsed
    );
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    stats()
}

fn list(limit: usize, all: bool) -> Result<()> {
    let sessions = Store::open()?.sessions(all)?;
    if sessions.is_empty() {
        println!("No sessions indexed yet. Run `scrollback index` first.");
    }
    for session in sessions.iter().take(limit) {
        print_row(session);
    }
    Ok(())
}

fn search(query: &str, limit: usize) -> Result<()> {
    let sessions = Store::open()?.sessions(false)?;
    let started = Instant::now();
    let hits = Searcher::new().search(query, &sessions);
    let elapsed = started.elapsed();
    for hit in hits.iter().take(limit) {
        print_row(&sessions[hit.index]);
    }
    println!(
        "{} matches among {} sessions in {:.1?}",
        hits.len(),
        sessions.len(),
        elapsed
    );

    let started = Instant::now();
    let messages = Store::open()?.search_messages(query, limit)?;
    let elapsed = started.elapsed();
    if !messages.is_empty() {
        println!("\nIn messages ({:.1?}):", elapsed);
    }
    for hit in &messages {
        let title = sessions
            .iter()
            .find(|s| s.key() == hit.key)
            .map(|s| s.display_title())
            .unwrap_or("");
        println!(
            "  {:<5} {}\n        in {}",
            if hit.from_user { "you" } else { "agent" },
            scrollback_core::squash(&hit.snippet, 150),
            scrollback_core::squash(title, 70)
        );
    }
    Ok(())
}

fn stats() -> Result<()> {
    let sessions = Store::open()?.sessions(true)?;
    for origin in Origin::ALL {
        let mine: Vec<&Session> = sessions.iter().filter(|s| s.origin == origin).collect();
        if mine.is_empty() {
            continue;
        }
        let main = mine.iter().filter(|s| s.parent.is_none()).count();
        let tokens: u64 = mine.iter().map(|s| s.tokens()).sum();
        let cached: u64 = mine.iter().map(|s| s.tokens_cached).sum();
        println!(
            "{:<15} {:>5} sessions  {:>4} sub-agent  {:>8} tokens  {:>8} cached",
            origin.name(),
            main,
            mine.len() - main,
            compact(tokens),
            compact(cached)
        );
    }
    Ok(())
}

/// The session a query matches best.
fn best_match(query: &str) -> Result<Session> {
    if query.trim().is_empty() {
        bail!("give a few words to pick a session, for example: scrollback show runyard redesign");
    }
    let mut sessions = Store::open()?.sessions(false)?;
    let hits = Searcher::new().search(query, &sessions);
    match hits.first() {
        Some(hit) => Ok(sessions.swap_remove(hit.index)),
        None => bail!("no session matches `{query}`"),
    }
}

fn show(query: &str) -> Result<()> {
    print_summary(&best_match(query)?);
    Ok(())
}

fn print_summary(session: &Session) {
    print_row(session);
    let Some(summary) = &session.summary else {
        println!("\nNo summary yet. Run `scrollback summarize` to write one.");
        return;
    };
    println!("\n{}  [{}]", summary.title, summary.outcome.label());
    println!("\n{}", summary.summary);
    println!("\nYou asked\n  {}", summary.asked);
    for (heading, lines) in [
        ("What happened", &summary.happened),
        ("Left open", &summary.open_ends),
        ("Friction", &summary.friction),
        ("Files edited", &summary.files),
    ] {
        if lines.is_empty() {
            continue;
        }
        println!("\n{heading}");
        for line in lines {
            println!("  - {line}");
        }
    }
    if session.summary_stale {
        println!("\nThe session has had activity since this summary was written.");
    }
}

fn summarize_pending(limit: usize, include_active: bool, redo: bool, ungrouped: bool) -> Result<()> {
    let store = Store::open()?;
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let pending: Vec<Session> = store
        .sessions(false)?
        .into_iter()
        .filter(|s| {
            let lacks_steps = s.summary.as_ref().is_some_and(|sum| sum.step_starts.is_empty());
            redo || s.summary.is_none() || s.summary_stale || (ungrouped && lacks_steps)
        })
        .filter(|s| include_active || now_ms - s.updated_ms > 10 * 60 * 1000)
        .take(limit)
        .collect();
    if pending.is_empty() {
        println!("Every session already has a current summary.");
        return Ok(());
    }

    // A few at a time; each one is a separate short request.
    let started = Instant::now();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    let mut cost = 0.0;
    let mut written = 0;
    std::thread::scope(|scope| -> Result<()> {
        for _ in 0..pending.len().min(4) {
            let tx = tx.clone();
            let (next, pending) = (&next, &pending);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(session) = pending.get(i) else { break };
                let _ = tx.send((i, summarize::summarize(session)));
            });
        }
        drop(tx);
        for (i, outcome) in rx {
            let session = &pending[i];
            match outcome {
                Ok(done) => {
                    store.set_summary(session, &done.summary)?;
                    cost += done.cost_usd;
                    written += 1;
                    println!(
                        "{:<8} {:<16} {}",
                        done.summary.outcome.label().to_lowercase(),
                        scrollback_core::squash(session.project(), 16),
                        done.summary.title
                    );
                }
                Err(err) => eprintln!(
                    "failed   {:<16} {}: {err:#}",
                    scrollback_core::squash(session.project(), 16),
                    scrollback_core::squash(&session.title, 40)
                ),
            }
        }
        Ok(())
    })?;
    println!(
        "Wrote {written} of {} summaries in {:.0?}, reported cost ${cost:.4}.",
        pending.len(),
        started.elapsed()
    );
    Ok(())
}

fn print_row(session: &Session) {
    let when = Local
        .timestamp_millis_opt(session.updated_ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    println!(
        "{when}  {:<14} {:<18} {}",
        session.origin.slug(),
        scrollback_core::squash(session.project(), 18),
        scrollback_core::squash(session.display_title(), 70)
    );
}

/// 1234567 -> "1.2M"
fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.0}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>> {
    let Some(at) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        bail!("{flag} needs a value");
    }
    args.remove(at);
    Ok(Some(args.remove(at)))
}
