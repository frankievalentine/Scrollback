mod home;
mod theme;

use gpui::{
    point, prelude::*, px, size, App, Application, AssetSource, Bounds, SharedString,
    TitlebarOptions, WindowBounds, WindowOptions,
};
use home::Home;
use std::borrow::Cow;

/// Harness logos and outcome marks, compiled into the binary.
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
        let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);
        cx.open_window(
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
                window.focus(&view.read(cx).focus);
                view
            },
        )
        .expect("opening the main window");

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
