//! The parts of a window the system supplies on a Mac and the app has to
//! draw elsewhere: the menu bar, and on Linux desktops that leave the frame
//! to the app (GNOME under Wayland, for one), the title bar's buttons and
//! the edges a window is resized by.
//!
//! The menus drawn here are the same list the Mac's menu bar is built from,
//! in `app_menu`, so the two cannot drift apart.

use crate::theme::{mono, text, Theme};
use crate::ui::{OneLine, Reachable};
use gpui::{
    canvas, deferred, div, point, prelude::*, px, Action, App, Bounds, Context, CursorStyle,
    Decorations, Div, FontWeight, HitboxBehavior, Keystroke, MouseButton, OwnedMenu,
    OwnedMenuItem, Pixels, Point, ResizeEdge, SharedString, Size, Tiling, Window,
};

/// How tall the bar across the top of a window is.
pub const BAR: f32 = 34.;
/// How close to a window's edge the pointer resizes it.
const EDGE: f32 = 5.;

/// A view with a menu bar: it remembers which menu is open.
pub trait Menus: Sized + 'static {
    fn open_menu(&mut self) -> &mut Option<usize>;
}

/// Whether the menus are drawn in the window. On a Mac they are in the
/// system's menu bar; `SCROLLBACK_DEV_MENU=1` draws them there too, to see
/// them without another computer (`0` draws them with none open).
pub fn in_window() -> bool {
    !cfg!(target_os = "macos") || std::env::var_os("SCROLLBACK_DEV_MENU").is_some()
}

/// Development aid: `SCROLLBACK_DEV_MENU=2` also opens the second menu at
/// launch, so it can be seen in a screenshot.
pub fn open_at_launch() -> Option<usize> {
    let which: usize = std::env::var("SCROLLBACK_DEV_MENU").ok()?.parse().ok()?;
    which.checked_sub(1)
}

/// Which of a window's sides are held by the screen's edges or a
/// neighbouring window, when the system has left the window's frame to the
/// app; `None` when the system draws the frame. `SCROLLBACK_DEV_FRAME=1`
/// draws the app's frame regardless, to see it on any desktop.
fn own_frame(window: &Window) -> Option<Tiling> {
    match window.window_decorations() {
        Decorations::Client { tiling } => Some(tiling),
        Decorations::Server => std::env::var_os("SCROLLBACK_DEV_FRAME").map(|_| Tiling::default()),
    }
}

/// The bar across the top of a window: the menus when `menus` is set, and
/// the title and window buttons when the system draws no title bar. `None`
/// when the window needs neither.
pub fn bar<V: Menus>(
    title: &'static str,
    menus: bool,
    open: Option<usize>,
    window: &mut Window,
    cx: &mut Context<V>,
    t: &Theme,
) -> Option<Div> {
    let menus = menus && in_window();
    let frameless = own_frame(window).is_some();
    if !menus && !frameless {
        return None;
    }
    let mut bar = div()
        .h(px(BAR))
        .flex_shrink_0()
        .flex()
        .items_center()
        .px(px(6.))
        .bg(t.rail)
        .border_b_1()
        .border_color(t.line)
        .text_size(px(text::SMALL));
    if menus {
        for (index, menu) in cx.get_menus().unwrap_or_default().into_iter().enumerate() {
            bar = bar.child(menu_button(index, menu, open == Some(index), open.is_some(), window, cx, t));
        }
    } else {
        bar = bar.child(
            div()
                .px(px(8.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.ink2)
                .child(title),
        );
    }
    // The rest of the bar is what a title bar is: something to drag the
    // window by, and to double-click to make it fill the screen.
    let mut handle = div().id("window-handle").flex_1().h_full();
    if frameless {
        handle = handle
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count < 2 {
                    window.start_window_move();
                }
            })
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.zoom_window();
                }
            });
    }
    bar = bar.child(handle);
    if frameless {
        bar = bar.child(window_buttons(window, t));
    }
    Some(bar)
}

/// One name in the menu bar, with its list hanging below while it is open.
fn menu_button<V: Menus>(
    index: usize,
    menu: OwnedMenu,
    open: bool,
    any_open: bool,
    window: &mut Window,
    cx: &mut Context<V>,
    t: &Theme,
) -> Div {
    let fill = t.fill;
    let name = div()
        .id(("menu", index))
        .reachable(t)
        .h(px(24.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .cursor_pointer()
        .when(open, |el| el.bg(fill))
        .hover(move |style| style.bg(fill))
        .on_click(cx.listener(move |this, _, _, cx| {
            let now = this.open_menu();
            *now = if *now == Some(index) { None } else { Some(index) };
            cx.notify();
        }))
        // With one menu open, moving along the bar opens the others.
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered && any_open && *this.open_menu() != Some(index) {
                *this.open_menu() = Some(index);
                cx.notify();
            }
        }))
        .child(menu.name.clone());
    let mut holder = div().relative().child(name);
    if open {
        let mut list = div()
            .id(("menu-list", index))
            .absolute()
            .top(px(28.))
            .left(px(0.))
            .min_w(px(230.))
            .occlude()
            .flex()
            .flex_col()
            .p(px(5.))
            .rounded(px(9.))
            .bg(t.raise)
            .border_1()
            .border_color(t.line)
            .shadow(t.card_shadow.clone())
            // A click anywhere else puts the menu away. Clicks on the bar
            // are left to the names there, which open and close menus.
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                if event.position.y > px(BAR) && this.open_menu().take().is_some() {
                    cx.notify();
                }
            }));
        for (row, item) in menu.items.into_iter().enumerate() {
            list = match item {
                OwnedMenuItem::Action { name, action, .. } => {
                    list.child(menu_item(index, row, name, action, window, cx, t))
                }
                OwnedMenuItem::Separator => {
                    list.child(div().h(px(1.)).my(px(4.)).mx(px(6.)).bg(t.line))
                }
                // Nothing here has menus inside menus, and the system's own
                // menus exist only on a Mac.
                OwnedMenuItem::Submenu(_) | OwnedMenuItem::SystemMenu(_) => list,
            };
        }
        holder = holder.child(deferred(list).with_priority(1));
    }
    holder
}

/// One command in an open menu, with its shortcut beside it.
fn menu_item<V: Menus>(
    menu: usize,
    row: usize,
    name: String,
    action: Box<dyn Action>,
    window: &mut Window,
    cx: &mut Context<V>,
    t: &Theme,
) -> impl IntoElement {
    let keys = window
        .bindings_for_action(action.as_ref())
        .last()
        .map(|binding| {
            let keys: Vec<String> = binding
                .keystrokes()
                .iter()
                .map(|keystroke| key_label(keystroke.inner()))
                .collect();
            keys.join(" ")
        })
        .unwrap_or_default();
    let fill = t.fill;
    div()
        .id(("menu-item", menu * 100 + row))
        .reachable(t)
        .h(px(28.))
        .px(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(28.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |style| style.bg(fill))
        .on_click(cx.listener(move |this, _, window, cx| {
            *this.open_menu() = None;
            cx.notify();
            window.dispatch_action(action.boxed_clone(), cx);
        }))
        .child(div().one_line().child(SharedString::from(name)))
        .child(
            div()
                .flex_shrink_0()
                .font_family(mono())
                .text_size(px(text::CAPTION))
                .text_color(t.ink3)
                .child(keys),
        )
}

/// A shortcut as it is written on this system: `⌘R` on a Mac, `Ctrl+R`
/// elsewhere.
fn key_label(keystroke: &Keystroke) -> String {
    let mods = keystroke.modifiers;
    let key = match keystroke.key.as_str() {
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut letters = key.chars();
            letters.next().map_or(String::new(), |first| first.to_uppercase().chain(letters).collect())
        }
    };
    if cfg!(target_os = "macos") {
        let held: String = [(mods.control, '⌃'), (mods.alt, '⌥'), (mods.shift, '⇧'), (mods.platform, '⌘')]
            .into_iter()
            .filter_map(|(held, sign)| held.then_some(sign))
            .collect();
        format!("{held}{key}")
    } else {
        let mut parts: Vec<&str> = [(mods.control, "Ctrl"), (mods.alt, "Alt"), (mods.shift, "Shift"), (mods.platform, "Super")]
            .into_iter()
            .filter_map(|(held, name)| held.then_some(name))
            .collect();
        parts.push(&key);
        parts.join("+")
    }
}

/// Minimise, fill the screen, and close, for a window the system gave no
/// title bar.
fn window_buttons(window: &Window, t: &Theme) -> Div {
    let ink = t.ink2;
    let fill = t.fill;
    // The close button turns red under the pointer; the others just lift.
    let button = |id: &'static str, closes: bool| {
        div()
            .id(id)
            .reachable(t)
            .size(px(26.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .text_color(ink)
            .cursor_pointer()
            .hover(move |style| match closes {
                true => style.bg(gpui::rgb(0xD0342C)).text_color(gpui::white()),
                false => style.bg(fill),
            })
    };
    let controls = window.window_controls();
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .children(controls.minimize.then(|| {
            button("window-minimise", false)
                .on_click(|_, window, _| window.minimize_window())
                .child(div().w(px(10.)).h(px(1.5)).bg(ink))
        }))
        .children(controls.maximize.then(|| {
            button("window-fill", false)
                .on_click(|_, window, _| window.zoom_window())
                .child(div().size(px(9.)).rounded(px(2.)).border_1().border_color(ink))
        }))
        .child(
            button("window-close", true)
                .on_click(|_, window, _| window.remove_window())
                .child(div().text_size(px(17.)).line_height(px(17.)).child("×")),
        )
}

/// Finish a window's root for a desktop that leaves the frame to the app:
/// a hairline around it, and edges it can be resized by. Elsewhere the root
/// is returned as it came.
pub fn frame(root: Div, window: &mut Window, t: &Theme) -> Div {
    let Some(tiling) = own_frame(window) else {
        return root;
    };
    // A window filling the screen, or snapped to a side, has no free edges.
    if tiling.is_tiled() || window.is_maximized() || window.is_fullscreen() {
        return root;
    }
    root.relative()
        .border_1()
        .border_color(t.line)
        // Seen first, so an edge wins over whatever lies under it.
        .capture_any_mouse_down(|event, window, cx| {
            if event.button != MouseButton::Left {
                return;
            }
            if let Some(edge) = edge_at(event.position, window.viewport_size()) {
                window.start_window_resize(edge);
                cx.stop_propagation();
            }
        })
        .on_mouse_move(|event, window, _| {
            // Only near an edge is there a pointer to change.
            let size = window.viewport_size();
            let near = |at: Pixels, length: Pixels| at < px(EDGE * 3.) || at > length - px(EDGE * 3.);
            if near(event.position.x, size.width) || near(event.position.y, size.height) {
                window.refresh();
            }
        })
        .child(
            canvas(
                |_, window, _| {
                    let all = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                    window.insert_hitbox(all, HitboxBehavior::Normal)
                },
                |_, hitbox, window, _| {
                    let Some(edge) = edge_at(window.mouse_position(), window.viewport_size()) else {
                        return;
                    };
                    let pointer = match edge {
                        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
                        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
                        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
                    };
                    window.set_cursor_style(pointer, &hitbox);
                },
            )
            .absolute()
            .size_full(),
        )
}

/// Which edge or corner of a window of `size` the point `at` is on, if any.
fn edge_at(at: Point<Pixels>, size: Size<Pixels>) -> Option<ResizeEdge> {
    let edge = px(EDGE);
    // Corners reach further along each side, as they are harder to hit.
    let corner = px(EDGE * 3.);
    let (left, right) = (at.x < edge, at.x > size.width - edge);
    let (top, bottom) = (at.y < edge, at.y > size.height - edge);
    let (near_left, near_right) = (at.x < corner, at.x > size.width - corner);
    let (near_top, near_bottom) = (at.y < corner, at.y > size.height - corner);
    Some(match () {
        _ if (top && near_left) || (left && near_top) => ResizeEdge::TopLeft,
        _ if (top && near_right) || (right && near_top) => ResizeEdge::TopRight,
        _ if (bottom && near_left) || (left && near_bottom) => ResizeEdge::BottomLeft,
        _ if (bottom && near_right) || (right && near_bottom) => ResizeEdge::BottomRight,
        _ if top => ResizeEdge::Top,
        _ if bottom => ResizeEdge::Bottom,
        _ if left => ResizeEdge::Left,
        _ if right => ResizeEdge::Right,
        _ => return None,
    })
}

/// How a window asks to be decorated: the system's frame, unless
/// `SCROLLBACK_DEV_FRAME=1` asks for the app's own.
pub fn decorations() -> Option<gpui::WindowDecorations> {
    std::env::var_os("SCROLLBACK_DEV_FRAME").map(|_| gpui::WindowDecorations::Client)
}

/// Close an open menu, for Escape. Returns whether one was open.
pub fn close<V: Menus>(view: &mut V, cx: &mut Context<V>) -> bool {
    let was_open = view.open_menu().take().is_some();
    if was_open {
        cx.notify();
    }
    was_open
}

#[allow(dead_code)]
fn _uses(_: &App) {}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn edges_and_corners_are_found() {
        let window = size(px(800.), px(600.));
        let at = |x: f32, y: f32| edge_at(point(px(x), px(y)), window);
        assert_eq!(at(400., 300.), None);
        assert_eq!(at(400., 2.), Some(ResizeEdge::Top));
        assert_eq!(at(798., 300.), Some(ResizeEdge::Right));
        assert_eq!(at(2., 2.), Some(ResizeEdge::TopLeft));
        // A corner reaches a little way along each side.
        assert_eq!(at(790., 598.), Some(ResizeEdge::BottomRight));
        assert_eq!(at(2., 590.), Some(ResizeEdge::BottomLeft));
    }
}
