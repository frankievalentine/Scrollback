//! The icon in the menu bar (the system tray on Windows and Linux). A click
//! opens the popover; a right-click offers the window, the settings and Quit.
//! While it is there, the app keeps running after its window is closed.
//!
//! Not every Linux desktop passes clicks on, so there the menu also offers
//! the popover.
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
    /// Only on Linux, where a click may never arrive.
    recent: Option<MenuId>,
    open: MenuId,
    settings: MenuId,
    quit: MenuId,
}

/// The icon, while it is showing.
struct Showing(Option<Tray>);

impl Global for Showing {}

enum Signal {
    /// A click, with where the icon is when the system says.
    Clicked(Option<Rect>),
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
            position,
            ..
        } = event
        {
            // Some systems give the icon's place, some only where the click
            // landed, and some neither.
            let at = if rect.size.width > 0 {
                Some(rect)
            } else if position.x != 0. || position.y != 0. {
                Some(Rect { position, ..Rect::default() })
            } else {
                None
            };
            clicks.unbounded_send(Signal::Clicked(at)).ok();
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

/// The icon's pixels as the system wants them. A Mac tints a black shape
/// itself to suit a light or dark menu bar. Windows and Linux show the
/// pixels as they are, on a bar that may be light or dark, so there the
/// shape takes the app icon's blue, which reads on both.
fn pixels() -> Vec<u8> {
    let mut pixels = PIXELS.to_vec();
    if !cfg!(target_os = "macos") {
        for pixel in pixels.chunks_exact_mut(4) {
            // Only the colour changes; how solid each pixel is stays.
            pixel[..3].copy_from_slice(&[0x8A, 0xA4, 0xFF]);
        }
    }
    pixels
}

fn build() -> Option<Tray> {
    let icon = Icon::from_rgba(pixels(), SIDE, SIDE).ok()?;
    let recent = cfg!(target_os = "linux").then(|| MenuItem::new("Recent sessions", true, None));
    let open = MenuItem::new("Open scrollback", true, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit scrollback", true, None);
    let menu = Menu::new();
    if let Some(recent) = &recent {
        menu.append(recent).ok()?;
    }
    menu.append_items(&[
        &open,
        &settings,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .ok()?;
    let builder = TrayIconBuilder::new();
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon);
    let built = builder
        .with_tooltip("scrollback")
        .with_menu(Box::new(menu))
        // A plain click opens the popover; the menu is for the right button.
        .with_menu_on_left_click(false)
        .build();
    match built {
        Ok(icon) => Some(Tray {
            icon,
            recent: recent.map(|item| item.id().clone()),
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
        Signal::Clicked(at) => popover::toggle(at, cx),
        Signal::Chose(id) => {
            let Some(tray) = &cx.global::<Showing>().0 else { return };
            if tray.recent.as_ref() == Some(&id) {
                let at = tray.icon.rect();
                popover::toggle(at, cx);
            } else if id == tray.open {
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
