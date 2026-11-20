//! A single-line text field, for the few places Scrollback asks for typed
//! text: an address, a model name, an API key. GPUI has no text input of
//! its own, so this keeps to what those need: typing at the end, deleting,
//! pasting, and a hidden mode for keys. It says when its text is settled,
//! on Enter or when the keyboard moves elsewhere, so a setting is saved
//! once rather than on every key.

use crate::theme::{mono, text, Theme};
use crate::ui;
use gpui::{
    div, prelude::*, px, Context, EventEmitter, FocusHandle, KeyDownEvent, SharedString, Window,
};

/// The field's text has been settled.
pub struct Committed(pub String);

pub struct TextField {
    text: String,
    /// The text when the field last settled, to tell whether it changed.
    settled: String,
    placeholder: SharedString,
    /// Show dots instead of the text.
    secret: bool,
    /// Set in a fixed-width font, for addresses and model names.
    code: bool,
    focus: FocusHandle,
}

impl EventEmitter<Committed> for TextField {}

impl TextField {
    pub fn new(
        text: impl Into<String>,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TextField {
        let focus = cx.focus_handle();
        cx.on_blur(&focus, window, |field, _, cx| field.commit(cx)).detach();
        let text = text.into();
        TextField {
            settled: text.clone(),
            text,
            placeholder: placeholder.into(),
            secret: false,
            code: false,
            focus,
        }
    }

    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    pub fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Replace the text, as when a setting changes elsewhere, without
    /// saying it settled.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.settled = self.text.clone();
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        if self.text != self.settled {
            self.settled = self.text.clone();
            cx.emit(Committed(self.text.trim().to_string()));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        match keystroke.key.as_str() {
            // Tab moves on, which settles the field through its blur.
            "tab" => return,
            "enter" => self.commit(cx),
            "escape" => {
                self.text = self.settled.clone();
                window.blur();
            }
            "backspace" => {
                if mods.secondary() || mods.alt {
                    self.text.clear();
                } else {
                    self.text.pop();
                }
            }
            "v" if mods.secondary() => {
                if let Some(pasted) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    // One line only: a pasted key often ends in a newline.
                    self.text.push_str(pasted.lines().next().unwrap_or("").trim());
                }
            }
            _ => {
                if mods.secondary() || mods.control || mods.function {
                    return;
                }
                let Some(typed) = keystroke.key_char.as_deref() else { return };
                if typed.is_empty() || typed.chars().any(char::is_control) {
                    return;
                }
                self.text.push_str(typed);
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        let focused = self.focus.is_focused(window);
        let shown = match self.secret {
            true => "•".repeat(self.text.chars().count().min(32)),
            false => self.text.clone(),
        };
        let accent = t.accent;
        let field = div()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(1.))
            .when(self.code, |el| el.font_family(mono()))
            .text_size(px(if self.code { text::CAPTION } else { text::SMALL }));
        let field = match (shown.is_empty(), focused) {
            (true, true) => field
                .child(ui::caret(true, &t))
                .child(div().flex_shrink_0().whitespace_nowrap().text_color(t.ink3).child(self.placeholder.clone())),
            (true, false) => field
                .child(div().flex_shrink_0().whitespace_nowrap().text_color(t.ink3).child(self.placeholder.clone())),
            (false, focused) => field
                .child(div().flex_shrink_0().whitespace_nowrap().text_color(t.ink).child(SharedString::from(shown)))
                .when(focused, |el| el.child(ui::caret(true, &t))),
        };
        div()
            .id("text-field")
            .track_focus(&self.focus)
            .tab_index(0)
            .on_key_down(cx.listener(Self::on_key))
            .h(px(32.))
            .w_full()
            .min_w(px(0.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(7.))
            .border_1()
            .border_color(if focused { t.accent } else { t.line })
            .bg(t.raise)
            .cursor_text()
            .hover(move |style| style.border_color(accent.opacity(0.6)))
            .child(field)
    }
}
