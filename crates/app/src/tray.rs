//! The icon in the menu bar (the system tray on Windows and Linux). A click
//! opens the popover; a right-click offers the window, the settings and Quit.
//! While it is there, the app keeps running after its window is closed.
//!
//! The icon itself comes from the `tray-icon` crate, which talks to each
//! operating system. Its events arrive outside GPUI, so they are passed
//! through a channel to a task that can act on the app.

use crate::settings::{self, Settings, Tab};
use crate::{open_main, popover};
use futures::channel::mpsc;
use futures::StreamExt;
use gpui::{App, Global};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, Rect, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// The icon's pixels: 36 by 36, black on clear, drawn from `icon/tray.svg`
/// by `scripts/render-tray.swift`.
const PIXELS: &[u8] = include_bytes!("../icon/tray.rgba");
const SIDE: u32 = 36;

struct Tray {
    icon: TrayIcon,
    open: MenuId,
    settings: MenuId,
    quit: MenuId,
}

/// The icon, while it is showing.
struct Showing(Option<Tray>);

impl Global for Showing {}

enum Signal {
    Clicked(Rect),
    Chose(MenuId),
}

/// Start following the icon's clicks, and show it if the settings say so.
pub fn install(cx: &mut App) {
    cx.set_global(Showing(None));

    let (send, mut receive) = mpsc::unbounded::<Signal>();
    let clicks = send.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            rect,
            ..
        } = event
        {
            clicks.unbounded_send(Signal::Clicked(rect)).ok();
        }
    }));
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        send.unbounded_send(Signal::Chose(event.id)).ok();
    }));
    cx.spawn(async move |cx| {
        while let Some(signal) = receive.next().await {
            cx.update(|cx| act(signal, cx)).ok();
        }
    })
    .detach();

    sync(cx);
    cx.observe_global::<Settings>(sync).detach();
}

/// Show or remove the icon to match the settings.
fn sync(cx: &mut App) {
    // Development runs that must not touch the menu bar can say so.
    let wanted = cx.global::<Settings>().0.menu_bar_icon
        && std::env::var_os("SCROLLBACK_NO_TRAY").is_none();
    let showing = cx.global::<Showing>().0.is_some();
    if wanted == showing {
        return;
    }
    let tray = if wanted { build() } else { None };
    cx.set_global(Showing(tray));
}

fn build() -> Option<Tray> {
    let icon = Icon::from_rgba(PIXELS.to_vec(), SIDE, SIDE).ok()?;
    let open = MenuItem::new("Open scrollback", true, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit scrollback", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &open,
        &settings,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .ok()?;
    let built = TrayIconBuilder::new()
        // As a template, macOS tints it for a light or dark menu bar.
        .with_icon_templated(icon)
        .with_tooltip("scrollback")
        .with_menu(Box::new(menu))
        // A plain click opens the popover; the menu is for the right button.
        .with_menu_on_left_click(false)
        .build();
    match built {
        Ok(icon) => Some(Tray {
            icon,
            open: open.id().clone(),
            settings: settings.id().clone(),
            quit: quit.id().clone(),
        }),
        Err(err) => {
            eprintln!("could not add the menu bar icon: {err}");
            None
        }
    }
}

fn act(signal: Signal, cx: &mut App) {
    match signal {
        Signal::Clicked(rect) => popover::toggle(Some(rect), cx),
        Signal::Chose(id) => {
            let Some(tray) = &cx.global::<Showing>().0 else { return };
            if id == tray.open {
                open_main(cx, None);
            } else if id == tray.settings {
                settings::open(cx, Tab::General);
            } else if id == tray.quit {
                cx.quit();
            }
        }
    }
}

/// Whether the icon is in the menu bar, keeping the app alive.
pub fn showing(cx: &App) -> bool {
    cx.global::<Showing>().0.is_some()
}

/// Where the icon is on screen, in physical pixels, when that is known.
pub fn rect(cx: &App) -> Option<Rect> {
    cx.global::<Showing>().0.as_ref().and_then(|tray| tray.icon.rect())
}
