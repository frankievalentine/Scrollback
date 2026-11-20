//! The settings window, and the settings themselves as the rest of the app
//! sees them. Every change is saved straight away and announced, so the
//! session window follows without being told directly.

use crate::chrome;
use crate::home::{label, logo, nav_row};
use crate::theme::{text, self, Theme, mono, UI_FONT};
use crate::ui::{self, OneLine, Reachable};
use gpui::{
    div, prelude::*, px, size, svg, App, Bounds, Context, Div, FocusHandle, FontWeight,
    Global, PathPromptOptions, SharedString, Stateful, Window, WindowBounds,
    WindowHandle, WindowOptions,
};
use crate::field::{Committed, TextField};
use gpui::Entity;
use scrollback_core::provider::{self, CLAUDE_MODELS};
use scrollback_core::{secrets, sources, summarize, Config, Endpoint, Harness, Origin, Provider, QUIET_MINUTES};
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
            app_id: Some(ui::APP_ID.into()),
            window_decorations: chrome::decorations(),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| SettingsView::new(tab, window, cx));
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
    /// Never set: this window has no menus, only the bar they would sit in.
    menu_open: Option<usize>,
    /// Whether Scrollback starts at login, as the system has it.
    at_login: bool,
    /// Why the last change to that failed.
    login_error: Option<String>,
    /// The typed settings on the Summaries page.
    fields: Fields,
    /// The models Ollama has, once asked; or why it could not be asked.
    ollama_models: Option<Result<Vec<String>, String>>,
    /// The models the chosen OpenAI-compatible service offers, likewise.
    endpoint_models: Option<Result<Vec<String>, String>>,
    /// Why the last API key could not be saved.
    key_error: Option<String>,
}

struct Fields {
    ollama_url: Entity<TextField>,
    ollama_model: Entity<TextField>,
    endpoint_url: Entity<TextField>,
    endpoint_model: Entity<TextField>,
    api_key: Entity<TextField>,
    cloudflare_gateway: Entity<TextField>,
}

impl SettingsView {
    fn new(tab: Tab, window: &mut Window, cx: &mut Context<Self>) -> SettingsView {
        // The session window can change settings too (its auto-summarise
        // switch, the first-run answer), so follow them here as well.
        cx.observe_global::<Settings>(|this, cx| {
            this.config = cx.global::<Settings>().0.clone();
            cx.notify();
        })
        .detach();
        let config = cx.global::<Settings>().0.clone();
        let fields = Fields::new(&config, window, cx);
        let mut view = SettingsView {
            tab,
            config: config.clone(),
            scroll: gpui::ScrollHandle::new(),
            measured: Default::default(),
            focus: cx.focus_handle(),
            menu_open: None,
            at_login: crate::login::enabled(),
            login_error: None,
            fields,
            ollama_models: None,
            endpoint_models: None,
            key_error: None,
        };
        match config.summary_provider {
            Provider::Ollama => view.load_ollama_models(cx),
            Provider::OpenAiCompatible => view.load_endpoint_models(cx),
            Provider::Claude => {}
        }
        view
    }

    /// The models Ollama has, asked for in the background.
    fn load_ollama_models(&mut self, cx: &mut Context<Self>) {
        let url = self.config.ollama_url();
        self.ollama_models = None;
        // Development captures are taken before an answer could arrive, so
        // they ask first.
        if std::env::var_os("SCROLLBACK_DEV_SETTINGS").is_some() {
            self.ollama_models = Some(provider::ollama_models(&url).map_err(|err| format!("{err:#}")));
            return;
        }
        cx.spawn(async move |this, cx| {
            let models = cx
                .background_executor()
                .spawn(async move { provider::ollama_models(&url).map_err(|err| format!("{err:#}")) })
                .await;
            this.update(cx, |this, cx| {
                this.ollama_models = Some(models);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The models the chosen service offers, asked for in the background.
    fn load_endpoint_models(&mut self, cx: &mut Context<Self>) {
        self.endpoint_models = None;
        if !self.config.endpoint.lists_models() {
            return;
        }
        if std::env::var_os("SCROLLBACK_DEV_SETTINGS").is_some() {
            self.endpoint_models = Some(provider::endpoint_models(&self.config).map_err(|err| format!("{err:#}")));
            return;
        }
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let models = cx
                .background_executor()
                .spawn(async move { provider::endpoint_models(&config).map_err(|err| format!("{err:#}")) })
                .await;
            this.update(cx, |this, cx| {
                this.endpoint_models = Some(models);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_provider(&mut self, chosen: Provider, cx: &mut Context<Self>) {
        self.change(cx, |c| c.summary_provider = chosen);
        match chosen {
            Provider::Ollama => self.load_ollama_models(cx),
            Provider::OpenAiCompatible => self.load_endpoint_models(cx),
            Provider::Claude => {}
        }
    }

    /// Switch service, showing that service's address, model and key.
    fn choose_endpoint(&mut self, endpoint: Endpoint, cx: &mut Context<Self>) {
        self.change(cx, |c| c.endpoint = endpoint);
        self.key_error = None;
        let (url, model) = (self.config.endpoint_url(), self.config.endpoint_model());
        self.fields.endpoint_url.update(cx, |f, cx| f.set_text(url, cx));
        self.fields.endpoint_model.update(cx, |f, cx| f.set_text(model, cx));
        self.fields.api_key.update(cx, |f, cx| f.set_text("", cx));
        self.load_endpoint_models(cx);
    }

    fn save_key(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        self.key_error = secrets::set_api_key(self.config.endpoint, key.as_deref()).err();
        self.fields.api_key.update(cx, |f, cx| f.set_text("", cx));
        // Readiness depends on the key, so everything that follows the
        // settings is told, though nothing in the file changed.
        store(self.config.clone(), cx);
        self.load_endpoint_models(cx);
        cx.notify();
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
        let at_login = self.at_login;
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
                 minutes it moves on, so it is among the recent sessions while you work. \
                 Uses more requests.",
                switch("live", live, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.change(cx, |c| c.live_summaries = !live))),
                t,
            ))
            .child(row("Appearance", "Light, dark, or whatever the system is set to.", look, t))
            .child(row(
                if mac { "Menu bar icon" } else { "Tray icon" },
                if mac {
                    "Keep Scrollback in the menu bar. It stays running there when its window \
                     is closed, and a click opens recent sessions."
                } else {
                    "Keep Scrollback in the system tray. It stays running there when its \
                     window is closed, and a click opens recent sessions."
                },
                switch("menu-bar-icon", in_menu_bar, t).on_click(
                    cx.listener(move |this, _, _, cx| this.change(cx, |c| c.menu_bar_icon = !in_menu_bar)),
                ),
                t,
            ))
            .child(row(
                "Open at login",
                "Start Scrollback when you log in. With the icon on, it starts there \
                 without opening its window.",
                switch("at-login", at_login, t).on_click(cx.listener(move |this, _, _, cx| {
                    match crate::login::set(!at_login) {
                        Ok(()) => this.login_error = None,
                        Err(err) => this.login_error = Some(err),
                    }
                    this.at_login = crate::login::enabled();
                    cx.notify();
                })),
                t,
            ))
            .children(self.login_error.as_ref().map(|err| {
                div()
                    .text_size(px(text::SMALL))
                    .text_color(t.ink2)
                    .child(format!("Could not change this: {err}"))
            }))
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
        let chosen = self.config.summary_provider;
        let mut providers = segmented(t);
        for (i, each) in Provider::ALL.into_iter().enumerate() {
            providers = providers.child(
                segment(("provider", i), each.name(), chosen == each, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_provider(each, cx))),
            );
        }
        let ready = provider::ready(&self.config);
        let status = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(text::SMALL))
            .text_color(if ready.is_ok() { t.ink2 } else { t.ink })
            .child(
                div()
                    .size(px(7.))
                    .rounded(px(4.))
                    .bg(if ready.is_ok() { gpui::rgb(0x3FA66B).into() } else { t.ink3 }),
            )
            .child(match &ready {
                Ok(()) => "Ready to write summaries.".to_string(),
                Err(why) => why.clone(),
            });
        let mut page = page()
            .child(row("Written by", "Who writes each session's summary.", providers, t))
            .child(match chosen {
                Provider::Claude => self.claude_settings(t, cx),
                Provider::Ollama => self.ollama_settings(t, cx),
                Provider::OpenAiCompatible => self.endpoint_settings(t, cx),
            })
            .child(status);
        let sent = match chosen {
            Provider::Claude => {
                "Requests go through the Claude Code command line and count towards your Claude plan."
            }
            Provider::Ollama => "Requests go to Ollama on this computer, so nothing leaves it.",
            Provider::OpenAiCompatible => {
                "Requests go to the service above with your API key, and are billed by it."
            }
        };
        page = page.child(
            div()
                .pt(px(18.))
                .border_t_1()
                .border_color(t.line)
                .text_size(px(text::SMALL))
                .line_height(px(20.))
                .text_color(t.ink2)
                .child(format!(
                    "A summary request sends your prompts, the agent's replies, and the names of the \
                     tools and files involved. Tool output and file contents are never sent. {sent}"
                )),
        );
        page
    }

    /// One labelled field, its name above it.
    fn field_row(name: &'static str, about: Option<String>, field: &Entity<TextField>, t: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().font_weight(FontWeight::MEDIUM).child(name))
            .child(field.clone())
            .children(about.map(|about| {
                div().text_size(px(text::CAPTION)).line_height(px(17.)).text_color(t.ink3).child(about)
            }))
    }

    fn claude_settings(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let mut models = segmented(t);
        for (i, (id, name)) in CLAUDE_MODELS.into_iter().enumerate() {
            models = models.child(
                segment(("model", i), name, self.config.model() == id, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.change(cx, |c| c.summary_model = id.to_string()))),
            );
        }
        let tool = match summarize::claude_binary() {
            Some(path) => format!("Found at {}", path.display()),
            None => "Not found. Install Claude Code and sign in, or choose another provider.".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(row("Model", "Haiku is quick, and uses little of your Claude plan.", models, t))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().font_weight(FontWeight::MEDIUM).child("The claude command"))
                    .child(div().font_family(mono()).text_size(px(text::CAPTION)).text_color(t.ink3).child(tool)),
            )
    }

    fn ollama_settings(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let installed: Div = match &self.ollama_models {
            None => div().text_size(px(text::CAPTION)).text_color(t.ink3).child("Asking Ollama for its models…"),
            Some(Err(_)) => div()
                .text_size(px(text::CAPTION))
                .line_height(px(17.))
                .text_color(t.ink2)
                .child(format!(
                    "Ollama is not answering at {}. Install it from ollama.com and start it, then pull a \
                     model, such as `ollama pull llama3.2`.",
                    self.config.ollama_url()
                )),
            Some(Ok(models)) if models.is_empty() => div()
                .text_size(px(text::CAPTION))
                .text_color(t.ink2)
                .child("Ollama has no models yet. Pull one, such as `ollama pull llama3.2`."),
            Some(Ok(models)) => self.model_chips("ollama-model", models, &self.config.ollama_model, true, t, cx),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(Self::field_row("Model", None, &self.fields.ollama_model, t))
            .child(installed)
            .child(Self::field_row(
                "Address",
                Some("Where Ollama is running. The usual address is filled in.".to_string()),
                &self.fields.ollama_url,
                t,
            ))
    }

    /// Models to pick from, as small buttons; the chosen one is marked.
    fn model_chips(
        &self,
        id: &'static str,
        models: &[String],
        chosen: &str,
        ollama: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut chips = div().flex().flex_wrap().gap(px(6.));
        // A long catalogue is cut short; the field takes any name.
        for (i, name) in models.iter().take(40).enumerate() {
            let picked = chosen == name;
            let pick = name.clone();
            let fill = t.fill;
            chips = chips.child(
                div()
                    .id((id, i))
                    .reachable(t)
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if picked { t.accent } else { t.line })
                    .when(picked, |el| el.bg(fill))
                    .font_family(mono())
                    .text_size(px(text::CAPTION))
                    .cursor_pointer()
                    .hover(move |style| style.bg(fill))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let pick = pick.clone();
                        if ollama {
                            this.fields.ollama_model.update(cx, |f, cx| f.set_text(pick.clone(), cx));
                            this.change(cx, |c| c.ollama_model = pick);
                        } else {
                            let slug = this.config.endpoint.slug().to_string();
                            this.fields.endpoint_model.update(cx, |f, cx| f.set_text(pick.clone(), cx));
                            this.change(cx, |c| {
                                c.endpoint_models.insert(slug, pick);
                            });
                        }
                    }))
                    .child(name.clone()),
            );
        }
        let heading = if ollama { "On this computer" } else { "Offered by this service" };
        let mut list = div().flex().flex_col().gap(px(6.)).child(label(heading, t)).child(chips);
        if models.len() > 40 {
            list = list.child(
                div()
                    .text_size(px(text::CAPTION))
                    .text_color(t.ink3)
                    .child(format!("and {} more; type any of them in the field above.", models.len() - 40)),
            );
        }
        list
    }

    fn endpoint_settings(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let endpoint = self.config.endpoint;
        let mut services = segmented(t);
        for (i, each) in Endpoint::ALL.into_iter().enumerate() {
            services = services.child(
                segment(("endpoint", i), each.short_name(), endpoint == each, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_endpoint(each, cx))),
            );
        }
        let saved = secrets::api_key_saved(endpoint);
        let from_env = !saved && secrets::api_key(endpoint).is_some();
        let key_about = match (saved, from_env) {
            (true, _) => "A key is saved in your system keychain. Paste another to replace it.".to_string(),
            (false, true) => "Using the key from your environment. Paste one to save it instead.".to_string(),
            (false, false) => "Saved in your system keychain, never in Scrollback's settings.".to_string(),
        };
        let url_about = match endpoint {
            Endpoint::Cloudflare => Some(
                "Put your account id in place of ACCOUNT_ID. The key is a Cloudflare API token with \
                 Workers AI access."
                    .to_string(),
            ),
            Endpoint::Vercel => Some("Models are named provider/model, such as openai/gpt-6-luna.".to_string()),
            Endpoint::OpenCodeGo => Some("Uses the API key from your OpenCode Go subscription.".to_string()),
            _ => None,
        };
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(services)
            .child(Self::field_row("Address", url_about, &self.fields.endpoint_url, t))
            .child(Self::field_row("Model", None, &self.fields.endpoint_model, t))
            .children(match &self.endpoint_models {
                Some(Ok(models)) if !models.is_empty() => {
                    Some(self.model_chips("endpoint-model", models, &self.config.endpoint_model(), false, t, cx))
                }
                _ => None,
            })
            .child(Self::field_row("API key", Some(key_about), &self.fields.api_key, t));
        if saved {
            let fill = t.fill;
            block = block.child(div().flex().child(
                div()
                    .id("remove-key")
                    .reachable(t)
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(text::SMALL))
                    .cursor_pointer()
                    .hover(move |style| style.bg(fill))
                    .on_click(cx.listener(|this, _, _, cx| this.save_key(None, cx)))
                    .child("Remove the saved key"),
            ));
        }
        if endpoint == Endpoint::Cloudflare {
            block = block.child(Self::field_row(
                "AI Gateway",
                Some("The id of the gateway to route through. Leave empty to call Cloudflare directly.".to_string()),
                &self.fields.cloudflare_gateway,
                t,
            ));
        }
        block.children(self.key_error.as_ref().map(|err| {
            div().text_size(px(text::SMALL)).text_color(t.ink2).child(format!("The keychain refused: {err}"))
        }))
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
                                    .child("Scrollback"),
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
                            .child("Scrollback reads session files where they are and never writes to them."),
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
        let bar = chrome::bar("Settings", false, None, window, cx, &t);
        let root = div()
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
            .flex_col()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(UI_FONT)
            .text_size(px(text::BODY))
            .line_height(px(20.))
            .children(bar);
        let body = div()
            .flex_1()
            .min_h(px(0.))
            .flex()
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
            );
        chrome::frame(root.child(body), window, &t)
    }
}

impl chrome::Menus for SettingsView {
    fn open_menu(&mut self) -> &mut Option<usize> {
        &mut self.menu_open
    }
}

impl Fields {
    fn new(config: &Config, window: &mut Window, cx: &mut Context<SettingsView>) -> Fields {
        let make = |text: String, placeholder: &str, secret: bool, window: &mut Window, cx: &mut Context<SettingsView>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let field = TextField::new(text, placeholder, window, cx).code();
                if secret { field.secret() } else { field }
            })
        };
        let fields = Fields {
            ollama_url: make(config.ollama_url(), "http://127.0.0.1:11434", false, window, cx),
            ollama_model: make(config.ollama_model.clone(), "llama3.2", false, window, cx),
            endpoint_url: make(config.endpoint_url(), "https://…/v1", false, window, cx),
            endpoint_model: make(config.endpoint_model(), "model name", false, window, cx),
            api_key: make(String::new(), "Paste a key", true, window, cx),
            cloudflare_gateway: make(config.cloudflare_gateway.clone(), "my-gateway", false, window, cx),
        };
        cx.subscribe(&fields.ollama_url, |this, _, Committed(text), cx| {
            let text = text.clone();
            this.change(cx, |c| c.ollama_url = text);
            this.load_ollama_models(cx);
        })
        .detach();
        cx.subscribe(&fields.ollama_model, |this, _, Committed(text), cx| {
            let text = text.clone();
            this.change(cx, |c| c.ollama_model = text);
        })
        .detach();
        cx.subscribe(&fields.endpoint_url, |this, _, Committed(text), cx| {
            let (slug, text) = (this.config.endpoint.slug().to_string(), text.clone());
            this.change(cx, |c| {
                c.endpoint_urls.insert(slug, text);
            });
        })
        .detach();
        cx.subscribe(&fields.endpoint_model, |this, _, Committed(text), cx| {
            let (slug, text) = (this.config.endpoint.slug().to_string(), text.clone());
            this.change(cx, |c| {
                c.endpoint_models.insert(slug, text);
            });
        })
        .detach();
        cx.subscribe(&fields.api_key, |this, _, Committed(text), cx| {
            if !text.is_empty() {
                this.save_key(Some(text.clone()), cx);
            }
        })
        .detach();
        cx.subscribe(&fields.cloudflare_gateway, |this, _, Committed(text), cx| {
            let text = text.clone();
            this.change(cx, |c| c.cloudflare_gateway = text);
        })
        .detach();
        fields
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
