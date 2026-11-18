//! Our tokens onto GPUI Kit's theme: every colour GPUI Kit's components read comes from a
//! [`Palette`] token through [`KIT_COLORS`], so a Kit `Button`, `Input` or `Sidebar` looks like
//! the rest of the app and a new theme (t3code's, a Zed file) changes them all at once.
//!
//! GPUI Kit's own theme is a list of dotted keys (`muted.foreground`, `sidebar.border`, ..) with
//! `#rrggbbaa` values plus a `highlight` section for code. It derives the keys a config leaves out
//! (hover and active tints) from the ones it has. Where t3code defines the hover state (a ghost
//! control's hover is `--accent`, a primary control's is primary at 90%) the table sets it.
//!
//! t3code: `apps/web/src/components/ui/button.tsx` (which token each variant's states use),
//! `apps/web/src/index.css` (the token names).

use anyhow::{Context as _, Result};
use gpui_kit::component::ThemeConfig;
use serde_json::{Map, Value, json};

use super::{Fonts, ThemeDef};
use crate::design::tokens::{Color, Palette, radius, text};

/// GPUI Kit colour key ← our token. Several Kit keys read one token where t3code uses one colour
/// for several jobs.
pub const KIT_COLORS: &[(&str, &str)] = &[
    ("background", "background"),
    ("foreground", "foreground"),
    ("border", "border"),
    ("window.border", "border"),
    ("input.border", "input"),
    ("ring", "ring"),
    ("caret", "foreground"),
    ("muted.background", "muted"),
    ("muted.foreground", "muted_foreground"),
    ("accent.background", "accent"),
    ("accent.foreground", "accent_foreground"),
    // A Kit button is t3code's `outline` variant: popover fill, accent on hover.
    ("button.background", "popover"),
    ("button.hover.background", "accent"),
    ("button.active.background", "accent"),
    ("button.foreground", "foreground"),
    ("primary.background", "primary"),
    ("primary.hover.background", "message_action_hover"),
    ("primary.active.background", "message_action_hover"),
    ("primary.foreground", "primary_foreground"),
    ("button.primary.background", "primary"),
    ("button.primary.hover.background", "message_action_hover"),
    ("button.primary.active.background", "message_action_hover"),
    ("button.primary.foreground", "primary_foreground"),
    ("secondary.background", "secondary"),
    ("secondary.hover.background", "accent"),
    ("secondary.active.background", "accent"),
    ("secondary.foreground", "secondary_foreground"),
    ("button.secondary.background", "secondary"),
    ("button.secondary.hover.background", "accent"),
    ("button.secondary.active.background", "accent"),
    ("button.secondary.foreground", "secondary_foreground"),
    ("popover.background", "popover"),
    ("popover.foreground", "popover_foreground"),
    ("danger.background", "destructive"),
    ("danger.foreground", "primary_foreground"),
    ("button.danger.background", "destructive"),
    ("button.danger.foreground", "primary_foreground"),
    ("success.background", "success"),
    ("success.foreground", "primary_foreground"),
    ("button.success.background", "success"),
    ("button.success.foreground", "primary_foreground"),
    ("warning.background", "warning"),
    ("warning.foreground", "primary_foreground"),
    ("button.warning.background", "warning"),
    ("button.warning.foreground", "primary_foreground"),
    ("info.background", "info"),
    ("info.foreground", "primary_foreground"),
    ("button.info.background", "info"),
    ("button.info.foreground", "primary_foreground"),
    ("link", "info_foreground"),
    ("link.hover", "info_foreground"),
    ("link.active", "info_foreground"),
    ("sidebar.background", "sidebar"),
    ("sidebar.foreground", "sidebar_foreground"),
    ("sidebar.border", "sidebar_border"),
    ("sidebar.accent.background", "sidebar_row_hover"),
    ("sidebar.accent.foreground", "sidebar_foreground"),
    ("sidebar.primary.background", "primary"),
    ("sidebar.primary.foreground", "primary_foreground"),
    ("list.background", "background"),
    ("list.even.background", "background"),
    ("list.head.background", "background"),
    ("list.hover.background", "sidebar_row_hover"),
    // The current item of a list is t3code's open-thread row.
    ("list.active.background", "sidebar_row_active"),
    ("list.active.border", "ring"),
    ("scrollbar.thumb.background", "scrollbar_thumb"),
    ("scrollbar.thumb.hover.background", "scrollbar_thumb_hover"),
    ("title_bar.background", "background"),
    ("title_bar.border", "border"),
    ("status_bar.background", "background"),
    ("status_bar.border", "border"),
    ("tab_bar.background", "sidebar"),
    ("tab.background", "sidebar"),
    ("tab.active.background", "background"),
    ("tab.foreground", "muted_foreground"),
    ("tab.active.foreground", "foreground"),
    ("table.background", "background"),
    ("table.head.background", "muted"),
    ("table.head.foreground", "foreground"),
    ("table.hover.background", "accent"),
    ("table.row.border", "border"),
    ("skeleton.background", "accent"),
    ("drop_target.background", "accent"),
    ("base.red", "error"),
    ("base.green", "success"),
    ("base.yellow", "warning"),
    ("base.blue", "info"),
];

/// Selected text: the primary colour at 30%, over whatever it is on.
const SELECTION_ALPHA: u8 = 77;

/// The colours object of a GPUI Kit theme config for `palette`.
pub fn colors(palette: &Palette) -> Map<String, Value> {
    let mut colors = Map::new();
    for (kit_key, token) in KIT_COLORS {
        if let Some(color) = palette.get(token) {
            colors.insert((*kit_key).to_string(), json!(color.hex()));
        }
    }
    let selection: Color = palette.primary.with_alpha(SELECTION_ALPHA);
    colors.insert("selection.background".into(), json!(selection.hex()));
    colors
}

/// GPUI Kit's theme config for `def`: its colours, its code highlighting when it has its own, the
/// interface and code fonts and the body text size. A highlight section GPUI Kit's strict
/// deserializer rejects (a user file with a malformed `syntax` entry) is dropped with a warning,
/// so the theme's colours still apply. Errors only when GPUI Kit rejects the rest (which the
/// tests rule out for every palette).
pub fn config(def: &ThemeDef, fonts: Option<&Fonts>) -> Result<ThemeConfig> {
    let mut config = json!({
        "name": def.name,
        "mode": if def.appearance.is_dark() { "dark" } else { "light" },
        "colors": colors(&def.palette),
        // Kit's `radius` (controls) is t3code's `--radius` / `rounded-lg`, 8 px; Kit's `radius.lg`
        // (dialogs, notifications) is t3code's `rounded-xl`, 12 px.
        // (Kit reads whole pixels here.)
        "radius": radius::LG.as_f32() as usize,
        "radius.lg": radius::XL.as_f32() as usize,
        "font.size": text::BODY.size.as_f32(),
        "mono_font.size": text::CODE.size.as_f32(),
    });
    if let Some(fonts) = fonts {
        config["font.family"] = json!(fonts.ui);
        config["mono_font.family"] = json!(fonts.mono);
    }
    if let Some(highlight) = &def.highlight {
        let mut highlight = highlight.clone();
        // GPUI Kit refuses a highlight section without `syntax`; a theme that has none gets none.
        highlight.entry("syntax").or_insert_with(|| json!({}));
        let mut with_highlight = config.clone();
        with_highlight["highlight"] = Value::Object(highlight);
        match serde_json::from_value(with_highlight) {
            Ok(built) => return Ok(built),
            Err(error) => tracing::warn!(
                "theme {}: its code colours were not usable ({error}); using the default ones",
                def.name
            ),
        }
    }
    serde_json::from_value(config).context("building the GPUI Kit theme")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::tokens::Appearance;
    use crate::theme::Source;

    fn def(appearance: Appearance) -> ThemeDef {
        ThemeDef {
            name: "Test".into(),
            appearance,
            palette: Palette::default_for(appearance),
            highlight: None,
            source: Source::Builtin,
        }
    }

    #[test]
    fn the_table_names_real_tokens_and_unique_kit_keys() {
        let palette = Palette::light();
        let mut keys: Vec<_> = KIT_COLORS.iter().map(|(k, _)| *k).collect();
        for (key, token) in KIT_COLORS {
            assert!(
                palette.get(token).is_some(),
                "{key} reads unknown token {token}"
            );
        }
        keys.sort_unstable();
        let total = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), total, "a Kit key is listed twice");
    }

    #[test]
    fn gpui_kit_keeps_every_colour_we_write() {
        // Kit's deserializer drops a key it does not know without a word: every key in the table
        // must come back out of the parsed config (CP0.1's review found this with the links).
        for appearance in [Appearance::Light, Appearance::Dark] {
            let config = config(&def(appearance), None).expect("builds");
            let kept = serde_json::to_value(&config.colors).expect("serializes");
            for (key, _) in KIT_COLORS {
                let color = kept[*key]
                    .as_str()
                    .unwrap_or_else(|| panic!("GPUI Kit dropped {key}"));
                gpui_kit::component::try_parse_color(color)
                    .unwrap_or_else(|e| panic!("{key} = {color}: {e}"));
            }
            assert!(kept["selection.background"].is_string());
            assert_eq!(config.mode.is_dark(), appearance.is_dark());
        }
    }

    #[test]
    fn the_config_carries_tokens_fonts_and_highlight() {
        let mut dark = def(Appearance::Dark);
        let mut style = Map::new();
        style.insert("editor_background".into(), json!("#101010ff"));
        // A style without `syntax` (a sparse user theme) still builds.
        dark.highlight = Some(style);
        let fonts = Fonts {
            ui: "Segoe UI".into(),
            mono: "Consolas".into(),
        };
        let config = config(&dark, Some(&fonts)).expect("builds");
        assert_eq!(config.colors.background.as_deref(), Some("#000000"));
        assert_eq!(config.colors.border.as_deref(), Some("#ffffff14"));
        assert_eq!(config.font_family.as_deref(), Some("Segoe UI"));
        assert_eq!(config.mono_font_family.as_deref(), Some("Consolas"));
        assert_eq!(config.font_size, Some(14.));
        assert!(
            config
                .highlight
                .expect("highlight")
                .editor_background
                .is_some()
        );
        assert_eq!((config.radius, config.radius_lg), (Some(8), Some(12)));
        // No fonts given: the config leaves GPUI Kit's own.
        assert!(
            super::config(&def(Appearance::Dark), None)
                .unwrap()
                .font_family
                .is_none()
        );
    }

    #[test]
    fn code_colours_kit_rejects_are_dropped_and_the_theme_still_builds() {
        // Our palette mapping skips a broken key, but Kit's highlight deserializer is strict: a
        // user file like this must not make the theme impossible to pick.
        let mut dark = def(Appearance::Dark);
        let mut style = Map::new();
        style.insert("syntax".into(), json!(5));
        dark.highlight = Some(style);
        let config = config(&dark, None).expect("builds without the highlight");
        assert!(config.highlight.is_none());
        assert_eq!(config.colors.background.as_deref(), Some("#000000"));
    }
}
