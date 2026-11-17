//! A Zed theme file mapped by hand onto GPUI Kit's theme (the spike's check that Zed themes can
//! keep working, CP0.1 of `docs/native-ui/PHASES.md`). No Zed code: the theme file is read as
//! JSON and its `style` keys are copied into a GPUI Kit theme config.
//!
//! GPUI Kit's own theme format is close to Zed's: its `highlight` section takes Zed's `style`
//! keys as they are (editor colours, status colours and `syntax`), so only the UI colours need a
//! table ([`COLOR_MAP`]). CP2.1 replaces this with typed serde structs and our own tokens.

use anyhow::{Context as _, Result};
use gpui_kit::component::ThemeConfig;
use serde_json::{Map, Value, json};

/// GPUI Kit colour key â† Zed `style` key. Where Zed has no direct equivalent the closest surface
/// is used (t3code's `background` is Zed's editor background, its `muted` an element background).
pub const COLOR_MAP: &[(&str, &str)] = &[
    ("background", "editor.background"),
    ("foreground", "text"),
    ("border", "border"),
    ("window.border", "border"),
    ("input.border", "border"),
    ("ring", "border.focused"),
    ("muted.background", "element.background"),
    ("muted.foreground", "text.muted"),
    ("accent.background", "element.hover"),
    ("accent.foreground", "text"),
    ("primary.background", "text.accent"),
    ("primary.hover.background", "text.accent"),
    ("primary.active.background", "text.accent"),
    ("primary.foreground", "editor.background"),
    ("secondary.background", "element.background"),
    ("secondary.hover.background", "element.hover"),
    ("secondary.active.background", "element.active"),
    ("secondary.foreground", "text"),
    ("popover.background", "elevated_surface.background"),
    ("popover.foreground", "text"),
    ("sidebar.background", "panel.background"),
    ("sidebar.foreground", "text"),
    ("sidebar.border", "border.variant"),
    ("sidebar.accent.background", "element.hover"),
    ("sidebar.accent.foreground", "text"),
    ("sidebar.primary.background", "text.accent"),
    ("sidebar.primary.foreground", "editor.background"),
    ("list.background", "panel.background"),
    ("list.hover.background", "ghost_element.hover"),
    ("list.active.background", "ghost_element.selected"),
    ("list.active.border", "border.selected"),
    ("list.even.background", "panel.background"),
    ("list.head.background", "panel.background"),
    ("link", "text.accent"),
    ("link.hover", "text.accent"),
    ("link.active", "text.accent"),
    ("scrollbar.background", "scrollbar.track.background"),
    ("scrollbar.thumb.background", "scrollbar.thumb.background"),
    (
        "scrollbar.thumb.hover.background",
        "scrollbar.thumb.hover_background",
    ),
    ("title_bar.background", "title_bar.background"),
    ("title_bar.border", "border.variant"),
    ("status_bar.background", "status_bar.background"),
    ("status_bar.border", "border.variant"),
    ("tab_bar.background", "tab_bar.background"),
    ("tab.background", "tab.inactive_background"),
    ("tab.active.background", "tab.active_background"),
    ("tab.foreground", "text.muted"),
    ("tab.active.foreground", "text"),
    ("table.background", "editor.background"),
    ("table.head.background", "element.background"),
    ("table.head.foreground", "text"),
    ("table.hover.background", "ghost_element.hover"),
    ("table.row.border", "border.variant"),
    ("drop_target.background", "drop_target.background"),
    ("danger.background", "error"),
    ("success.background", "success"),
    ("warning.background", "warning"),
    ("info.background", "info"),
];

/// One theme of a Zed theme family file, by name, as a GPUI Kit theme config.
pub fn kit_theme(family_json: &str, name: &str) -> Result<ThemeConfig> {
    let family: Value = serde_json::from_str(family_json).context("reading the Zed theme file")?;
    let theme = family["themes"]
        .as_array()
        .and_then(|themes| themes.iter().find(|t| t["name"] == name))
        .with_context(|| format!("no theme named {name:?} in the file"))?;
    let style = theme["style"]
        .as_object()
        .context("the theme has no `style`")?;

    let mut colors = Map::new();
    for (kit_key, zed_key) in COLOR_MAP {
        if let Some(color) = style.get(*zed_key).and_then(Value::as_str) {
            colors.insert((*kit_key).to_string(), json!(color));
        }
    }
    // Text selection and the caret come from the first player, like in Zed.
    if let Some(player) = style.get("players").and_then(|p| p.get(0)) {
        if let Some(selection) = player["selection"].as_str() {
            colors.insert("selection.background".into(), json!(selection));
        }
        if let Some(cursor) = player["cursor"].as_str() {
            colors.insert("caret".into(), json!(cursor));
        }
    }

    let mode = if theme["appearance"] == "light" {
        "light"
    } else {
        "dark"
    };
    let config = json!({
        "name": name,
        "mode": mode,
        "colors": colors,
        // Zed's `style` is what GPUI Kit's `highlight` section reads; the keys it doesn't know
        // (UI colours, `players`, terminal colours) are ignored by its deserializer.
        "highlight": without_nulls(style),
    });
    serde_json::from_value(config).context("building the GPUI Kit theme")
}

/// Zed writes `null` for "not set"; GPUI Kit's colour fields take a colour or nothing.
///
/// GPUI Kit 0.10.0 built without `tree-sitter` (we use `syntect`, see Cargo.toml) declares its
/// highlight style without the `editor.*` serde renames, so it reads `editor_background` where
/// its own docs and theme files say `editor.background`. Both spellings are written so the
/// editor colours load either way.
fn without_nulls(style: &Map<String, Value>) -> Value {
    let mut out: Map<String, Value> = style
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (dotted, snake) in [
        ("editor.background", "editor_background"),
        ("editor.foreground", "editor_foreground"),
        ("editor.active_line.background", "editor_active_line"),
        ("editor.line_number", "editor_line_number"),
        ("editor.active_line_number", "editor_active_line_number"),
        ("editor.invisible", "editor_invisible"),
    ] {
        if let Some(value) = out.get(dotted).cloned() {
            out.insert(snake.to_string(), value);
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = include_str!("../../../assets/themes/one/one.json");

    #[test]
    fn one_dark_maps_onto_a_kit_theme() {
        let config = kit_theme(ONE, "One Dark").expect("maps");
        assert!(config.mode.is_dark());
        assert_eq!(config.name.as_ref(), "One Dark");
        assert_eq!(config.colors.background.as_deref(), Some("#282c33ff"));
        assert_eq!(config.colors.foreground.as_deref(), Some("#dce0e5ff"));
        let highlight = config.highlight.as_ref().expect("highlight section");
        assert!(highlight.editor_background.is_some());
        assert!(
            highlight.syntax.keyword.is_some(),
            "syntax colours carried over"
        );
    }

    #[test]
    fn one_light_is_a_light_theme() {
        let config = kit_theme(ONE, "One Light").expect("maps");
        assert!(!config.mode.is_dark());
    }

    #[test]
    fn every_mapped_zed_colour_parses() {
        let config = kit_theme(ONE, "One Dark").expect("maps");
        let colors = serde_json::to_value(&config.colors).expect("serializes");
        let family: Value = serde_json::from_str(ONE).unwrap();
        let style = &family["themes"][0]["style"];
        assert_eq!(family["themes"][0]["name"], "One Dark");
        for (kit_key, zed_key) in COLOR_MAP {
            if !style[*zed_key].is_string() {
                continue; // Zed leaves it unset in this theme.
            }
            // A kit key GPUI Kit doesn't know is dropped silently by its deserializer.
            let color = colors[*kit_key]
                .as_str()
                .unwrap_or_else(|| panic!("{kit_key} (from {zed_key}) was not kept"));
            gpui_kit::component::try_parse_color(color)
                .unwrap_or_else(|e| panic!("{kit_key} = {color}: {e}"));
        }
    }

    #[test]
    fn an_unknown_theme_name_is_an_error() {
        assert!(kit_theme(ONE, "No Such Theme").is_err());
    }
}
