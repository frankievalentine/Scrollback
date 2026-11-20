//! The application's commands: what the menu bar lists, the shortcuts bound
//! to them, and the ones that act on the app as a whole. Commands that act on
//! the session window are handled by the window itself.
//!
//! On macOS GPUI turns this list into the real menu bar. On Windows and Linux
//! it only keeps the list, for the app to draw.

use crate::settings::{self, Tab};
use gpui::{actions, App, KeyBinding, Menu, MenuItem, SystemMenuType};

actions!(
    scrollback,
    [
        About,
        OpenSettings,
        Hide,
        HideOthers,
        ShowAll,
        Quit,
        Reindex,
        CloseWindow,
        Minimize,
        Zoom,
        ShowWindow,
        ShowSessions,
        ShowActivity,
        ReadTranscript,
        CopyResume,
        WriteSummary,
    ]
);

pub fn install(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &OpenSettings, cx| settings::open(cx, Tab::General));
    cx.on_action(|_: &About, cx| settings::open(cx, Tab::About));
    cx.on_action(|_: &ShowWindow, cx| {
        crate::open_main(cx, None);
    });
    // These act on whichever window is in front.
    cx.on_action(|_: &CloseWindow, cx| in_front(cx, |window| window.remove_window()));
    cx.on_action(|_: &Minimize, cx| in_front(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| in_front(cx, |window| window.zoom_window()));

    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-r", Reindex, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-0", ShowWindow, None),
        KeyBinding::new("cmd-1", ShowSessions, None),
        KeyBinding::new("cmd-2", ShowActivity, None),
        KeyBinding::new("cmd-s", WriteSummary, None),
    ]);

    cx.set_menus(vec![
        Menu {
            name: "scrollback".into(),
            items: vec![
                MenuItem::action("About scrollback", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide scrollback", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit scrollback", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Reindex", Reindex),
                MenuItem::separator(),
                MenuItem::action("Close Window", CloseWindow),
            ],
        },
        Menu {
            name: "Session".into(),
            items: vec![
                MenuItem::action("Read Transcript", ReadTranscript),
                MenuItem::action("Copy Resume Command", CopyResume),
                MenuItem::separator(),
                MenuItem::action("Write Summary", WriteSummary),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Sessions", ShowSessions),
                MenuItem::action("Activity", ShowActivity),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Sessions Window", ShowWindow),
            ],
        },
    ]);
}

fn in_front(cx: &mut App, act: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| act(window)).ok();
    }
}
