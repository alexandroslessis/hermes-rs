//! The themes the spike and the gallery switch between: GPUI Kit's own light and dark, and Zed's
//! One Light and One Dark mapped by hand ([`zed_theme`]). CP2.1 replaces this with our tokens and
//! a theme folder.

use std::rc::Rc;

use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};

use crate::zed_theme;

/// The Zed theme file the spike maps by hand (`assets/themes/one/one.json`, Zed's One theme).
const ONE_THEME: &str = include_str!("../../../assets/themes/one/one.json");

/// A theme to switch to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    /// GPUI Kit's default dark theme.
    DefaultDark,
    /// GPUI Kit's default light theme.
    DefaultLight,
    /// Zed's One Dark, mapped by [`zed_theme::kit_theme`].
    OneDark,
    /// Zed's One Light, mapped by [`zed_theme::kit_theme`].
    OneLight,
}

impl ThemeChoice {
    /// Every theme, in the order the spike lists them.
    pub const ALL: [ThemeChoice; 4] = [
        Self::DefaultDark,
        Self::DefaultLight,
        Self::OneDark,
        Self::OneLight,
    ];

    /// The name shown on the theme's button.
    pub fn label(self) -> &'static str {
        match self {
            Self::DefaultDark => "Default dark",
            Self::DefaultLight => "Default light",
            Self::OneDark => "One Dark",
            Self::OneLight => "One Light",
        }
    }

    /// The short name used in snapshot file names (`button-states-light.png`).
    pub fn slug(self) -> &'static str {
        match self {
            Self::DefaultDark => "dark",
            Self::DefaultLight => "light",
            Self::OneDark => "one-dark",
            Self::OneLight => "one-light",
        }
    }

    /// Whether it is a dark theme.
    pub fn is_dark(self) -> bool {
        matches!(self, Self::DefaultDark | Self::OneDark)
    }

    /// Whether it is one of Zed's (One) rather than GPUI Kit's default.
    pub fn is_zed(self) -> bool {
        matches!(self, Self::OneDark | Self::OneLight)
    }

    /// The same family (Kit default or Zed One) in light or dark.
    pub fn with_dark(self, dark: bool) -> Self {
        match (self.is_zed(), dark) {
            (false, false) => Self::DefaultLight,
            (false, true) => Self::DefaultDark,
            (true, false) => Self::OneLight,
            (true, true) => Self::OneDark,
        }
    }
}

/// Switches the whole app to `choice`.
pub fn apply_theme(choice: ThemeChoice, cx: &mut App) {
    let zed_name = match choice {
        ThemeChoice::OneDark => Some("One Dark"),
        ThemeChoice::OneLight => Some("One Light"),
        _ => None,
    };
    if let Some(name) = zed_name {
        match zed_theme::kit_theme(ONE_THEME, name) {
            Ok(config) => Theme::update(cx, |theme| theme.apply_config(&Rc::new(config))),
            Err(error) => eprintln!("theme {name}: {error:#}"),
        }
        return;
    }
    // Put GPUI Kit's own themes back before switching mode, or the mode keeps the Zed colours.
    let registry = ThemeRegistry::global(cx);
    let light = registry.default_light_theme().clone();
    let dark = registry.default_dark_theme().clone();
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    let mode = if choice == ThemeChoice::DefaultLight {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, None, cx);
}
