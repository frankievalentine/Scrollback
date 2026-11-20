use gpui::{point, px, rgb, rgba, BoxShadow, Hsla, WindowAppearance};

/// Colours for one appearance. The names match the design canvas.
#[derive(Clone)]
pub struct Theme {
    pub bg: Hsla,
    pub rail: Hsla,
    pub raise: Hsla,
    pub fill: Hsla,
    /// The chosen option of a segmented control, which sits on `fill`.
    pub segment: Hsla,
    pub line: Hsla,
    pub ink: Hsla,
    pub ink2: Hsla,
    pub ink3: Hsla,
    pub accent: Hsla,
    pub on_accent: Hsla,
    pub card_shadow: Vec<BoxShadow>,
}

pub const UI_FONT: &str = ".SystemUIFont";
pub const MONO_FONT: &str = "Menlo";

/// Claude Code's logo keeps its own colour in both appearances.
pub fn claude_orange() -> Hsla {
    rgb(0xD97757).into()
}

/// The appearance chosen in the settings: 0 follows the system, 1 is light,
/// 2 is dark. Kept here so every window can ask without being handed it.
static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Apply the settings' appearance choice: `system`, `light` or `dark`.
pub fn set_mode(mode: &str) {
    let value = match mode {
        "light" => 1,
        "dark" => 2,
        _ => 0,
    };
    MODE.store(value, std::sync::atomic::Ordering::Relaxed);
}

impl Theme {
    /// The theme for the window's appearance, unless the settings pick one.
    /// `SCROLLBACK_THEME=light` or `dark` overrides both, for development.
    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        match std::env::var("SCROLLBACK_THEME").as_deref() {
            Ok("light") => return Theme::light(),
            Ok("dark") => return Theme::dark(),
            _ => {}
        }
        match MODE.load(std::sync::atomic::Ordering::Relaxed) {
            1 => return Theme::light(),
            2 => return Theme::dark(),
            _ => {}
        }
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
        }
    }

    fn light() -> Theme {
        Theme {
            bg: rgb(0xFBFBFA).into(),
            rail: rgb(0xF6F6F4).into(),
            raise: rgb(0xFFFFFF).into(),
            fill: rgb(0xEEEEEB).into(),
            segment: rgb(0xFFFFFF).into(),
            line: rgb(0xE4E4E1).into(),
            ink: rgb(0x17181A).into(),
            ink2: rgb(0x3D4046).into(),
            ink3: rgb(0x5C5F66).into(),
            accent: rgb(0x3B5BDB).into(),
            on_accent: rgb(0xFFFFFF).into(),
            card_shadow: vec![
                shadow(0x17181A0F, 0., 0., 1.),
                shadow(0x17181A1F, 10., 18., -10.),
                shadow(0x17181A38, 32., 56., -28.),
            ],
        }
    }

    fn dark() -> Theme {
        Theme {
            bg: rgb(0x131416).into(),
            rail: rgb(0x0F1012).into(),
            raise: rgb(0x1C1E22).into(),
            fill: rgb(0x24262B).into(),
            segment: rgb(0x3A3D45).into(),
            line: rgb(0x2A2C32).into(),
            ink: rgb(0xECECEA).into(),
            ink2: rgb(0xC9CACD).into(),
            ink3: rgb(0xA3A6AD).into(),
            accent: rgb(0x8AA4FF).into(),
            on_accent: rgb(0x131416).into(),
            card_shadow: vec![
                shadow(0x00000099, 0., 0., 1.),
                shadow(0x00000066, 2., 4., 0.),
                shadow(0x000000CC, 28., 56., -24.),
            ],
        }
    }
}

fn shadow(color: u32, y: f32, blur: f32, spread: f32) -> BoxShadow {
    BoxShadow {
        color: rgba(color).into(),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}
