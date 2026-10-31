//! hermes-rs: stands in for Zed's `theme_settings::ThemeSettings`, which reads Zed's settings
//! system. Fonts and sizes come from the `theme::ThemeSettingsProvider` the app registers; Zed's
//! separate agent / markdown-preview font settings all map onto the UI and code fonts.

use gpui::{App, Font, FontWeight, Pixels, SharedString};

pub struct LineHeight(f32);

impl LineHeight {
    pub fn value(&self) -> f32 {
        self.0
    }
}

pub struct ThemeSettings {
    pub buffer_font: Font,
    pub ui_font: Font,
    pub buffer_line_height: LineHeight,
    ui_font_size: Pixels,
    buffer_font_size: Pixels,
}

impl ThemeSettings {
    pub fn get_global(cx: &App) -> Self {
        let provider = theme::theme_settings(cx);
        Self {
            buffer_font: provider.buffer_font(cx).clone(),
            ui_font: provider.ui_font(cx).clone(),
            buffer_line_height: LineHeight(1.618),
            ui_font_size: provider.ui_font_size(cx),
            buffer_font_size: provider.buffer_font_size(cx),
        }
    }

    pub fn ui_font_size(&self, _cx: &App) -> Pixels {
        self.ui_font_size
    }

    pub fn buffer_font_size(&self, _cx: &App) -> Pixels {
        self.buffer_font_size
    }

    pub fn agent_ui_font_size(&self, _cx: &App) -> Pixels {
        self.ui_font_size
    }

    pub fn agent_buffer_font_size(&self, _cx: &App) -> Pixels {
        self.buffer_font_size
    }

    pub fn markdown_preview_font_size(&self, _cx: &App) -> Pixels {
        self.ui_font_size
    }

    pub fn agent_ui_font_family(&self) -> &SharedString {
        &self.ui_font.family
    }

    pub fn agent_buffer_font_family(&self) -> &SharedString {
        &self.buffer_font.family
    }

    pub fn markdown_preview_font_family(&self) -> &SharedString {
        &self.ui_font.family
    }

    pub fn markdown_preview_code_font_family(&self) -> &SharedString {
        &self.buffer_font.family
    }

    pub fn markdown_preview_heading_font_weight(&self) -> FontWeight {
        FontWeight::SEMIBOLD
    }

    pub fn mermaid_font_family(&self) -> &SharedString {
        &self.ui_font.family
    }
}

/// Test setup replacing Zed's `settings::init` + `theme_settings::init`.
#[cfg(test)]
pub fn init_test(cx: &mut App) {
    struct TestFonts {
        ui: Font,
        buffer: Font,
    }
    impl theme::ThemeSettingsProvider for TestFonts {
        fn ui_font<'a>(&'a self, _: &'a App) -> &'a Font {
            &self.ui
        }
        fn buffer_font<'a>(&'a self, _: &'a App) -> &'a Font {
            &self.buffer
        }
        fn ui_font_size(&self, _: &App) -> Pixels {
            gpui::px(16.)
        }
        fn buffer_font_size(&self, _: &App) -> Pixels {
            gpui::px(14.)
        }
        fn ui_density(&self, _: &App) -> theme::UiDensity {
            theme::UiDensity::Default
        }
    }
    if !cx.has_global::<theme::GlobalTheme>() {
        theme::init(theme::LoadThemes::JustBase, cx);
        theme::set_theme_settings_provider(
            Box::new(TestFonts {
                ui: gpui::font(".ZedSans"),
                buffer: gpui::font(".ZedMono"),
            }),
            cx,
        );
    }
}
