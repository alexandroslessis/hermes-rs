//! The terminal's colours, with no GPUI in it: the canvas, text, cursor and selection from the
//! theme's tokens (`terminal_background`, `terminal_foreground`, `terminal_cursor`,
//! `terminal_selection`), the 16 ANSI colours from the theme (a Zed theme's `terminal.ansi.*`) or
//! t3code's palettes, xterm's 6x6x6 cube and grey ramp above 15, and true colour as it comes. A
//! program's own palette changes (OSC 4 / 10 / 11 / 12) win over all of these.
//!
//! t3code: `apps/web/src/components/ThreadTerminalDrawer.tsx` (`TERMINAL_PALETTES`,
//! `terminalThemeFromApp`: the `--terminal-*` variables and the 16 colours per light and dark).

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};

use crate::design::Color;
use crate::theme::ThemeDef;

/// t3code's 16 colours on a dark canvas (`TERMINAL_PALETTES.dark`).
pub const T3_DARK: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
    0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];

/// t3code's 16 colours on a light canvas (`TERMINAL_PALETTES.light`), with three of its bright
/// colours darkened (a deviation, CP6.3 review): bright green `#14ce14`, bright yellow `#b5ba00`
/// and bright white `#a5a5a5` are about 2:1 against the light canvas and hard to read; here they
/// are `#0e9a0e`, `#8f8a00` and `#8c8c8c` (3.4:1 or more, still apart from their normal colours).
pub const T3_LIGHT: [u32; 16] = [
    0x000000, 0xcd3131, 0x107c10, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, 0x666666,
    0xcd3131, 0x0e9a0e, 0x8f8a00, 0x0451a5, 0xbc05bc, 0x0598bc, 0x8c8c8c,
];

/// The names of Zed's `terminal.ansi.*` keys, in ANSI order.
const ZED_ANSI: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

/// The colours a terminal is drawn with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermColors {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    /// Selection, translucent over the canvas.
    pub selection: Color,
    pub ansi: [Color; 16],
}

impl TermColors {
    /// The colours of `theme`: its terminal tokens, its `terminal.ansi.*` when a Zed theme names
    /// them, else t3code's palette for its light or dark.
    pub fn of(theme: &ThemeDef) -> Self {
        let palette = &theme.palette;
        let base = if theme.appearance.is_dark() {
            T3_DARK
        } else {
            T3_LIGHT
        };
        let mut ansi = base.map(Color::rgb);
        if let Some(style) = &theme.highlight {
            for (ix, name) in ZED_ANSI.iter().enumerate() {
                if let Some(color) = style
                    .get(&format!("terminal.ansi.{name}"))
                    .and_then(|v| v.as_str())
                    .and_then(Color::parse)
                {
                    ansi[ix] = color.with_alpha(255);
                }
            }
        }
        Self {
            background: palette.terminal_background.with_alpha(255),
            foreground: palette.terminal_foreground.with_alpha(255),
            cursor: palette.terminal_cursor.with_alpha(255),
            selection: palette.terminal_selection,
            ansi,
        }
    }

    /// The colour of palette index `ix` (0-255): the 16, then xterm's cube and grey ramp.
    pub fn indexed(&self, ix: u8) -> Color {
        match ix {
            0..=15 => self.ansi[usize::from(ix)],
            16..=231 => {
                let n = ix - 16;
                let level = |v: u8| if v == 0 { 0u32 } else { 55 + 40 * u32::from(v) };
                Color::rgb(level(n / 36) << 16 | level(n / 6 % 6) << 8 | level(n % 6))
            }
            _ => {
                let v = 8 + 10 * u32::from(ix - 232);
                Color::rgb(v << 16 | v << 8 | v)
            }
        }
    }

    /// The colour `color` means, with a program's own palette changes (`overrides`) first.
    pub fn resolve(&self, color: AnsiColor, overrides: &Colors) -> Color {
        match color {
            AnsiColor::Spec(rgb) => from_rgb(rgb),
            AnsiColor::Indexed(ix) => overrides[usize::from(ix)]
                .map(from_rgb)
                .unwrap_or_else(|| self.indexed(ix)),
            AnsiColor::Named(named) => {
                if let Some(rgb) = overrides[named] {
                    return from_rgb(rgb);
                }
                match named {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
                    NamedColor::Background => self.background,
                    NamedColor::Cursor => self.cursor,
                    NamedColor::DimForeground => dim(self.foreground, self.background),
                    other => {
                        let ix = other as usize;
                        if ix < 16 {
                            self.ansi[ix]
                        } else {
                            // Dim black .. dim white (259..267): the colour dimmed.
                            let base = self.ansi[(ix - NamedColor::DimBlack as usize).min(7)];
                            dim(base, self.background)
                        }
                    }
                }
            }
        }
    }

    /// The RGB of palette slot `ix` for a program that asks (OSC 4 / 10 / 11 / 12 with `?`).
    pub fn rgb_of_slot(&self, ix: usize, overrides: &Colors) -> Rgb {
        let color = match ix {
            0..=255 => overrides[ix]
                .map(from_rgb)
                .unwrap_or_else(|| self.indexed(ix as u8)),
            256 => overrides[ix].map(from_rgb).unwrap_or(self.foreground),
            257 => overrides[ix].map(from_rgb).unwrap_or(self.background),
            _ => overrides[ix].map(from_rgb).unwrap_or(self.cursor),
        };
        Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

fn from_rgb(rgb: Rgb) -> Color {
    Color {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
        a: 255,
    }
}

/// `color` two thirds of the way from `background` (SGR 2, faint).
pub fn dim(color: Color, background: Color) -> Color {
    color.with_alpha(170).over(background)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeDef;

    #[test]
    fn the_sixteen_come_from_t3code_and_the_rest_from_xterm() {
        let dark = TermColors::of(&ThemeDef::default_dark());
        assert_eq!(dark.indexed(1), Color::rgb(0xcd3131));
        assert_eq!(dark.indexed(10), Color::rgb(0x23d18b));
        // The cube: 16 is black, 196 pure red (5,0,0), 231 white.
        assert_eq!(dark.indexed(16), Color::rgb(0x000000));
        assert_eq!(dark.indexed(196), Color::rgb(0xff0000));
        assert_eq!(dark.indexed(231), Color::rgb(0xffffff));
        // The grey ramp.
        assert_eq!(dark.indexed(232), Color::rgb(0x080808));
        assert_eq!(dark.indexed(255), Color::rgb(0xeeeeee));
        let light = TermColors::of(&ThemeDef::default_light());
        assert_eq!(light.indexed(2), Color::rgb(0x107c10));
        assert_ne!(dark.background, light.background);
    }

    /// WCAG relative luminance.
    fn luminance(c: Color) -> f32 {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    /// Every colour a program writes text in can be read on the light canvas (3:1, WCAG's
    /// large-text bar; t3code's bright green, yellow and white were about 2:1).
    #[test]
    fn the_light_palette_is_readable_on_the_light_canvas() {
        let light = TermColors::of(&ThemeDef::default_light());
        let bg = luminance(light.background);
        for (ix, color) in light.ansi.iter().enumerate() {
            let fg = luminance(*color);
            let ratio = (bg.max(fg) + 0.05) / (bg.min(fg) + 0.05);
            assert!(ratio >= 3.0, "colour {ix} {color:?}: {ratio:.2}:1");
        }
    }

    #[test]
    fn a_zed_theme_names_its_own_ansi_colours_and_a_program_overrides_them() {
        let mut theme = ThemeDef::default_dark();
        let mut style = serde_json::Map::new();
        style.insert("terminal.ansi.red".into(), "#ff0011".into());
        theme.highlight = Some(style);
        let colors = TermColors::of(&theme);
        assert_eq!(colors.ansi[1], Color::rgb(0xff0011));
        assert_eq!(colors.ansi[2], Color::rgb(T3_DARK[2]));
        let mut overrides = Colors::default();
        assert_eq!(
            colors.resolve(AnsiColor::Named(NamedColor::Red), &overrides),
            Color::rgb(0xff0011)
        );
        overrides[1] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            colors.resolve(AnsiColor::Indexed(1), &overrides),
            Color::rgb(0x010203)
        );
        assert_eq!(
            colors.resolve(AnsiColor::Spec(Rgb { r: 9, g: 8, b: 7 }), &overrides),
            Color::rgb(0x090807)
        );
    }
}
