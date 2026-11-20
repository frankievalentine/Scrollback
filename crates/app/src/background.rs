//! Keeps summaries up to date while the session window is closed and only
//! the menu bar icon is left. With the window open, the window does this
//! itself and this stays out of the way.

use crate::settings::Settings;
use crate::MainWindow;
use gpui::App;
use scrollback_core::{summarize, Store};
use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EVERY: Duration = Duration::from_secs(60);
/// How many summaries one pass writes, so a backlog is worked off gently.
const PER_PASS: usize = 2;
/// How long to hold off after the Claude plan limit is reported.
const LIMIT_PAUSE: Duration = Duration::from_secs(30 * 60);

pub fn start(cx: &mut App) {
    cx.spawn(async move |cx| {
        // Sessions that failed, so the same one is not retried every minute.
        let mut failed: HashSet<String> = HashSet::new();
        let mut paused_until: Option<Instant> = None;
        loop {
            cx.background_executor().timer(EVERY).await;
            let wanted = cx.update(|cx| {
                let config = &cx.global::<Settings>().0;
                let window_open = cx
                    .global::<MainWindow>()
                    .0
                    .is_some_and(|window| window.read_with(cx, |_, _| ()).is_ok());
                (!window_open && config.onboarded && config.auto_summarize).then(|| config.clone())
            });
            let Ok(Some(config)) = wanted else { continue };
            if paused_until.is_some_and(|until| Instant::now() < until) {
                continue;
            }
            let skip = failed.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move { pass(&config, &skip) })
                .await;
            for (key, error) in outcome {
                eprintln!("background summary failed for {key}: {error}");
                if error.to_lowercase().contains("limit") {
                    paused_until = Some(Instant::now() + LIMIT_PAUSE);
                }
                failed.insert(key);
            }
        }
    })
    .detach();
}

/// Refresh the index and summarise a couple of sessions that are due one:
/// those gone quiet, and those in use that have moved on. Returns the
/// sessions that failed, with why.
fn pass(config: &scrollback_core::Config, skip: &HashSet<String>) -> Vec<(String, String)> {
    if std::env::var_os("SCROLLBACK_NO_CLAUDE").is_some() || summarize::claude_binary().is_none() {
        return Vec::new();
    }
    let sessions = Store::open().and_then(|mut store| {
        store.refresh()?;
        store.sessions(false)
    });
    let Ok(sessions) = sessions else { return Vec::new() };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let due = sessions.iter().filter(|s| {
        summarize::due(s, config, now_ms)
            && !skip.contains(&s.key())
            && !config.hidden_sources.iter().any(|slug| slug == s.origin.slug())
    });
    let mut failures = Vec::new();
    for session in due.take(PER_PASS) {
        let written = summarize::summarize(session)
            .and_then(|written| Store::open()?.set_summary(session, &written.summary));
        if let Err(err) = written {
            failures.push((session.key(), format!("{err:#}")));
        }
    }
    failures
}
