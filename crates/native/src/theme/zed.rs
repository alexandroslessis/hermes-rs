//! Zed's theme family files, read as serde structs and mapped onto our tokens, so any Zed theme
//! (the bundled ones, or a file the user drops into `<data>/themes/`) can be the app's theme.
//! No Zed code: the file format is read as JSON (`$schema` `zed.dev/schema/themes/v0.2.0.json`)
//! and only the fields we map are typed. Replaces CP0.1's spike mapping (`zed_theme.rs`).
//!
//! A theme file is a *family* (`{"name", "author", "themes": [..]}`), each theme an `appearance`
//! (`light` or `dark`) and a `style`. We map `style`'s UI colours onto [`Palette`] tokens through
//! [`MAPPING`], starting from the default palette of the theme's appearance, so a key a theme
//! leaves out (or writes as `null`, or breaks) falls back to t3code's value instead of leaving a
//! hole. The raw `style` also goes to GPUI Kit's `highlight` section unchanged (same format:
//! editor colours, status colours and `syntax`), which is what colours code blocks.
//!
//! t3code has no Zed-theme loader; the token each Zed key lands on is the surface that plays the
//! same role in t3code's CSS (`--card` is Zed's `surface.background`, `--popover` its
//! `elevated_surface.background`, `--muted-foreground` its `text.muted`, `--ring` its
//! `border.focused`). t3code: `apps/web/src/index.css` (the variable names).

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::design::tokens::{Appearance, Color, Palette};

/// A colour field: the string if the value is one, nothing for `null` or anything else (a theme
/// with one broken key keeps the rest of its colours).
fn lenient_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Value::deserialize(deserializer)?
        .as_str()
        .map(str::to_owned))
}

/// Declares [`Style`]: one `Option<String>` field per Zed `style` key we map, with the key as its
/// serde name, and [`Style::color`] to look one up by that key.
macro_rules! zed_style {
    ($($field:ident: $key:literal),* $(,)?) => {
        /// The UI colours of a Zed theme's `style` that we map (the other keys are ignored here;
        /// the whole `style` still reaches GPUI Kit as the highlight section). Colours are
        /// `#rrggbbaa` strings as written in the file.
        #[derive(Clone, Debug, Default, Deserialize)]
        #[serde(default)]
        pub struct Style {
            $(
                #[serde(rename = $key, deserialize_with = "lenient_string")]
                $field: Option<String>,
            )*
            /// The multiplayer colours; the first is the local user's (caret and selection).
            #[serde(deserialize_with = "lenient_players")]
            pub players: Vec<Player>,
        }

        impl Style {
            /// Every Zed key we read, for tests and for the docs.
            pub const KEYS: &'static [&'static str] = &[$($key),*];

            /// The colour string written under Zed `style` key `key`, if there is one.
            pub fn color(&self, key: &str) -> Option<&str> {
                match key {
                    $($key => self.$field.as_deref(),)*
                    _ => None,
                }
            }
        }
    };
}

zed_style! {
    border: "border",
    border_variant: "border.variant",
    border_focused: "border.focused",
    elevated_surface_background: "elevated_surface.background",
    surface_background: "surface.background",
    element_background: "element.background",
    element_hover: "element.hover",
    ghost_element_hover: "ghost_element.hover",
    ghost_element_active: "ghost_element.active",
    ghost_element_selected: "ghost_element.selected",
    text: "text",
    text_muted: "text.muted",
    text_placeholder: "text.placeholder",
    text_accent: "text.accent",
    icon_muted: "icon.muted",
    editor_background: "editor.background",
    panel_background: "panel.background",
    scrollbar_thumb_background: "scrollbar.thumb.background",
    scrollbar_thumb_hover_background: "scrollbar.thumb.hover_background",
    terminal_background: "terminal.background",
    terminal_foreground: "terminal.foreground",
    error: "error",
    error_background: "error.background",
    warning: "warning",
    warning_background: "warning.background",
    success: "success",
    info: "info",
}

/// One of a theme's players: the colours of a caret and its selection.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Player {
    /// The caret.
    #[serde(deserialize_with = "lenient_string")]
    pub cursor: Option<String>,
    /// The selection behind the text.
    #[serde(deserialize_with = "lenient_string")]
    pub selection: Option<String>,
}

/// `players` as a list; anything else (or a broken entry) counts as no players.
fn lenient_players<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Player>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .map(|players| {
            players
                .iter()
                .map(|p| serde_json::from_value(p.clone()).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default())
}

/// One theme of a family, with its colours as tokens.
#[derive(Clone, Debug)]
pub struct ZedTheme {
    /// The theme's name (`One Dark`).
    pub name: String,
    /// Light or dark.
    pub appearance: Appearance,
    /// The mapped colours; keys the file lacks hold the appearance's default.
    pub palette: Palette,
    /// The whole `style` object, for GPUI Kit's `highlight` section (nulls removed).
    pub highlight: Map<String, Value>,
}

/// A parsed family file.
#[derive(Clone, Debug)]
pub struct Family {
    /// The family's name (`One`).
    pub name: String,
    /// Its themes, in file order.
    pub themes: Vec<ZedTheme>,
}

/// The file's outline: names and each theme's raw `style`, which [`Style`] then reads.
#[derive(Deserialize)]
struct RawFamily {
    #[serde(default)]
    name: String,
    themes: Vec<RawTheme>,
}

#[derive(Deserialize)]
struct RawTheme {
    #[serde(default)]
    name: String,
    #[serde(default)]
    appearance: String,
    #[serde(default)]
    style: Map<String, Value>,
}

/// Which token takes which Zed `style` key (token, Zed key). The first token listed for a key is
/// the main one; several tokens share a surface where t3code's CSS does (`--input` and
/// `--border` are one colour in Zed's). Tokens that no key is listed for keep the default
/// palette's value, or are derived in [`map_style`] (`message_action_hover`).
pub const MAPPING: &[(&str, &str)] = &[
    ("background", "editor.background"),
    ("foreground", "text"),
    ("card", "surface.background"),
    ("card_foreground", "text"),
    ("surface_raised", "surface.background"),
    ("popover", "elevated_surface.background"),
    ("popover_foreground", "text"),
    ("primary", "text.accent"),
    ("primary_foreground", "editor.background"),
    ("secondary", "element.background"),
    ("secondary_foreground", "text"),
    ("muted", "element.background"),
    ("muted_foreground", "text.muted"),
    ("placeholder", "text.placeholder"),
    ("icon_muted", "icon.muted"),
    ("secondary_label", "text.muted"),
    ("accent", "element.hover"),
    ("accent_foreground", "text"),
    ("border", "border"),
    ("input", "border"),
    ("ring", "border.focused"),
    ("message_surface", "element.background"),
    ("message_foreground", "text"),
    ("message_action", "text.accent"),
    ("message_action_foreground", "editor.background"),
    ("error", "error"),
    ("error_foreground", "error"),
    ("error_surface", "error.background"),
    ("destructive", "error"),
    ("destructive_foreground", "error"),
    ("info", "info"),
    ("info_foreground", "info"),
    ("success", "success"),
    ("success_foreground", "success"),
    ("warning", "warning"),
    ("warning_foreground", "warning"),
    ("warning_surface", "warning.background"),
    ("update", "text.accent"),
    ("update_foreground", "text.accent"),
    ("sidebar", "panel.background"),
    ("sidebar_foreground", "text"),
    ("sidebar_muted_foreground", "text.muted"),
    ("sidebar_secondary_label", "text.muted"),
    ("sidebar_control_surface", "element.background"),
    ("sidebar_row_hover", "ghost_element.hover"),
    ("sidebar_row_active", "ghost_element.active"),
    ("sidebar_row_selected", "ghost_element.selected"),
    ("sidebar_border", "border.variant"),
    ("sidebar_input", "border"),
    ("sidebar_accent", "element.hover"),
    ("code_background", "element.background"),
    ("code_foreground", "text"),
    ("terminal_background", "terminal.background"),
    ("terminal_foreground", "terminal.foreground"),
    ("scrollbar_thumb", "scrollbar.thumb.background"),
    ("scrollbar_thumb_hover", "scrollbar.thumb.hover_background"),
];

/// A Zed theme's `style` as a palette over the appearance's defaults.
pub fn map_style(appearance: Appearance, style: &Style) -> Palette {
    let mut palette = Palette::default_for(appearance);
    for (token, key) in MAPPING {
        if let Some(color) = style.color(key).and_then(Color::parse) {
            palette.set(token, color);
        }
    }
    // The terminal's caret and selection come from the first player, like the editor's.
    if let Some(player) = style.players.first() {
        if let Some(color) = player.cursor.as_deref().and_then(Color::parse) {
            palette.terminal_cursor = color;
        }
        if let Some(color) = player.selection.as_deref().and_then(Color::parse) {
            palette.terminal_selection = color;
        }
    }
    // t3code's hover colour for a primary control is primary at 90% over the canvas.
    palette.message_action_hover = palette
        .message_action
        .with_alpha(230)
        .over(palette.background);
    palette
}

/// Reads a Zed theme family file. Errors when the file is not JSON, has no `themes` list, or has
/// no usable theme; a theme inside it that is unusable (no name, an appearance other than light
/// or dark) is skipped with a warning, so it does not take its family's other themes with it.
/// A UTF-8 byte-order mark (Notepad writes one) is ignored.
pub fn parse_family(json: &str) -> Result<Family> {
    let json = json.strip_prefix('\u{feff}').unwrap_or(json);
    let raw: RawFamily = serde_json::from_str(json).context("reading the Zed theme file")?;
    let mut themes = Vec::new();
    let mut skipped = Vec::new();
    for theme in raw.themes {
        if theme.name.trim().is_empty() {
            skipped.push("a theme has no name".to_string());
            continue;
        }
        let appearance = match theme.appearance.as_str() {
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            other => {
                skipped.push(format!(
                    "theme {:?}: appearance {other:?} is neither light nor dark",
                    theme.name
                ));
                continue;
            }
        };
        // Every field of `Style` is lenient, so this only fails on a non-object, which the
        // `RawTheme` read already ruled out.
        let style: Style = serde_json::from_value(Value::Object(theme.style.clone()))
            .with_context(|| format!("theme {:?}: reading `style`", theme.name))?;
        themes.push(ZedTheme {
            palette: map_style(appearance, &style),
            highlight: without_nulls(&theme.style),
            appearance,
            name: theme.name,
        });
    }
    if themes.is_empty() {
        if skipped.is_empty() {
            bail!("the file has no usable theme");
        }
        bail!("the file has no usable theme: {}", skipped.join("; "));
    }
    for reason in &skipped {
        tracing::warn!("{reason}; skipped");
    }
    Ok(Family {
        name: raw.name,
        themes,
    })
}

/// Zed writes `null` for "not set"; GPUI Kit's colour fields take a colour or nothing.
///
/// GPUI Kit 0.10.0 built without `tree-sitter` (we use `syntect`, see Cargo.toml) declares its
/// highlight style without the `editor.*` serde renames, so it reads `editor_background` where
/// its own docs and theme files say `editor.background`: the dotted keys are silently ignored.
/// Both spellings are written so the editor colours load either way (CP0.1's finding; a test
/// below keeps it from rotting).
fn without_nulls(style: &Map<String, Value>) -> Map<String, Value> {
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = include_str!("../../assets/themes/one/one.json");
    const AYU: &str = include_str!("../../assets/themes/ayu/ayu.json");

    fn theme<'a>(family: &'a Family, name: &str) -> &'a ZedTheme {
        family
            .themes
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("no theme {name}"))
    }

    #[test]
    fn a_real_zed_theme_maps_onto_tokens() {
        let family = parse_family(ONE).expect("parses");
        assert_eq!(family.name, "One");
        let dark = theme(&family, "One Dark");
        assert_eq!(dark.appearance, Appearance::Dark);
        // `editor.background` is the canvas, `text` the foreground, `border` both the border and
        // the input border.
        assert_eq!(dark.palette.background.hex(), "#282c33");
        assert_eq!(dark.palette.foreground.hex(), "#dce0e5");
        assert_eq!(dark.palette.border.hex(), "#464b57");
        assert_eq!(dark.palette.input, dark.palette.border);
        let light = theme(&family, "One Light");
        assert_eq!(light.appearance, Appearance::Light);
        assert_ne!(light.palette.background, dark.palette.background);
        // The derived hover differs from the primary and is opaque.
        assert_ne!(
            dark.palette.message_action_hover,
            dark.palette.message_action
        );
        assert_eq!(dark.palette.message_action_hover.a, 255);
    }

    #[test]
    fn every_bundled_family_parses_with_both_appearances_mapped() {
        for (name, json) in [("one", ONE), ("ayu", AYU)] {
            let family = parse_family(json).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(!family.themes.is_empty(), "{name}");
            for theme in &family.themes {
                // Every mapped key the theme sets was read (a key that is in the file but not in
                // `Style` would silently not be mapped).
                assert!(
                    theme.highlight.contains_key("editor_background"),
                    "{name}/{}: the editor.* spelling for GPUI Kit is missing",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn the_mapping_names_real_tokens_and_keys() {
        let palette = Palette::light();
        for (token, key) in MAPPING {
            assert!(palette.get(token).is_some(), "no token {token}");
            assert!(Style::KEYS.contains(key), "Style does not read {key}");
        }
        // Every key Style reads is a key some token uses (no dead fields).
        for key in Style::KEYS {
            assert!(
                MAPPING.iter().any(|(_, k)| k == key),
                "{key} is read but nothing maps it"
            );
        }
    }

    #[test]
    fn missing_null_and_broken_keys_fall_back_to_the_defaults() {
        let json = r##"{"name": "Sparse", "themes": [
            {"name": "Sparse Dark", "appearance": "dark", "style": {
                "editor.background": "#112233ff",
                "text": null,
                "text.muted": 42,
                "border": "not a colour",
                "players": "nope"
            }},
            {"name": "Bare Light", "appearance": "light"}
        ]}"##;
        let family = parse_family(json).expect("parses");
        let sparse = theme(&family, "Sparse Dark");
        let defaults = Palette::dark();
        assert_eq!(sparse.palette.background.hex(), "#112233");
        assert_eq!(sparse.palette.foreground, defaults.foreground, "null");
        assert_eq!(
            sparse.palette.muted_foreground, defaults.muted_foreground,
            "number"
        );
        assert_eq!(sparse.palette.border, defaults.border, "unparsable colour");
        assert_eq!(
            sparse.palette.terminal_cursor, defaults.terminal_cursor,
            "players"
        );
        // A theme with no `style` at all is its appearance's defaults (the hover is derived).
        let bare = theme(&family, "Bare Light");
        assert_eq!(bare.palette.background, Palette::light().background);
    }

    #[test]
    fn broken_files_are_errors_not_panics() {
        assert!(parse_family("").is_err());
        assert!(parse_family("{").is_err());
        assert!(parse_family("[1, 2]").is_err());
        assert!(parse_family(r#"{"themes": []}"#).is_err());
        assert!(parse_family(r#"{"themes": [{"name": "X", "appearance": "sepia"}]}"#).is_err());
        assert!(parse_family(r#"{"themes": [{"appearance": "dark"}]}"#).is_err());
    }

    #[test]
    fn an_unusable_theme_is_skipped_and_its_family_still_loads() {
        // A BOM, a theme with an unknown appearance and one with no name, next to a good one.
        let json = "\u{feff}{\"themes\": [
            {\"name\": \"X HC\", \"appearance\": \"high-contrast\"},
            {\"appearance\": \"dark\"},
            {\"name\": \"X Dark\", \"appearance\": \"dark\"}
        ]}";
        let family = parse_family(json).expect("the good theme loads");
        let names: Vec<_> = family.themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["X Dark"]);
        // With nothing usable left the file is an error that says why.
        let error = parse_family(r#"{"themes": [{"name": "X", "appearance": "sepia"}]}"#)
            .expect_err("nothing usable");
        assert!(format!("{error:#}").contains("sepia"), "{error:#}");
    }

    #[test]
    fn the_editor_colours_are_written_in_both_spellings_for_gpui_kit() {
        let family = parse_family(ONE).expect("parses");
        let dark = theme(&family, "One Dark");
        assert_eq!(
            dark.highlight.get("editor.background"),
            dark.highlight.get("editor_background")
        );
        assert!(
            dark.highlight.values().all(|v| !v.is_null()),
            "nulls are removed"
        );
    }
}
