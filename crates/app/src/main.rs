// A windowed app on Windows opens a console beside itself unless told not
// to. Development builds keep it, for the messages printed there.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app_menu;
mod background;
mod chrome;
mod field;
mod home;
mod login;
mod markdown;
mod popover;
mod settings;
mod theme;
mod tray;
mod ui;
mod updates;

use gpui::{
    prelude::*, px, size, App, Application, AssetSource, Bounds, Global, SharedString,
    WindowBounds, WindowHandle, WindowOptions,
};
use home::Home;
use std::borrow::Cow;

/// Harness logos, outcome marks and view icons, compiled into the binary.
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "claude-code.svg" => Some(include_bytes!("../assets/claude-code.svg")),
            "codex.svg" => Some(include_bytes!("../assets/codex.svg")),
            "opencode.svg" => Some(include_bytes!("../assets/opencode.svg")),
            "pi.svg" => Some(include_bytes!("../assets/pi.svg")),
            "outcome-done.svg" => Some(include_bytes!("../assets/outcome-done.svg")),
            "outcome-partial.svg" => Some(include_bytes!("../assets/outcome-partial.svg")),
            "outcome-stalled.svg" => Some(include_bytes!("../assets/outcome-stalled.svg")),
            "view-sessions.svg" => Some(include_bytes!("../assets/view-sessions.svg")),
            "view-activity.svg" => Some(include_bytes!("../assets/view-activity.svg")),
            "check.svg" => Some(include_bytes!("../assets/check.svg")),
            "mark.svg" => Some(include_bytes!("../assets/mark.svg")),
            "search.svg" => Some(include_bytes!("../assets/search.svg")),
            "claude.svg" => Some(include_bytes!("../assets/claude.svg")),
            "openai.svg" => Some(include_bytes!("../assets/openai.svg")),
            "cursor.svg" => Some(include_bytes!("../assets/cursor.svg")),
            "t3.svg" => Some(include_bytes!("../assets/t3.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

/// The session window, while it is open.
pub struct MainWindow(pub Option<WindowHandle<Home>>);

impl Global for MainWindow {}

/// Bring the session window forward, opening it if it is closed, and select
/// a session in it when one is named.
pub fn open_main(cx: &mut App, select: Option<String>) -> Option<WindowHandle<Home>> {
    open_main_with(cx, select, String::new())
}

fn open_main_with(
    cx: &mut App,
    select: Option<String>,
    query: String,
) -> Option<WindowHandle<Home>> {
    // SCROLLBACK_BACKGROUND=1 opens windows without taking the keyboard, for
    // checking the app while someone is typing elsewhere.
    let quiet = std::env::var_os("SCROLLBACK_BACKGROUND").is_some();
    let pick = |home: &mut Home, cx: &mut gpui::Context<Home>| {
        if let Some(key) = &select {
            home.show_session(key, cx);
        }
    };
    if let Some(window) = cx.global::<MainWindow>().0 {
        let shown = window.update(cx, |home, window, cx| {
            window.activate_window();
            pick(home, cx);
        });
        if shown.is_ok() {
            if !quiet {
                cx.activate(true);
            }
            return Some(window);
        }
    }

    // The index refresh takes well under a second, so it runs before the
    // window opens and the first frame already shows real sessions.
    let (sessions, status) = home::load();
    // Development aid: `SCROLLBACK_HEIGHT=1300` opens a taller window, to
    // see a whole view in one screenshot.
    let height = std::env::var("SCROLLBACK_HEIGHT")
        .ok()
        .and_then(|h| h.parse::<f32>().ok())
        .unwrap_or(820.);
    let bounds = Bounds::centered(None, size(px(1180.), px(height)), cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(480.))),
                titlebar: Some(ui::titlebar("Scrollback")),
                app_id: Some(ui::APP_ID.into()),
                window_decorations: chrome::decorations(),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| Home::new(sessions, status, query, cx));
                view.update(cx, |home, cx| home.watch_window(window, cx));
                window.focus(&view.read(cx).focus);
                view
            },
        )
        .ok()?;
    cx.set_global(MainWindow(Some(window)));
    window.update(cx, |home, _, cx| pick(home, cx)).ok();
    if !quiet {
        cx.activate(true);
    }
    Some(window)
}

fn main() {
    // `scrollback-app some words` opens with that search already typed.
    // A start at login is marked, and opens in the menu bar alone.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let at_login = args.iter().any(|arg| arg == login::AT_LOGIN);
    let query = args.into_iter().filter(|arg| arg != login::AT_LOGIN).collect::<Vec<_>>().join(" ");

    let app = Application::new().with_assets(Assets);
    // Clicking the Dock icon when the window is closed brings it back.
    app.on_reopen(|cx| {
        open_main(cx, None);
    });
    app.run(move |cx: &mut App| {
        theme::choose_fonts(cx);
        settings::install(cx);
        updates::start(cx);
        app_menu::install(cx);
        popover::install(cx);
        cx.set_global(MainWindow(None));

        // With the icon in the menu bar the app outlives its window. Without
        // the icon, closing the last window quits, as before.
        tray::install(cx);
        // Started at login with the icon showing, the app waits there; the
        // index is still brought up to date and summaries still written.
        if at_login && tray::showing(cx) {
            background::start(cx);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() && !tray::showing(cx) {
                    cx.quit();
                }
            })
            .detach();
            return;
        }
        let opened = open_main_with(cx, None, query).expect("opening the main window");
        background::start(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() && !tray::showing(cx) {
                cx.quit();
            }
        })
        .detach();

        // Development aid: `SCROLLBACK_DEV_SETTINGS=sources` opens the settings
        // window on that page at launch.
        if let Ok(page) = std::env::var("SCROLLBACK_DEV_SETTINGS") {
            let tab = match page.as_str() {
                "sources" => settings::Tab::Sources,
                "summaries" => settings::Tab::Summaries,
                "about" => settings::Tab::About,
                _ => settings::Tab::General,
            };
            settings::open(cx, tab);
        }

        // Development aid: `SCROLLBACK_DEV_HIDE=opencode` switches that source
        // off through the settings shortly after launch, to check that the
        // session window follows a change made elsewhere.
        if let Ok(slug) = std::env::var("SCROLLBACK_DEV_HIDE") {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                cx.update(|cx| {
                    let mut config = cx.global::<settings::Settings>().0.clone();
                    config.hidden_sources.push(slug);
                    settings::store(config, cx);
                })
                .ok();
            })
            .detach();
        }

        // Development aid: `SCROLLBACK_DEV_POPOVER=1` opens the popover at
        // launch, under the menu bar icon when there is one.
        let with_popover = std::env::var_os("SCROLLBACK_DEV_POPOVER").is_some();
        if with_popover {
            cx.spawn(async move |cx| {
                // The icon takes a moment to find its place in the menu bar.
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(400))
                    .await;
                cx.update(|cx| {
                    let icon = tray::rect(cx);
                    eprintln!("menu bar icon at {icon:?}");
                    let started = std::time::Instant::now();
                    popover::toggle(icon, cx);
                    eprintln!("popover opened in {:.1?}", started.elapsed());
                })
                .ok();
            })
            .detach();
        }

        // Development aid: `SCROLLBACK_DEV_KEYS="enter r u n"` presses those
        // keys through a window's own key handling shortly after launch and
        // logs what its view holds after each, so typing can be checked in a
        // window that never takes the keyboard. With the popover open, the
        // keys go to it instead of the session window.
        if let Ok(keys) = std::env::var("SCROLLBACK_DEV_KEYS") {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(900))
                    .await;
                for key in keys.split_whitespace() {
                    let Ok(keystroke) = gpui::Keystroke::parse(key) else {
                        eprintln!("key {key}: not a key");
                        continue;
                    };
                    let panel = cx.update(|cx| popover::window(cx)).ok().flatten();
                    let target: gpui::AnyWindowHandle = match panel.filter(|_| with_popover) {
                        Some(panel) => panel.into(),
                        None => opened.into(),
                    };
                    let handled = cx
                        .update_window(target, |_, window, cx| window.dispatch_keystroke(keystroke, cx))
                        .unwrap_or(false);
                    let state = match panel.filter(|_| with_popover) {
                        Some(panel) => panel.read_with(cx, |panel, _| panel.dev_state()),
                        None => opened.read_with(cx, |home, _| home.dev_state()),
                    }
                    .unwrap_or_else(|_| "window closed".to_string());
                    // Whether the keyboard is on the window itself or has
                    // moved to one of its controls.
                    let home_focus = opened.read_with(cx, |home, _| home.focus.clone()).ok();
                    let on_control = cx
                        .update_window(opened.into(), |_, window, cx| {
                            window.focused(cx).is_some()
                                && !home_focus.as_ref().is_some_and(|f| f.is_focused(window))
                        })
                        .unwrap_or(false);
                    eprintln!("key {key}: handled={handled} on_control={on_control} {state}");
                }
            })
            .detach();
        }
    });
}
