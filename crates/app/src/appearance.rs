use gpui::{App, Font, FontFeatures, FontWeight, Pixels, font, px};
use theme::{ThemeSettingsProvider, UiDensity};

use crate::config::FontSettings;

pub const DEFAULT_UI_FONT: &str = ".ZedSans";
pub const DEFAULT_CODE_FONT: &str = ".ZedMono";
pub const DEFAULT_UI_SIZE: f32 = 15.;
pub const DEFAULT_CODE_SIZE: f32 = 14.;
pub const MIN_FONT_SIZE: f32 = 10.;
pub const MAX_FONT_SIZE: f32 = 28.;

/// Fonts and sizes the Zed `ui` crate reads through `theme::theme_settings`, from Settings.
pub struct Appearance {
    ui_font: Font,
    buffer_font: Font,
    ui_size: Pixels,
    buffer_size: Pixels,
}

impl Default for Appearance {
    fn default() -> Self {
        Self::new(&FontSettings::default())
    }
}

impl Appearance {
    pub fn new(fonts: &FontSettings) -> Self {
        let size = |size: Option<f32>, default: f32| {
            px(size.unwrap_or(default).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE))
        };
        Self {
            ui_font: font(fonts.ui_family.clone().unwrap_or(DEFAULT_UI_FONT.into())),
            buffer_font: Font {
                features: FontFeatures::disable_ligatures(),
                weight: FontWeight::NORMAL,
                ..font(fonts.code_family.clone().unwrap_or(DEFAULT_CODE_FONT.into()))
            },
            ui_size: size(fonts.ui_size, DEFAULT_UI_SIZE),
            buffer_size: size(fonts.code_size, DEFAULT_CODE_SIZE),
        }
    }
}

/// The window's rem: Zed's widgets are sized in rems (16px by default), so the interface scales
/// with its font size.
pub fn rem_size(cx: &App) -> Pixels {
    let ui_size = theme::theme_settings(cx).ui_font_size(cx);
    px(16.) * (ui_size / px(DEFAULT_UI_SIZE))
}

impl ThemeSettingsProvider for Appearance {
    fn ui_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.ui_font
    }

    fn buffer_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.buffer_font
    }

    fn ui_font_size(&self, _cx: &App) -> Pixels {
        self.ui_size
    }

    fn buffer_font_size(&self, _cx: &App) -> Pixels {
        self.buffer_size
    }

    fn ui_density(&self, _cx: &App) -> UiDensity {
        UiDensity::Default
    }
}
