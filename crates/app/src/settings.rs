//! The settings window, and the settings themselves as the rest of the app
//! sees them. Every change is saved straight away and announced, so the
//! session window follows without being told directly.

use crate::home::{label, logo, nav_row};
use crate::theme::{text, self, Theme, mono, UI_FONT};
use crate::ui::{self, OneLine, Reachable};
use gpui::{
    div, prelude::*, px, size, svg, App, Bounds, Context, Div, FocusHandle, FontWeight,
    Global, PathPromptOptions, SharedString, Stateful, Window, WindowBounds,
    WindowHandle, WindowOptions,
};
use scrollback_core::{sources, summarize, Config, Harness, Origin, QUIET_MINUTES, SUMMARY_MODELS};
use std::path::Path;

/// The saved settings. Anything that needs to follow changes observes this.
pub struct Settings(pub Config);

impl Global for Settings {}

/// The settings window, while it is open.
struct OpenWindow(Option<WindowHandle<SettingsView>>);

impl Global for OpenWindow {}

/// Load the settings and make them available to the whole app.
pub fn install(cx: &mut App) {
    let config = Config::load();
    theme::set_mode(&config.theme);
    cx.set_global(Settings(config));
    cx.set_global(OpenWindow(None));
}

/// Save new settings and tell everything that follows them.
pub fn store(config: Config, cx: &mut App) {
    if let Err(err) = config.save() {
        eprintln!("could not save settings: {err:#}");
    }
    theme::set_mode(&config.theme);
    cx.set_global(Settings(config));
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    General,
    Sources,
    Summaries,
    About,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::General, Tab::Sources, Tab::Summaries, Tab::About];

    fn name(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Sources => "Sources",
            Tab::Summaries => "Summaries",
            Tab::About => "About",
        }
    }
}

/// Bring the settings window forward on `tab`, opening it if need be.
pub fn open(cx: &mut App, tab: Tab) {
    let existing = cx.global::<OpenWindow>().0;
    if let Some(handle) = existing {
        let shown = handle.update(cx, |view, window, cx| {
            view.tab = tab;
            window.activate_window();
            cx.notify();
        });
        if shown.is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(680.), px(520.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(600.), px(420.))),
            titlebar: Some(ui::titlebar("Settings")),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| SettingsView::new(tab, cx));
            window.focus(&view.read(cx).focus);
            view
        },
    );
    if let Ok(handle) = opened {
        cx.set_global(OpenWindow(Some(handle)));
        // SCROLLBACK_BACKGROUND keeps development runs from taking the keyboard.
        if std::env::var_os("SCROLLBACK_BACKGROUND").is_none() {
            cx.activate(true);
        }
    }
}

pub struct SettingsView {
    tab: Tab,
    config: Config,
    scroll: gpui::ScrollHandle,
    measured: ui::Measured,
    focus: FocusHandle,
}

impl SettingsView {
    fn new(tab: Tab, cx: &mut Context<Self>) -> SettingsView {
        // The session window can change settings too (its auto-summarise
        // switch, the first-run answer), so follow them here as well.
        cx.observe_global::<Settings>(|this, cx| {
            this.config = cx.global::<Settings>().0.clone();
            cx.notify();
        })
        .detach();
        SettingsView {
            tab,
            config: cx.global::<Settings>().0.clone(),
            scroll: gpui::ScrollHandle::new(),
            measured: Default::default(),
            focus: cx.focus_handle(),
        }
    }

    /// Apply one change and save it.
    fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Config)) {
        edit(&mut self.config);
        store(self.config.clone(), cx);
        cx.notify();
    }

    fn choose_folder(&mut self, harness: Harness, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                this.change(cx, |config| {
                    config
                        .folders
                        .insert(harness.slug().to_string(), folder.to_string_lossy().into_owned());
                });
            })
            .ok();
        })
        .detach();
    }

    fn general(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let auto = self.config.auto_summarize;
        let in_menu_bar = self.config.menu_bar_icon;
        let quiet = self.config.quiet_minutes;
        let live = self.config.live_summaries;
        let mac = cfg!(target_os = "macos");
        let mut wait = segmented(t);
        for minutes in QUIET_MINUTES {
            let name = match minutes {
                60 => "1 hour".to_string(),
                m => format!("{m} min"),
            };
            wait = wait.child(
                segment(("quiet", minutes as usize), name, quiet == minutes, t).on_click(
                    cx.listener(move |this, _, _, cx| this.change(cx, |c| c.quiet_minutes = minutes)),
                ),
            );
        }
        let mut look = segmented(t);
        for (i, mode) in ["system", "light", "dark"].into_iter().enumerate() {
            let name = match mode {
                "light" => "Light",
                "dark" => "Dark",
                _ => "Match system",
            };
            look = look.child(
                segment(("theme", i), name, self.config.theme == mode, t).on_click(
                    cx.listener(move |this, _, _, cx| this.change(cx, |c| c.theme = mode.to_string())),
                ),
            );
        }
        page()
            .child(row(
                "Summarise automatically",
                "Write a summary for each session once it has gone quiet, without being asked.",
                switch("auto", auto, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.change(cx, |c| c.auto_summarize = !auto))),
                t,
            ))
            .child(row(
                "Summarise after",
                "How long a session must be idle to count as finished.",
                wait,
                t,
            ))
            .child(row(
                "Summarise sessions in use",
                "Also summarise a session a couple of minutes in, and again every ten \
                 minutes it moves on, so it is in the menu bar list while you work. Uses \
                 more requests.",
                switch("live", live, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.change(cx, |c| c.live_summaries = !live))),
                t,
            ))
            .child(row("Appearance", "Light, dark, or whatever the system is set to.", look, t))
            .child(row(
                if mac { "Menu bar icon" } else { "Tray icon" },
                if mac {
                    "Keep scrollback in the menu bar. It stays running there when its window \
                     is closed, and a click opens recent sessions."
                } else {
                    "Keep scrollback in the system tray. It stays running there when its \
                     window is closed, and a click opens recent sessions."
                },
                switch("menu-bar-icon", in_menu_bar, t).on_click(
                    cx.listener(move |this, _, _, cx| this.change(cx, |c| c.menu_bar_icon = !in_menu_bar)),
                ),
                t,
            ))
    }

    fn sources(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let home = scrollback_core::home();
        let shown = |path: &Path| match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        };

        let mut list = div().flex().flex_col();
        for origin in Origin::ALL {
            let slug = origin.slug();
            let hidden = self.config.hidden_sources.iter().any(|s| s == slug);
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .h(px(38.))
                    .child(logo(origin, t, 16.))
                    .child(div().flex_1().child(origin.name()))
                    .child(switch(slug, !hidden, t).on_click(cx.listener(move |this, _, _, cx| {
                        this.change(cx, |config| {
                            if hidden {
                                config.hidden_sources.retain(|s| s != slug);
                            } else {
                                config.hidden_sources.push(slug.to_string());
                            }
                        })
                    }))),
            );
        }

        let mut folders = div().flex().flex_col().gap(px(14.));
        for harness in Harness::ALL {
            let slug = harness.slug();
            let chosen = self.config.folders.contains_key(slug);
            let folder = sources::location(harness);
            let found = folder.exists();
            let shared = match harness {
                Harness::ClaudeCode => "Claude Code and Claude",
                Harness::Codex => "Codex and ChatGPT",
                other => other.name(),
            };
            let mut buttons = div().flex().flex_shrink_0().gap(px(6.)).child(
                button(("choose", harness as usize), "Choose…", t)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_folder(harness, cx))),
            );
            if chosen {
                buttons = buttons.child(
                    button(("reset", harness as usize), "Reset", t).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.change(cx, |config| {
                                config.folders.remove(slug);
                            })
                        },
                    )),
                );
            }
            folders = folders.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().child(shared))
                            .child(
                                div()
                                    .id(("folder", harness as usize))
                                    .tooltip(ui::tip(folder.display().to_string()))
                                    .one_line()
                                    .font_family(mono())
                                    .text_size(px(text::CAPTION))
                                    .text_color(t.ink3)
                                    .child(format!(
                                        "{}{}",
                                        ui::middle(&shown(&folder), 46),
                                        if found { "" } else { "  (not found)" }
                                    )),
                            ),
                    )
                    .child(buttons),
            );
        }

        page()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(label("Show sessions from", t))
                    .child(list),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .pt(px(18.))
                    .border_t_1()
                    .border_color(t.line)
                    .child(label("Where sessions are read from", t))
                    .child(folders),
            )
    }

    fn summaries(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let mut models = segmented(t);
        for (i, model) in SUMMARY_MODELS.into_iter().enumerate() {
            let mut name = model.to_string();
            if let Some(first) = name.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            models = models.child(
                segment(("model", i), name, self.config.model() == model, t).on_click(cx.listener(
                    move |this, _, _, cx| this.change(cx, |c| c.summary_model = model.to_string()),
                )),
            );
        }
        let found = summarize::claude_binary();
        let tool = match &found {
            Some(path) => format!("Found at {}", path.display()),
            None => "Not found. Summaries cannot be written until Claude Code is installed.".to_string(),
        };
        page()
            .child(row(
                "Written by",
                "Haiku is the quickest and uses the least of your Claude plan. \
                 Sonnet and Opus read more carefully and use more.",
                models,
                t,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().font_weight(FontWeight::MEDIUM).child("The claude command"))
                    .child(
                        div()
                            .font_family(mono())
                            .text_size(px(text::CAPTION))
                            .text_color(t.ink3)
                            .child(tool),
                    ),
            )
            .child(
                div()
                    .pt(px(18.))
                    .border_t_1()
                    .border_color(t.line)
                    .text_size(px(text::SMALL))
                    .line_height(px(20.))
                    .text_color(t.ink2)
                    .child(
                        "A summary request sends your prompts, the agent's replies, and the names \
                         of the tools and files involved. Tool output and file contents are \
                         never sent. Requests go through the Claude CLI and count towards \
                         your Claude plan.",
                    ),
            )
    }

    fn about(&self, t: &Theme) -> Div {
        let index = scrollback_core::data_dir().join("index.db");
        page()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(svg().path("mark.svg").size(px(36.)).text_color(t.accent))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(text::HEADLINE))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("scrollback"),
                            )
                            .child(
                                div()
                                    .font_family(mono())
                                    .text_size(px(text::CAPTION))
                                    .text_color(t.ink3)
                                    .child(format!("version {}", env!("CARGO_PKG_VERSION"))),
                            ),
                    ),
            )
            .child(
                div()
                    .text_color(t.ink2)
                    .line_height(px(22.))
                    .child(
                        "Every coding-agent session on this computer in one place, each with a \
                         summary of what happened and what was left open.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .pt(px(18.))
                    .border_t_1()
                    .border_color(t.line)
                    .child(label("Index", t))
                    .child(
                        div()
                            .font_family(mono())
                            .text_size(px(text::CAPTION))
                            .text_color(t.ink3)
                            .child(index.display().to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(text::SMALL))
                            .text_color(t.ink2)
                            .child("scrollback reads session files where they are and never writes to them."),
                    ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        ui::settle(&[&self.scroll], &self.measured, window, cx);
        let mut tabs = div()
            .w(px(168.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(ui::TOP_INSET))
            .px(px(10.))
            .bg(t.rail)
            .border_r_1()
            .border_color(t.line);
        for tab in Tab::ALL {
            tabs = tabs.child(
                nav_row(&t, self.tab == tab)
                    .id(tab.name())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
                    .child(tab.name()),
            );
        }
        let content = match self.tab {
            Tab::General => self.general(&t, cx),
            Tab::Sources => self.sources(&t, cx),
            Tab::Summaries => self.summaries(&t, cx),
            Tab::About => self.about(&t),
        };
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| {
                ui::control_key(event, &this.focus, window, cx);
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| ui::pointer_used(&this.focus, window)),
            )
            .size_full()
            .flex()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .line_height(px(20.))
            .child(tabs)
            .child(
                div().relative().flex_1().min_w(px(0.)).h_full().child(
                div()
                    .id("settings-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .pt(px(ui::TOP_INSET))
                    .px(px(32.))
                    .pb(px(32.))
                    .child(
                        div()
                            .pb(px(22.))
                            .text_size(px(text::TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.tab.name()),
                    )
                    .child(content))
                    .children(ui::scrollbar(&self.scroll, &t)),
            )
    }
}

fn page() -> Div {
    div().flex().flex_col().gap(px(24.))
}

/// One setting: its name and a line about it on the left, its control on the right.
fn row(name: &'static str, about: &'static str, control: impl IntoElement, t: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(20.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().font_weight(FontWeight::MEDIUM).child(name))
                .child(
                    div()
                        .text_size(px(text::SMALL))
                        .line_height(px(19.))
                        .text_color(t.ink3)
                        .child(about),
                ),
        )
        .child(div().flex_shrink_0().child(control))
}

/// An on/off switch.
fn switch(id: impl Into<gpui::ElementId>, on: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .reachable(t)
        .w(px(38.))
        .h(px(22.))
        .flex_shrink_0()
        .p(px(2.))
        .flex()
        .when(on, |el| el.justify_end())
        .rounded_full()
        .bg(if on { t.accent } else { t.control_off })
        .cursor_pointer()
        .child(div().size(px(18.)).rounded_full().bg(if on { t.on_accent } else { t.raise }))
}

/// The track a set of mutually exclusive choices sits on.
fn segmented(t: &Theme) -> Div {
    div()
        .flex()
        .gap(px(2.))
        .p(px(3.))
        .rounded(px(9.))
        .bg(t.fill)
        .border_1()
        .border_color(t.line)
}

fn segment(
    id: impl Into<gpui::ElementId>,
    name: impl Into<SharedString>,
    chosen: bool,
    t: &Theme,
) -> Stateful<Div> {
    let picked = t.segment;
    div()
        .id(id)
        .reachable(t)
        .h(px(28.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .text_size(px(text::SMALL))
        .text_color(if chosen { t.ink } else { t.ink2 })
        .cursor_pointer()
        .when(chosen, |el| el.bg(picked).font_weight(FontWeight::MEDIUM))
        .child(name.into())
}

fn button(id: impl Into<gpui::ElementId>, name: &'static str, t: &Theme) -> Stateful<Div> {
    let fill = t.fill;
    div()
        .id(id)
        .reachable(t)
        .h(px(30.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(7.))
        .border_1()
        .border_color(t.line)
        .text_size(px(text::SMALL))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(move |style| style.bg(fill))
        .active(|style| style.opacity(0.75))
        .child(name)
}
