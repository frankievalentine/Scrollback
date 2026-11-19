use anyhow::{bail, Result};
use chrono::{Local, TimeZone};
use scrollback_core::search::Searcher;
use scrollback_core::{Harness, Session, Store};
use std::time::Instant;

const USAGE: &str = "\
scrollback: every coding-agent session on this machine, in one place

Usage:
  scrollback index                 Update the index from every harness found
  scrollback list [-n N] [--all]   Recent sessions (--all includes sub-agents)
  scrollback search <query> [-n N] Fuzzy search titles, projects and prompts
  scrollback stats                 Totals per harness
";

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let limit = take_value(&mut args, "-n")?
        .map(|n| n.parse::<usize>())
        .transpose()?;
    let all = take_flag(&mut args, "--all");

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
        "Read {} changed files, skipped {} unchanged, dropped {} removed, in {:.0?}.",
        report.parsed, report.unchanged, report.removed, report.elapsed
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
    Ok(())
}

fn stats() -> Result<()> {
    let sessions = Store::open()?.sessions(true)?;
    for harness in Harness::ALL {
        let mine: Vec<&Session> = sessions.iter().filter(|s| s.harness == harness).collect();
        let main = mine.iter().filter(|s| s.parent.is_none()).count();
        let tokens: u64 = mine.iter().map(|s| s.tokens_in + s.tokens_out).sum();
        println!(
            "{:<12} {:>5} sessions  {:>4} sub-agent  {:>8} tokens",
            harness.name(),
            main,
            mine.len() - main,
            compact(tokens)
        );
    }
    Ok(())
}

fn print_row(session: &Session) {
    let when = Local
        .timestamp_millis_opt(session.updated_ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    println!(
        "{when}  {:<11} {:<18} {}",
        session.harness.slug(),
        scrollback_core::squash(session.project(), 18),
        scrollback_core::squash(&session.title, 70)
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
