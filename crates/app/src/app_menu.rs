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

    // "secondary" is the command key on a Mac and the control key elsewhere.
    let mut keys = vec![
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-r", Reindex, None),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("secondary-0", ShowWindow, None),
        KeyBinding::new("secondary-1", ShowSessions, None),
        KeyBinding::new("secondary-2", ShowActivity, None),
        KeyBinding::new("secondary-s", WriteSummary, None),
    ];
    if cfg!(target_os = "macos") {
        // Hiding the app and minimising from the keyboard are Mac habits;
        // elsewhere these keys mean other things.
        keys.extend([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
        ]);
    }
    cx.bind_keys(keys);
    cx.set_menus(menus());
}

/// The menus, laid out the way each system expects: a Mac has a menu named
/// after the app and a Window menu; elsewhere Settings and Quit sit under
/// File, and About under Help.
fn menus() -> Vec<Menu> {
    let mac = cfg!(target_os = "macos");
    let mut file = vec![MenuItem::action("Reindex", Reindex), MenuItem::separator()];
    if !mac {
        file.extend([MenuItem::action("Settings…", OpenSettings), MenuItem::separator()]);
    }
    file.push(MenuItem::action("Close Window", CloseWindow));
    if !mac {
        file.push(MenuItem::action("Quit", Quit));
    }
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu {
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
        });
    }
    menus.extend([
        Menu { name: "File".into(), items: file },
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
    ]);
    menus.push(if mac {
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Sessions Window", ShowWindow),
            ],
        }
    } else {
        Menu {
            name: "Help".into(),
            items: vec![MenuItem::action("About scrollback", About)],
        }
    });
    menus
}

fn in_front(cx: &mut App, act: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| act(window)).ok();
    }
}
