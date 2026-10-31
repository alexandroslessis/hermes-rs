use gpui::{App, Font, FontFeatures, FontWeight, Pixels, font, px};
use theme::{ThemeSettingsProvider, UiDensity};

/// Fonts and sizes the Zed `ui` crate reads through `theme::theme_settings`.
/// Hard-coded for now; becomes user-configurable with the settings screen.
pub struct Appearance {
    ui_font: Font,
    buffer_font: Font,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            ui_font: font(".ZedSans"),
            buffer_font: Font {
                features: FontFeatures::disable_ligatures(),
                weight: FontWeight::NORMAL,
                ..font(".ZedMono")
            },
        }
    }
}

impl ThemeSettingsProvider for Appearance {
    fn ui_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.ui_font
    }

    fn buffer_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.buffer_font
    }

    fn ui_font_size(&self, _cx: &App) -> Pixels {
        px(15.)
    }

    fn buffer_font_size(&self, _cx: &App) -> Pixels {
        px(14.)
    }

    fn ui_density(&self, _cx: &App) -> UiDensity {
        UiDensity::Default
    }
}
