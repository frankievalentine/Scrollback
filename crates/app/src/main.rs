mod home;
mod markdown;
mod theme;

use gpui::{
    point, prelude::*, px, size, App, Application, AssetSource, Bounds, SharedString,
    TitlebarOptions, WindowBounds, WindowOptions,
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

fn main() {
    // The index refresh takes well under a second, so it runs before the
    // window opens and the first frame already shows real sessions.
    let (sessions, status) = home::load();
    // `scrollback-app some words` opens with that search already typed.
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        // Development aid: `SCROLLBACK_HEIGHT=1300` opens a taller window, to
        // see a whole view in one screenshot.
        let height = std::env::var("SCROLLBACK_HEIGHT")
            .ok()
            .and_then(|h| h.parse::<f32>().ok())
            .unwrap_or(820.);
        let bounds = Bounds::centered(None, size(px(1180.), px(height)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(480.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("scrollback".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(16.))),
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| Home::new(sessions, status, query, cx));
                view.update(cx, |home, cx| home.watch_window(window, cx));
                window.focus(&view.read(cx).focus);
                view
            },
        )
        .expect("opening the main window");

        // Development aid: `SCROLLBACK_DEV_KEYS="enter r u n"` presses those
        // keys through the window's own key handling shortly after launch and
        // logs what the view holds after each, so typing can be checked in a
        // window that never takes the keyboard.
        if let Ok(keys) = std::env::var("SCROLLBACK_DEV_KEYS") {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(700))
                    .await;
                for key in keys.split_whitespace() {
                    let Ok(keystroke) = gpui::Keystroke::parse(key) else {
                        eprintln!("key {key}: not a key");
                        continue;
                    };
                    let handled = cx
                        .update_window(opened.into(), |_, window, cx| {
                            window.dispatch_keystroke(keystroke, cx)
                        })
                        .unwrap_or(false);
                    let state = opened
                        .read_with(cx, |home, _| home.dev_state())
                        .unwrap_or_default();
                    eprintln!("key {key}: handled={handled} {state}");
                }
            })
            .detach();
        }

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // SCROLLBACK_BACKGROUND=1 opens the window without taking focus, for
        // checking the app while someone is typing elsewhere.
        if std::env::var_os("SCROLLBACK_BACKGROUND").is_none() {
            cx.activate(true);
        }
    });
}
