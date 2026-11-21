//! Keeping Scrollback up to date: a look for a newer release shortly after
//! launch and every few hours, a quiet download when one is found, and
//! "Restart to update" once it is ready. Where this copy cannot replace
//! itself, the release is only pointed out.

use crate::settings::Settings;
use gpui::{App, AsyncApp, Global};
use scrollback_core::update::{self, Release};
use std::path::PathBuf;
use std::time::Duration;

/// How long after launch the first look is, so it does not slow the start.
const FIRST_LOOK: Duration = Duration::from_secs(20);
/// How often to look again while Scrollback runs.
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone)]
pub enum State {
    /// Not looked yet.
    Idle,
    Checking,
    /// This is the latest version, as of the time given.
    Current(chrono::DateTime<chrono::Local>),
    /// A newer release, which this copy cannot install by itself.
    Available(Release),
    Downloading(Release),
    /// Downloaded and checked; installed on restart.
    Ready(Release, PathBuf),
    Installing(Release),
    /// Why the last look or download failed.
    Failed(String),
}

pub struct Updates(pub State);

impl Global for Updates {}

pub fn state(cx: &App) -> State {
    cx.global::<Updates>().0.clone()
}

/// The release that is out, if any: ready, on its way, or to fetch by hand.
pub fn pending(cx: &App) -> Option<(Release, bool)> {
    match state(cx) {
        State::Ready(release, _) => Some((release, true)),
        State::Available(release) | State::Downloading(release) => Some((release, false)),
        _ => None,
    }
}

pub fn start(cx: &mut App) {
    cx.set_global(Updates(State::Idle));
    // Development aid: `SCROLLBACK_DEV_UPDATE_SYNC=1` looks and downloads
    // before any window opens, so captures of the first frames show it.
    if std::env::var_os("SCROLLBACK_DEV_UPDATE_SYNC").is_some() {
        let found = update::check().ok().flatten();
        let state = match found {
            Some(release) if release.installable() => match update::download(&release) {
                Ok(path) => State::Ready(release, path),
                Err(_) => State::Available(release),
            },
            Some(release) => State::Available(release),
            None => State::Current(chrono::Local::now()),
        };
        set(cx, state);
        return;
    }
    // Development aid: with a test server, look straight away.
    let first = match std::env::var_os("SCROLLBACK_UPDATE_URL") {
        Some(_) => Duration::from_millis(500),
        None => FIRST_LOOK,
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(first).await;
        loop {
            let wanted = cx.update(|cx| cx.global::<Settings>().0.check_updates).unwrap_or(false);
            if wanted {
                let _ = cx.update(|cx| check(cx, false));
            }
            cx.background_executor().timer(EVERY).await;
        }
    })
    .detach();
}

/// Look for a newer release now, as asked from the menu or Settings.
pub fn check_now(cx: &mut App) {
    check(cx, true);
}

fn set(cx: &mut App, state: State) {
    cx.set_global(Updates(state));
}

fn check(cx: &mut App, asked: bool) {
    match state(cx) {
        // Already busy, or already done.
        State::Checking | State::Downloading(_) | State::Ready(..) | State::Installing(_) => return,
        _ => {}
    }
    set(cx, State::Checking);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let found = cx.background_executor().spawn(async { update::check() }).await;
        let _ = cx.update(|cx| match found {
            Ok(None) => set(cx, State::Current(chrono::Local::now())),
            Ok(Some(release)) if release.installable() => download(cx, release),
            Ok(Some(release)) => set(cx, State::Available(release)),
            Err(err) => {
                eprintln!("could not look for updates: {err:#}");
                // A look nobody asked for fails quietly.
                set(cx, if asked { State::Failed(format!("{err:#}")) } else { State::Idle });
            }
        });
    })
    .detach();
}

fn download(cx: &mut App, release: Release) {
    set(cx, State::Downloading(release.clone()));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let fetched = release.clone();
        let file = cx.background_executor().spawn(async move { update::download(&fetched) }).await;
        let _ = cx.update(|cx| match file {
            Ok(path) => {
                set(cx, State::Ready(release, path));
                // Development aid: install as soon as it is ready.
                if std::env::var_os("SCROLLBACK_DEV_UPDATE_INSTALL").is_some() {
                    act(cx);
                }
            }
            Err(err) => {
                eprintln!("could not download {}: {err:#}", release.version);
                // It can still be had by hand.
                set(cx, State::Available(release));
            }
        });
    })
    .detach();
}

/// Install the downloaded release and start again, or, for a release this
/// copy cannot install, open its page.
pub fn act(cx: &mut App) {
    match state(cx) {
        State::Ready(release, path) => {
            set(cx, State::Installing(release.clone()));
            cx.spawn(async move |cx: &mut AsyncApp| {
                let installing = release.clone();
                let done = cx
                    .background_executor()
                    .spawn(async move { update::install(&installing, &path) })
                    .await;
                let _ = cx.update(|cx| match done {
                    Ok(()) => cx.quit(),
                    Err(err) => set(cx, State::Failed(format!("Could not install {}: {err:#}", release.version))),
                });
            })
            .detach();
        }
        State::Available(release) | State::Downloading(release) => cx.open_url(&release.page),
        _ => check_now(cx),
    }
}

/// A line about where updates stand, for Settings.
pub fn describe(state: &State) -> String {
    match state {
        State::Idle => format!("Scrollback {} is installed.", update::CURRENT),
        State::Checking => "Looking for a newer version…".to_string(),
        State::Current(at) => format!(
            "Scrollback {} is the latest version. Last checked at {}.",
            update::CURRENT,
            at.format("%H:%M")
        ),
        State::Available(release) => {
            let why = match update::method() {
                update::Method::Manual(why) => format!(" It can't install itself: {why}."),
                _ => String::new(),
            };
            format!("Scrollback {} is out.{why}", release.version)
        }
        State::Downloading(release) => format!("Downloading Scrollback {}…", release.version),
        State::Ready(release, _) => format!("Scrollback {} is ready. It is installed when Scrollback restarts.", release.version),
        State::Installing(release) => format!("Installing Scrollback {}…", release.version),
        State::Failed(why) => why.clone(),
    }
}
