//! Zed's JSON themes (One, Ayu, Gruvbox, …): parsing theme files and refining them into
//! `theme::Theme`s. Copied from Zed's `theme_settings` crate, minus the settings plumbing.

mod content;
mod schema;

use std::sync::Arc;

use anyhow::Context as _;
use gpui::{FontStyle, FontWeight, HighlightStyle, Refineable, WindowBackgroundAppearance};
use theme::{
    AccentColors, Appearance, AppearanceContent, PlayerColor, PlayerColors, StatusColors,
    SyntaxTheme, SystemColors, Theme, ThemeColors, ThemeFamily, ThemeRegistry, ThemeStyles,
    default_color_scales, try_parse_color,
};

pub use content::*;
pub use schema::{
    ThemeContent, ThemeFamilyContent, status_colors_refinement, syntax_overrides,
    theme_colors_refinement,
};

/// Converts a theme-file value into its GPUI counterpart.
pub trait IntoGpui {
    type Output;
    fn into_gpui(self) -> Self::Output;
}

impl IntoGpui for FontStyleContent {
    type Output = FontStyle;

    fn into_gpui(self) -> Self::Output {
        match self {
            FontStyleContent::Normal => FontStyle::Normal,
            FontStyleContent::Italic => FontStyle::Italic,
            FontStyleContent::Oblique => FontStyle::Oblique,
        }
    }
}

impl IntoGpui for FontWeightContent {
    type Output = FontWeight;

    fn into_gpui(self) -> Self::Output {
        FontWeight(self.0.clamp(100., 950.))
    }
}

impl IntoGpui for WindowBackgroundContent {
    type Output = WindowBackgroundAppearance;

    fn into_gpui(self) -> Self::Output {
        match self {
            WindowBackgroundContent::Opaque => WindowBackgroundAppearance::Opaque,
            WindowBackgroundContent::Transparent => WindowBackgroundAppearance::Transparent,
            WindowBackgroundContent::Blurred => WindowBackgroundAppearance::Blurred,
            WindowBackgroundContent::MicaBackdrop => WindowBackgroundAppearance::MicaBackdrop,
            WindowBackgroundContent::MicaAltBackdrop => WindowBackgroundAppearance::MicaAltBackdrop,
        }
    }
}

trait ResultExt<T> {
    fn log_err(self) -> Option<T>;
}

impl<T> ResultExt<T> for anyhow::Result<T> {
    fn log_err(self) -> Option<T> {
        self.map_err(|error| log::error!("{error:#}")).ok()
    }
}

/// Loads the themes bundled with the app into the registry.
pub fn load_bundled_themes(registry: &ThemeRegistry) {
    let theme_paths = registry
        .assets()
        .list("themes/")
        .expect("failed to list theme assets")
        .into_iter()
        .filter(|path| path.ends_with(".json"));

    for path in theme_paths {
        let Some(theme) = registry.assets().load(&path).log_err().flatten() else {
            continue;
        };

        let Some(theme_family) = serde_json::from_slice(&theme)
            .with_context(|| format!("failed to parse theme at path \"{path}\""))
            .log_err()
        else {
            continue;
        };

        let refined = refine_theme_family(theme_family);
        registry.insert_theme_families([refined]);
    }
}

/// Refines a [`ThemeFamilyContent`] and its [`ThemeContent`]s into a [`ThemeFamily`].
pub fn refine_theme_family(theme_family_content: ThemeFamilyContent) -> ThemeFamily {
    let id = uuid::Uuid::new_v4().to_string();
    let name = theme_family_content.name.clone();
    let author = theme_family_content.author.clone();

    let themes: Vec<Theme> = theme_family_content
        .themes
        .iter()
        .map(|theme_content| refine_theme(theme_content))
        .collect();

    ThemeFamily {
        id,
        name: name.into(),
        author: author.into(),
        themes,
        scales: default_color_scales(),
    }
}

/// Refines a [`ThemeContent`] into a [`Theme`].
pub fn refine_theme(theme: &ThemeContent) -> Theme {
    let appearance = match theme.appearance {
        AppearanceContent::Light => Appearance::Light,
        AppearanceContent::Dark => Appearance::Dark,
    };

    let mut refined_status_colors = match theme.appearance {
        AppearanceContent::Light => StatusColors::light(),
        AppearanceContent::Dark => StatusColors::dark(),
    };
    let mut status_colors_refinement = status_colors_refinement(&theme.style.status);
    theme::apply_status_color_defaults(&mut status_colors_refinement);
    refined_status_colors.refine(&status_colors_refinement);

    let mut refined_player_colors = match theme.appearance {
        AppearanceContent::Light => PlayerColors::light(),
        AppearanceContent::Dark => PlayerColors::dark(),
    };
    merge_player_colors(&mut refined_player_colors, &theme.style.players);

    let mut refined_theme_colors = match theme.appearance {
        AppearanceContent::Light => ThemeColors::light(),
        AppearanceContent::Dark => ThemeColors::dark(),
    };
    let mut theme_colors_refinement = theme_colors_refinement(
        &theme.style.colors,
        &status_colors_refinement,
        theme.appearance == AppearanceContent::Light,
    );
    theme::apply_theme_color_defaults(&mut theme_colors_refinement, &refined_player_colors);
    refined_theme_colors.refine(&theme_colors_refinement);

    let mut refined_accent_colors = match theme.appearance {
        AppearanceContent::Light => AccentColors::light(),
        AppearanceContent::Dark => AccentColors::dark(),
    };
    merge_accent_colors(&mut refined_accent_colors, &theme.style.accents);

    let syntax_highlights = theme.style.syntax.iter().map(|(syntax_token, highlight)| {
        (
            syntax_token.clone(),
            HighlightStyle {
                color: highlight
                    .color
                    .as_ref()
                    .and_then(|color| try_parse_color(color).ok()),
                background_color: highlight
                    .background_color
                    .as_ref()
                    .and_then(|color| try_parse_color(color).ok()),
                font_style: highlight.font_style.map(|s| s.into_gpui()),
                font_weight: highlight.font_weight.map(|w| w.into_gpui()),
                ..Default::default()
            },
        )
    });
    let syntax_theme = Arc::new(SyntaxTheme::new(syntax_highlights));

    let window_background_appearance = theme
        .style
        .window_background_appearance
        .map(|w| w.into_gpui())
        .unwrap_or_default();

    Theme {
        id: uuid::Uuid::new_v4().to_string(),
        name: theme.name.clone().into(),
        appearance,
        styles: ThemeStyles {
            system: SystemColors::default(),
            window_background_appearance,
            accents: refined_accent_colors,
            colors: refined_theme_colors,
            status: refined_status_colors,
            player: refined_player_colors,
            syntax: syntax_theme,
        },
    }
}

/// Merges player color overrides into the given [`PlayerColors`].
pub fn merge_player_colors(
    player_colors: &mut PlayerColors,
    user_player_colors: &[crate::content::PlayerColorContent],
) {
    if user_player_colors.is_empty() {
        return;
    }

    for (idx, player) in user_player_colors.iter().enumerate() {
        let cursor = player
            .cursor
            .as_ref()
            .and_then(|color| try_parse_color(color).ok());
        let background = player
            .background
            .as_ref()
            .and_then(|color| try_parse_color(color).ok());
        let selection = player
            .selection
            .as_ref()
            .and_then(|color| try_parse_color(color).ok());

        if let Some(player_color) = player_colors.0.get_mut(idx) {
            *player_color = PlayerColor {
                cursor: cursor.unwrap_or(player_color.cursor),
                background: background.unwrap_or(player_color.background),
                selection: selection.unwrap_or(player_color.selection),
            };
        } else {
            player_colors.0.push(PlayerColor {
                cursor: cursor.unwrap_or_default(),
                background: background.unwrap_or_default(),
                selection: selection.unwrap_or_default(),
            });
        }
    }
}

/// Merges accent color overrides into the given [`AccentColors`].
pub fn merge_accent_colors(
    accent_colors: &mut AccentColors,
    user_accent_colors: &[crate::content::AccentContent],
) {
    if user_accent_colors.is_empty() {
        return;
    }

    let colors = user_accent_colors
        .iter()
        .filter_map(|accent_color| {
            accent_color
                .0
                .as_ref()
                .and_then(|color| try_parse_color(color).ok())
        })
        .collect::<Vec<_>>();

    if !colors.is_empty() {
        accent_colors.0 = Arc::from(colors);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_parse_and_refine() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
        let mut names = Vec::new();
        for family in ["one/one.json", "ayu/ayu.json", "gruvbox/gruvbox.json"] {
            let bytes = std::fs::read(root.join(family)).unwrap();
            let content: ThemeFamilyContent = serde_json::from_slice(&bytes).unwrap();
            let family = refine_theme_family(content);
            names.extend(family.themes.iter().map(|theme| theme.name.to_string()));
            for theme in &family.themes {
                assert!(
                    theme.syntax().highlight_id("keyword").is_some(),
                    "{} has no keyword color",
                    theme.name
                );
            }
        }
        assert!(names.contains(&"One Dark".to_string()));
        assert!(names.contains(&"Gruvbox Light".to_string()));
    }
}
