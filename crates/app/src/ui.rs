//! Small pieces every window shares: the keyboard focus ring, the scrollbar
//! that shows when a list has more below, tooltips, and shortening long
//! paths so the end stays readable.

use crate::theme::{text, Theme, mono, UI_FONT};
use gpui::{
    div, point, prelude::*, px, AnyView, App, BoxShadow, Context, Div, InteractiveElement,
    ScrollHandle, SharedString, Styled, Window,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Half a blink of a search caret.
/// How a shortcut is written for the system the app is running on: `⌘R` on
/// a Mac, `Ctrl+R` elsewhere.
pub fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

/// A shortcut with shift held: `⇧space` on a Mac, `Shift+space` elsewhere.
pub fn shifted(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⇧{key}")
    } else {
        format!("Shift+{key}")
    }
}

/// The title bar of a window. On a Mac the content runs up under a clear
/// title bar with the window's buttons set into it; elsewhere the system
/// draws its usual title bar above the content.
pub fn titlebar(title: &'static str) -> gpui::TitlebarOptions {
    let mac = cfg!(target_os = "macos");
    gpui::TitlebarOptions {
        title: Some(title.into()),
        appears_transparent: mac,
        traffic_light_position: mac.then(|| gpui::point(px(16.), px(16.))),
    }
}

/// Room left above a window's content: on a Mac, enough to clear the
/// window's buttons.
pub const TOP_INSET: f32 = if cfg!(target_os = "macos") { 52. } else { 20. };

pub const CARET_BLINK: std::time::Duration = std::time::Duration::from_millis(530);

/// The caret in a search box. It shows that typing lands there: lit and
/// dark in turn, and steady while keys are arriving.
pub fn caret(lit: bool, t: &Theme) -> Div {
    div()
        .w(px(2.))
        .h(px(18.))
        .flex_shrink_0()
        .rounded(px(1.))
        .bg(if lit { t.accent } else { t.accent.opacity(0.) })
}

/// The inside of a search box: what has been typed with the caret after it,
/// or the caret with the placeholder beside it when nothing has been.
///
/// Typed text and placeholder start at the same place, three pixels in, the
/// room the caret takes when it stands in front. So the caret sits against
/// the first letter of the placeholder, as it does in any text field, and
/// the first typed character lands exactly where the placeholder began.
pub fn search_field(query: &str, placeholder: &'static str, lit: bool, t: &Theme) -> Div {
    let field = div().flex().items_center().gap(px(1.)).min_w(px(0.));
    if query.is_empty() {
        field.child(caret(lit, t)).child(
            div()
                .min_w(px(0.))
                .one_line()
                .text_color(t.ink3)
                .child(placeholder),
        )
    } else {
        field
            .pl(px(3.))
            .child(
                div()
                    .min_w(px(0.))
                    .one_line()
                    .text_color(t.ink)
                    .child(SharedString::from(query.to_string())),
            )
            .child(caret(lit, t))
    }
}

/// Whether the keyboard is being used to move between controls. The focus
/// ring is drawn only then, so clicking a button does not leave a ring on it.
static KEYBOARD: AtomicBool = AtomicBool::new(false);

/// Note that Tab was pressed (`true`) or the mouse was used (`false`).
/// Returns whether that changed anything, so the caller can redraw.
pub fn set_keyboard(on: bool) -> bool {
    KEYBOARD.swap(on, Ordering::Relaxed) != on
}

/// What every window does with a key press before anything else, so the
/// keyboard can reach its controls. Tab and Shift-Tab move between them and
/// switch the focus ring on. With a control focused, Enter and Space belong
/// to it (GPUI presses it), and Escape steps back out to `home`, the
/// window's own focus. Returns true when the key has been dealt with.
pub fn control_key(
    event: &gpui::KeyDownEvent,
    home: &gpui::FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let key = event.keystroke.key.as_str();
    let mods = event.keystroke.modifiers;
    if key == "tab" && !mods.platform && !mods.control && !mods.alt {
        set_keyboard(true);
        if mods.shift {
            window.focus_prev();
        } else {
            window.focus_next();
        }
        window.refresh();
        cx.stop_propagation();
        return true;
    }
    if home.is_focused(window) {
        return false;
    }
    match key {
        "enter" | "space" => {
            cx.stop_propagation();
            true
        }
        "escape" => {
            window.focus(home);
            set_keyboard(false);
            window.refresh();
            cx.stop_propagation();
            true
        }
        // Anything else is for the window: give it the keyboard back.
        _ => {
            window.focus(home);
            false
        }
    }
}

/// What every window does on a mouse press: the ring goes away, and the
/// keyboard returns to the window itself, so a later Enter is not taken as
/// another press of whatever was just clicked.
pub fn pointer_used(home: &gpui::FocusHandle, window: &mut Window) {
    window.focus(home);
    if set_keyboard(false) {
        window.refresh();
    }
}

/// Make a clickable element reachable with Tab. GPUI then lets Enter or
/// Space press it, and it gets a ring while the keyboard is on it.
pub fn tab_stop<E: InteractiveElement>(element: E, t: &Theme) -> E {
    let element = element.tab_index(0);
    if !KEYBOARD.load(Ordering::Relaxed) {
        return element;
    }
    // Drawn as shadows so it does not shift the layout: a band of the page
    // colour, then the accent, so the ring stands clear of a button that is
    // itself the accent colour. GPUI draws nothing for a shadow with no blur
    // at all, hence the half pixel.
    let band = |color, spread: f32| BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.5),
        spread_radius: px(spread),
    };
    let ring = vec![band(t.accent, 4.), band(t.bg, 2.)];
    element.focus(move |style| style.shadow(ring))
}

/// `.reachable(t)` on any clickable element: see [`tab_stop`].
pub trait Reachable: InteractiveElement + Sized {
    fn reachable(self, t: &Theme) -> Self {
        tab_stop(self, t)
    }
}

impl<E: InteractiveElement + Sized> Reachable for E {}

/// The style for text that must stay on one line and end in "…" when it
/// does not fit. (GPUI's own `truncate` loses the ellipsis: text that may
/// not wrap is measured once, before its width is known, and never again.)
pub trait OneLine: Styled + Sized {
    fn one_line(self) -> Self {
        self.overflow_hidden().line_clamp(1).text_ellipsis()
    }
}

impl<E: Styled + Sized> OneLine for E {}

/// A slim scrollbar for a scrolling area, shown only when there is more
/// than fits. Put it in a `relative` parent that is the size of the area.
pub fn scrollbar(handle: &ScrollHandle, t: &Theme) -> Option<Div> {
    let view = handle.bounds().size.height;
    let beyond = handle.max_offset().height;
    if view <= px(0.) || beyond <= px(1.) {
        return None;
    }
    let thumb = (view * (view / (view + beyond))).max(px(28.));
    let travelled = (-handle.offset().y / beyond).clamp(0., 1.);
    Some(
        div()
            .absolute()
            .top(px(2.) + (view - thumb - px(4.)) * travelled)
            .right(px(3.))
            .w(px(4.))
            .h(thumb)
            .rounded(px(2.))
            .bg(t.ink3.opacity(0.45)),
    )
}

/// What a view's scrolling areas last measured, to notice when it changes.
pub type Measured = std::rc::Rc<std::cell::Cell<f32>>;

/// Call from `render` with every scroll handle the view draws a scrollbar
/// for. A scrollbar is drawn from the previous frame's measurements, so when
/// laying this frame out changes them (the first frame, a different page, a
/// longer summary), the view is drawn once more to catch up.
pub fn settle<V: 'static>(
    handles: &[&ScrollHandle],
    measured: &Measured,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let handles: Vec<ScrollHandle> = handles.iter().map(|h| (*h).clone()).collect();
    let measured = measured.clone();
    cx.on_next_frame(window, move |_, _, cx| {
        let now: f32 = handles
            .iter()
            .map(|h| f32::from(h.bounds().size.height) * 3. + f32::from(h.max_offset().height))
            .sum();
        if (measured.get() - now).abs() > 0.5 {
            measured.set(now);
            cx.notify();
        }
    });
}

struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        div()
            .max_w(px(560.))
            .px(px(9.))
            .py(px(5.))
            .rounded(px(6.))
            .bg(t.ink)
            .text_color(t.bg)
            .font_family(if self.0.contains(['/', '\\']) { mono() } else { UI_FONT })
            .text_size(px(text::CAPTION))
            .line_height(px(17.))
            .child(self.0.clone())
    }
}

/// A tooltip showing `text` in full, for values the layout has to shorten.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

/// Shorten a path to at most `max` characters by taking the middle out, so
/// both where it starts and the file or folder it ends in stay readable.
pub fn middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max || max < 8 {
        return text.to_string();
    }
    let tail = (max - 1) * 3 / 5;
    let head = max - 1 - tail;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

#[cfg(test)]
mod tests {
    use super::middle;

    #[test]
    fn long_paths_lose_their_middle() {
        assert_eq!(middle("/short/path", 40), "/short/path");
        let cut = middle("/Users/me/Library/Application Support/Claude/scratch/abc/project", 30);
        assert_eq!(cut.chars().count(), 30);
        assert!(cut.starts_with("/Users/me/Li"));
        assert!(cut.ends_with("abc/project"));
        assert!(cut.contains('…'));
    }
}
