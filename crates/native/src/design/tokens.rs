//! The design tokens: colours named after t3code's CSS variables, the 4 px spacing grid, radii,
//! control heights and font sizes. Everything in the native UI that has a colour, a gap or a size
//! takes it from here (through [`crate::theme::DesignTheme`] for colours), so the look changes in
//! one place and a theme can replace the colours without touching a view.
//!
//! The default light and dark palettes are t3code's own, converted from `index.css` to sRGB:
//! Tailwind's oklch greys and `color-mix()` results are evaluated once (the derivation is in the
//! comment of each value that is not a plain Tailwind colour) and written as hex. Alpha colours
//! stay alpha (`--muted` in the dark theme is white at 3%), as in the CSS, so they look right on
//! any surface.
//!
//! Two scopes exist in t3code's CSS: the page (`:root`, `.dark`) and the sidebar
//! (`[data-app-sidebar]` on the left sidebar's root in `AppSidebarLayout.tsx`, which re-declares
//! `--background`, `--foreground`, `--card`, `--muted*`, `--accent`, `--border`, `--input`,
//! `--secondary-label` and the `--sidebar-*` family for its subtree, and recomputes `--contrast-*`
//! there). The `sidebar_*` tokens here are the sidebar scope's values (inside the sidebar, use
//! them, not the page tokens of the same role); the others are the page's. The sidebar scope's
//! `--background` and `--card` equal `sidebar`, and its `--accent-foreground` (#f7f9ff dark) is
//! not worth a token. `--contrast-*` variables are the same colours at t3code's default contrast
//! setting, so they have no tokens.
//!
//! Screenshot-only differences (checked against `docs/native-ui/reference/t3code/`): t3code
//! paints a 3.5% fractal-noise grain over the body (`--surface-grain`), so the sidebar's #000
//! shows as noise around #030303; and floating surfaces are "glass", a token at
//! [`GLASS_OPACITY`] over what is behind (the dark composer is `surface_raised` #111 at 80% over
//! black: #0e0e0e). Neither changes a token. The right panel's #0f0f0f and its #252525 edge are
//! `dropdown-glass` (CP2.2): the tokens `panel_surface` (the popover at [`PANEL_ALPHA`]) and
//! `panel_edge` (the foreground at 10%).
//!
//! t3code: `apps/web/src/index.css` (`:root` and `@variant dark` blocks, `[data-app-sidebar]`,
//! `@theme inline` radii and text sizes), `apps/web/src/components/ui/button.tsx` (control
//! sizes: `icon-micro` and `micro` h-5, `xs` h-6, `sm` h-7, `default` h-8, `lg` h-9 on desktop).
//! Screenshot check of the dark values: `docs/native-ui/progress/phase-2.md`.

use gpui_kit::{Hsla, Pixels, Rgba, px, rgba};

/// An sRGB colour with alpha, 8 bits a channel. A plain value so palettes are `const`-friendly and
/// tests can compare exact numbers; [`Color::hsla`] converts it for GPUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red, 0–255.
    pub r: u8,
    /// Green, 0–255.
    pub g: u8,
    /// Blue, 0–255.
    pub b: u8,
    /// Alpha, 0 (clear) to 255 (opaque).
    pub a: u8,
}

impl Color {
    /// Opaque colour from `0xRRGGBB`.
    pub const fn rgb(hex: u32) -> Self {
        Self {
            r: (hex >> 16) as u8,
            g: (hex >> 8) as u8,
            b: hex as u8,
            a: 255,
        }
    }

    /// Colour with alpha from `0xRRGGBBAA`.
    pub const fn rgba(hex: u32) -> Self {
        Self {
            r: (hex >> 24) as u8,
            g: (hex >> 16) as u8,
            b: (hex >> 8) as u8,
            a: hex as u8,
        }
    }

    /// The same colour with another alpha.
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// `#rrggbb`, or `#rrggbbaa` when not opaque (the spelling Zed's theme files and GPUI Kit's
    /// theme config use).
    pub fn hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    /// Parses `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` (the `#` is optional).
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.trim().trim_start_matches('#');
        if !digits.is_ascii() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let nibble = |ix: usize| {
            u8::from_str_radix(&digits[ix..=ix], 16)
                .ok()
                .map(|n| n * 17)
        };
        let byte = |ix: usize| u8::from_str_radix(&digits[ix..ix + 2], 16).ok();
        match digits.len() {
            3 => Some(Self {
                r: nibble(0)?,
                g: nibble(1)?,
                b: nibble(2)?,
                a: 255,
            }),
            4 => Some(Self {
                r: nibble(0)?,
                g: nibble(1)?,
                b: nibble(2)?,
                a: nibble(3)?,
            }),
            6 => Some(Self {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: 255,
            }),
            8 => Some(Self {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: byte(6)?,
            }),
            _ => None,
        }
    }

    /// This colour painted over an opaque `background`: what the eye (and a screenshot) sees.
    pub fn over(self, background: Color) -> Color {
        let alpha = f32::from(self.a) / 255.;
        let mix = |top: u8, below: u8| {
            (f32::from(top) * alpha + f32::from(below) * (1. - alpha)).round() as u8
        };
        Color {
            r: mix(self.r, background.r),
            g: mix(self.g, background.g),
            b: mix(self.b, background.b),
            a: 255,
        }
    }

    /// The GPUI colour.
    pub fn gpui_rgba(self) -> Rgba {
        rgba(u32::from_be_bytes([self.r, self.g, self.b, self.a]))
    }

    /// The GPUI colour as [`Hsla`], what styles take.
    pub fn hsla(self) -> Hsla {
        self.gpui_rgba().into()
    }
}

impl From<Color> for Hsla {
    fn from(color: Color) -> Self {
        color.hsla()
    }
}

/// Declares [`Palette`] from a list of token names, so the struct, the list of names
/// ([`Palette::NAMES`]) and name-based access cannot drift apart.
macro_rules! palette {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {
        /// Every colour token, named after the t3code CSS variable it comes from
        /// (`--muted-foreground` is `muted_foreground`). A palette sets all of them: the light
        /// and dark defaults below are struct literals, so a missing token does not compile.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct Palette {
            $($(#[$meta])* pub $name: Color,)*
        }

        impl Palette {
            /// The token names, in declaration order.
            pub const NAMES: &'static [&'static str] = &[$(stringify!($name)),*];

            /// Every token with its name.
            pub fn entries(&self) -> Vec<(&'static str, Color)> {
                vec![$((stringify!($name), self.$name)),*]
            }

            /// The token called `name`, if there is one.
            pub fn get(&self, name: &str) -> Option<Color> {
                match name {
                    $(stringify!($name) => Some(self.$name),)*
                    _ => None,
                }
            }

            /// Sets the token called `name`; false when there is none.
            pub fn set(&mut self, name: &str, color: Color) -> bool {
                match name {
                    $(stringify!($name) => self.$name = color,)*
                    _ => return false,
                }
                true
            }
        }
    };
}

palette! {
    /// The canvas behind everything (`--background`).
    background,
    /// Default text on the canvas (`--foreground`).
    foreground,
    /// Raised surfaces: cards, panels (`--card`).
    card,
    /// Text on a card.
    card_foreground,
    /// The glass surface of the composer in the dark theme and of attached banners
    /// (`--surface-raised`), drawn at [`GLASS_OPACITY`] over the canvas. In the light theme the
    /// composer uses `card` instead (`ComposerSurface.tsx`).
    surface_raised,
    /// Menus, popovers and the palette (`--popover`).
    popover,
    /// The glass panel behind the right panel, toasts and floating cards: `dropdown-glass`'s
    /// fill, `--popover` at [`PANEL_ALPHA`] (18% of it over 80% of it). Dark: #0e0e0e over the
    /// canvas, which the screenshots show as #0f0f0f (CP2.1's "right panel" difference).
    panel_surface,
    /// The edge of a glass panel: `dropdown-glass`'s 1 px border, the foreground at 10%
    /// (#252525 over the dark panel: CP2.1's "panel edge" difference).
    panel_edge,
    /// Text on a popover.
    popover_foreground,
    /// The brand colour: primary buttons, links, focus (`--primary`).
    primary,
    /// Text on the primary colour.
    primary_foreground,
    /// Quiet button fill (`--secondary`).
    secondary,
    /// Text on secondary.
    secondary_foreground,
    /// Quiet surfaces (`--muted`).
    muted,
    /// Secondary text (`--muted-foreground`).
    muted_foreground,
    /// Placeholder text in inputs (`--placeholder`).
    placeholder,
    /// Icons that sit back (`--icon-muted`).
    icon_muted,
    /// Row metadata a step below muted text (`--secondary-label`).
    secondary_label,
    /// Hover and pressed fill of ghost controls (`--accent`).
    accent,
    /// Text on accent.
    accent_foreground,
    /// Dividers and outlines (`--border`).
    border,
    /// Input and outline-button borders (`--input`).
    input,
    /// The focus ring (`--ring`).
    ring,
    /// A user message's bubble (`--message-surface`).
    message_surface,
    /// Text in a user message.
    message_foreground,
    /// A button inside a message (`--message-action`).
    message_action,
    /// Text on a message action.
    message_action_foreground,
    /// A message action under the pointer: primary at 90% over the canvas.
    message_action_hover,
    /// Error accent (`--error`).
    error,
    /// Error text (`--error-foreground`).
    error_foreground,
    /// Tint behind an error banner (`--error-surface`).
    error_surface,
    /// Destructive buttons (`--destructive`, the same as `error`).
    destructive,
    /// Text for destructive actions (`--destructive-foreground`).
    destructive_foreground,
    /// Information accent (`--info`).
    info,
    /// Information text.
    info_foreground,
    /// Success accent (`--success`).
    success,
    /// Success text, also added lines.
    success_foreground,
    /// Warning accent (`--warning`).
    warning,
    /// Warning text.
    warning_foreground,
    /// Tint behind a warning banner.
    warning_surface,
    /// "Update available" accent (`--update`).
    update,
    /// Text for it.
    update_foreground,
    /// The sidebar's surface (`--sidebar`).
    sidebar,
    /// Text in the sidebar (`--sidebar-foreground`).
    sidebar_foreground,
    /// Muted text in the sidebar.
    sidebar_muted_foreground,
    /// Row metadata in the sidebar: the sidebar scope's `--secondary-label`.
    sidebar_secondary_label,
    /// Buttons and search field fill in the sidebar.
    sidebar_control_surface,
    /// A sidebar row under the pointer.
    sidebar_row_hover,
    /// The sidebar row of the open thread (`SidebarThreadRow.tsx`: `isActive`).
    sidebar_row_active,
    /// Sidebar rows picked in a multi-selection (`SidebarThreadRow.tsx`: `isSelected`).
    sidebar_row_selected,
    /// The sidebar's edge.
    sidebar_border,
    /// The sidebar scope's `--input`: borders of inputs and outline buttons in the sidebar.
    sidebar_input,
    /// The sidebar scope's `--accent`: hover fill of its icon buttons.
    sidebar_accent,
    /// Inline code and code blocks (`--code-background`).
    code_background,
    /// Text in code.
    code_foreground,
    /// Terminal canvas (`--terminal-background`).
    terminal_background,
    /// Terminal text.
    terminal_foreground,
    /// Terminal cursor.
    terminal_cursor,
    /// Terminal selection (alpha).
    terminal_selection,
    /// The idle scrollbar thumb (`--app-scrollbar-thumb`).
    scrollbar_thumb,
    /// The scrollbar thumb under the pointer.
    scrollbar_thumb_hover,
}

impl Palette {
    /// t3code's light theme (`:root` in `index.css`).
    pub const fn light() -> Self {
        // Tailwind v4 colours as sRGB (its oklch values, converted): zinc-25 #fcfcfc, zinc-50
        // #fafafa, zinc-100 #f4f4f5, zinc-200 #e4e4e7, zinc-300 #d4d4d8, zinc-500 #71717b,
        // zinc-800 #27272a, zinc-900 #18181b, red-500 #fb2c36, red-700 #c10007, blue-500
        // #2b7fff, blue-700 #1447e6, emerald-500 #00bc7d, emerald-700 #007a55, amber-500
        // #fe9a00, amber-700 #bb4d00.
        let foreground = Color::rgb(0x18181b);
        let muted_foreground = Color::rgb(0x71717b);
        let primary = Color::rgb(0x1b4ed8); // oklch(0.488 0.217 264)
        let error = Color::rgb(0xfb2c36);
        let border = Color::rgb(0xe4e4e7);
        Self {
            background: Color::rgb(0xfcfcfc),
            foreground,
            card: Color::rgb(0xffffff),
            card_foreground: foreground,
            // card at 20%.
            surface_raised: Color::rgba(0xffffff33),
            popover: Color::rgb(0xffffff),
            panel_surface: Color::rgba(0xffffffd5),
            panel_edge: Color::rgba(0x18181b19),
            popover_foreground: foreground,
            primary,
            primary_foreground: Color::rgb(0xffffff),
            secondary: Color::rgb(0xfafafa),
            secondary_foreground: Color::rgb(0x27272a),
            muted: Color::rgb(0xfafafa),
            muted_foreground,
            placeholder: muted_foreground,
            icon_muted: muted_foreground,
            secondary_label: muted_foreground,
            accent: Color::rgb(0xf4f4f5),
            accent_foreground: foreground,
            border,
            input: Color::rgb(0xd4d4d8),
            ring: primary,
            // zinc-200 at 55% over zinc-100.
            message_surface: Color::rgb(0xebebed),
            message_foreground: foreground,
            message_action: primary,
            message_action_foreground: Color::rgb(0xffffff),
            // primary 90% over the canvas (mixed before rounding to 8 bits).
            message_action_hover: Color::rgb(0x3160db),
            error,
            error_foreground: Color::rgb(0xc10007),
            // error at 8%.
            error_surface: Color::rgba(0xfb2c3614),
            destructive: error,
            destructive_foreground: Color::rgb(0xc10007),
            info: Color::rgb(0x2b7fff),
            info_foreground: Color::rgb(0x1447e6),
            success: Color::rgb(0x00bc7d),
            success_foreground: Color::rgb(0x007a55),
            warning: Color::rgb(0xfe9a00),
            warning_foreground: Color::rgb(0xbb4d00),
            // warning at 8%.
            warning_surface: Color::rgba(0xfe9a0014),
            update: primary,
            update_foreground: primary,
            sidebar: Color::rgb(0xfafafa),
            sidebar_foreground: foreground,
            sidebar_muted_foreground: muted_foreground,
            sidebar_secondary_label: muted_foreground,
            sidebar_control_surface: Color::rgb(0xf4f4f5),
            sidebar_row_hover: Color::rgb(0xfcfcfc),
            sidebar_row_active: Color::rgb(0xffffff),
            sidebar_row_selected: Color::rgb(0xffffff),
            sidebar_border: border,
            sidebar_input: Color::rgb(0xd4d4d8),
            sidebar_accent: Color::rgb(0xf4f4f5),
            // card 90% over the canvas.
            code_background: Color::rgb(0xffffff),
            code_foreground: foreground,
            terminal_background: Color::rgb(0xfcfcfc),
            terminal_foreground: foreground,
            terminal_cursor: Color::rgb(0x26384e),
            terminal_selection: Color::rgba(0x253f6333),
            scrollbar_thumb: Color::rgb(0xd9d9d9),
            scrollbar_thumb_hover: Color::rgb(0xbfbfbf),
        }
    }

    /// t3code's dark theme (`@variant dark` in `index.css`, with the sidebar scope's overrides
    /// for the `sidebar_*` tokens).
    pub const fn dark() -> Self {
        // neutral-100 #f5f5f5, neutral-400 #a1a1a1, neutral-950 #0a0a0a, red-400 #ff6467,
        // blue-400 #51a2ff, emerald-400 #00d492, amber-400 #ffb900.
        let foreground = Color::rgb(0xf5f5f5);
        let muted_foreground = Color::rgb(0xa1a1a1);
        let primary = Color::rgb(0x346bf1); // oklch(0.571 0.21 264)
        // red-500 at 90% over white.
        let error = Color::rgb(0xfb414a);
        // Sidebar scope (`[data-app-sidebar]` dark): foreground #f1f3f7 and its row tints.
        let sidebar_ink = Color::rgb(0xf1f3f7);
        let white_8 = Color::rgba(0xffffff14);
        // neutral-950 at 97% over white (the card, the popover and the composer's tone).
        let card = Color::rgb(0x111111);
        Self {
            background: Color::rgb(0x000000),
            foreground,
            card,
            card_foreground: foreground,
            surface_raised: card,
            popover: card,
            panel_surface: Color::rgba(0x111111d5),
            panel_edge: Color::rgba(0xf5f5f519),
            popover_foreground: foreground,
            primary,
            primary_foreground: Color::rgb(0xffffff),
            // white at 3%, 3%, 4%.
            secondary: Color::rgba(0xffffff08),
            secondary_foreground: foreground,
            muted: Color::rgba(0xffffff08),
            muted_foreground,
            placeholder: muted_foreground,
            icon_muted: muted_foreground,
            secondary_label: muted_foreground,
            accent: Color::rgba(0xffffff0a),
            accent_foreground: foreground,
            // white at 8% and at 11%.
            border: white_8,
            input: Color::rgba(0xffffff1c),
            ring: primary,
            // neutral-950 at 92% over white.
            message_surface: Color::rgb(0x1e1e1e),
            message_foreground: foreground,
            message_action: primary,
            message_action_foreground: Color::rgb(0xffffff),
            // primary 90% over black.
            message_action_hover: Color::rgb(0x2f60d9),
            error,
            error_foreground: Color::rgb(0xff6467),
            // error at 16%.
            error_surface: Color::rgba(0xfb414a29),
            destructive: error,
            destructive_foreground: Color::rgb(0xff6467),
            info: Color::rgb(0x2b7fff),
            info_foreground: Color::rgb(0x51a2ff),
            success: Color::rgb(0x00bc7d),
            success_foreground: Color::rgb(0x00d492),
            warning: Color::rgb(0xfe9a00),
            warning_foreground: Color::rgb(0xffb900),
            // warning at 16%.
            warning_surface: Color::rgba(0xfe9a0029),
            update: primary,
            update_foreground: Color::rgb(0x51a2ff),
            // The sidebar scope sets `--card` to #000, so the sidebar is as black as the canvas.
            sidebar: Color::rgb(0x000000),
            sidebar_foreground: sidebar_ink,
            sidebar_muted_foreground: Color::rgb(0xa3a3a3),
            sidebar_secondary_label: Color::rgb(0x8f8f8f),
            sidebar_control_surface: Color::rgb(0x0a0a0a),
            // sidebar ink at 8%, 11% and 7%.
            sidebar_row_hover: sidebar_ink.with_alpha(0x14),
            sidebar_row_active: sidebar_ink.with_alpha(0x1c),
            sidebar_row_selected: sidebar_ink.with_alpha(0x12),
            sidebar_border: white_8,
            // white at 18%.
            sidebar_input: Color::rgba(0xffffff2e),
            sidebar_accent: Color::rgb(0x191a1d),
            // card 90% over the canvas.
            code_background: Color::rgb(0x0f0f0f),
            code_foreground: foreground,
            terminal_background: Color::rgb(0x000000),
            terminal_foreground: foreground,
            terminal_cursor: Color::rgb(0xb4cbff),
            terminal_selection: Color::rgba(0xb4cbff40),
            // white at 36% and 45%.
            scrollbar_thumb: Color::rgba(0xffffff5c),
            scrollbar_thumb_hover: Color::rgba(0xffffff73),
        }
    }

    /// The default palette of an appearance.
    pub const fn default_for(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::light(),
            Appearance::Dark => Self::dark(),
        }
    }
}

/// Whether a theme is light or dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Appearance {
    /// Dark text on a light canvas.
    Light,
    /// Light text on a dark canvas.
    Dark,
}

impl Appearance {
    /// Whether it is [`Appearance::Dark`].
    pub fn is_dark(self) -> bool {
        self == Appearance::Dark
    }
}

/// How opaque a glass surface is over what lies behind it (t3code's `--glass-opacity`, used by
/// the `surface-glass` utility and the composer; the blur behind it is not reproduced).
pub const GLASS_OPACITY: f32 = 0.8;

/// The alpha of `panel_surface` (t3code's `dropdown-glass`: `--popover` at 18%, then at
/// `--glass-opacity` over what is left: 0.18 + 0.82 * 0.8 = 0.836).
pub const PANEL_ALPHA: f32 = 0.18 + (1. - 0.18) * GLASS_OPACITY;

/// The spacing unit (t3code's Tailwind `--spacing`): every gap and padding is a multiple.
pub const SPACING_UNIT: f32 = 4.;

/// `n` spacing units: `space(1.)` is 4 px, `space(0.5)` 2 px, `space(3.)` 12 px.
pub const fn space(n: f32) -> Pixels {
    px(SPACING_UNIT * n)
}

/// Corner radii. t3code's `--radius` is 0.5rem (8 px) and the others are offsets from it
/// (`@theme inline`: `sm` −4, `md` −2, `lg` +0, `xl` +4, `2xl` +8, `3xl` +12).
pub mod radius {
    use gpui_kit::{Pixels, px};

    /// `--radius`: 8 px.
    pub const BASE: f32 = 8.;
    /// Micro controls, small chips: 4 px.
    pub const SM: Pixels = px(BASE - 4.);
    /// Compact controls: 6 px.
    pub const MD: Pixels = px(BASE - 2.);
    /// Buttons, inputs, rows (`--control-radius`): 8 px.
    pub const LG: Pixels = px(BASE);
    /// Cards, the composer: 12 px.
    pub const XL: Pixels = px(BASE + 4.);
    /// Large surfaces: 16 px.
    pub const XL2: Pixels = px(BASE + 8.);
    /// Dialogs: 20 px.
    pub const XL3: Pixels = px(BASE + 12.);
}

/// Heights of controls (t3code's button size scale on desktop, `sm:` classes in `button.tsx`).
pub mod control {
    use gpui_kit::{Pixels, px};

    /// `micro` and `icon-micro`: 20 px.
    pub const MICRO: Pixels = px(20.);
    /// `xs`, `icon-xs`: 24 px.
    pub const XS: Pixels = px(24.);
    /// `sm`, `icon-sm`: 28 px.
    pub const SM: Pixels = px(28.);
    /// `default`, `icon`: 32 px.
    pub const DEFAULT: Pixels = px(32.);
    /// `lg`, `icon-lg`: 36 px.
    pub const LG: Pixels = px(36.);
}

/// A font size and the line height t3code pairs with it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSize {
    /// Font size in logical pixels.
    pub size: Pixels,
    /// Line height in logical pixels.
    pub line_height: Pixels,
}

/// Font sizes: Tailwind's scale plus t3code's `2xs` and `3xs` for dense metadata
/// (`@theme inline` in `index.css`). `body` is t3code's desktop text (`sm:text-sm`).
pub mod text {
    use super::TextSize;
    use gpui_kit::px;

    /// 10 px, line 14: counters, kbd hints.
    pub const XXS3: TextSize = TextSize {
        size: px(10.),
        line_height: px(14.),
    };
    /// 11 px, line 16: metadata rows, badges.
    pub const XXS2: TextSize = TextSize {
        size: px(11.),
        line_height: px(16.),
    };
    /// 12 px, line 16.
    pub const XS: TextSize = TextSize {
        size: px(12.),
        line_height: px(16.),
    };
    /// 14 px, line 20: the interface's body text.
    pub const SM: TextSize = TextSize {
        size: px(14.),
        line_height: px(20.),
    };
    /// 16 px, line 24.
    pub const BASE: TextSize = TextSize {
        size: px(16.),
        line_height: px(24.),
    };
    /// 18 px, line 28.
    pub const LG: TextSize = TextSize {
        size: px(18.),
        line_height: px(28.),
    };
    /// 20 px, line 28.
    pub const XL: TextSize = TextSize {
        size: px(20.),
        line_height: px(28.),
    };
    /// 24 px, line 32.
    pub const XL2: TextSize = TextSize {
        size: px(24.),
        line_height: px(32.),
    };
    /// The interface's body text.
    pub const BODY: TextSize = SM;
    /// Code and paths: 13 px, line 20.
    pub const CODE: TextSize = TextSize {
        size: px(13.),
        line_height: px(20.),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_round_trip_through_hex() {
        assert_eq!(Color::rgb(0x18181b).hex(), "#18181b");
        assert_eq!(Color::rgba(0xffffff14).hex(), "#ffffff14");
        assert_eq!(Color::parse("#282c33ff"), Some(Color::rgb(0x282c33)));
        assert_eq!(Color::parse("#fff"), Some(Color::rgb(0xffffff)));
        assert_eq!(Color::parse("#f008"), Some(Color::rgba(0xff000088)));
        assert_eq!(Color::parse("not a colour"), None);
        assert_eq!(Color::parse("#12345"), None);
        // A multi-byte character must not slice in the middle of a code point.
        assert_eq!(Color::parse("#ééé"), None);
    }

    #[test]
    fn alpha_colours_composite_over_a_surface() {
        // white at 8% over black is the #141414 t3code's dark border shows on screen.
        assert_eq!(
            Color::rgba(0xffffff14).over(Color::rgb(0x000000)),
            Color::rgb(0x141414)
        );
        assert_eq!(
            Color::rgb(0x123456).over(Color::rgb(0xffffff)),
            Color::rgb(0x123456)
        );
    }

    #[test]
    fn t3code_values_are_the_ones_in_the_css() {
        let (light, dark) = (Palette::light(), Palette::dark());
        // `:root`: --background zinc-25, --foreground zinc-900, --border zinc-200.
        assert_eq!(light.background.hex(), "#fcfcfc");
        assert_eq!(light.foreground.hex(), "#18181b");
        assert_eq!(light.border.hex(), "#e4e4e7");
        assert_eq!(light.card.hex(), "#ffffff");
        // `@variant dark`: --background #000, --border white 8%, --input white 11%.
        assert_eq!(dark.background.hex(), "#000000");
        assert_eq!(dark.border.hex(), "#ffffff14");
        assert_eq!(dark.input.hex(), "#ffffff1c");
        // --message-surface in the dark theme sits above the composer's card.
        assert_eq!(dark.card.hex(), "#111111");
        assert_eq!(dark.message_surface.hex(), "#1e1e1e");
        // The sidebar scope's own foreground and muted text.
        assert_eq!(dark.sidebar_foreground.hex(), "#f1f3f7");
        assert_eq!(dark.sidebar_muted_foreground.hex(), "#a3a3a3");
        // The destructive colour is the error colour.
        assert_eq!(light.destructive, light.error);
        assert_eq!(dark.destructive, dark.error);
        // Tailwind oklch values, converted (checked against an independent oklch -> sRGB
        // conversion at review): `--primary` oklch(0.488 0.217 264) and oklch(0.571 0.21 264).
        assert_eq!(light.primary.hex(), "#1b4ed8");
        assert_eq!(dark.primary.hex(), "#346bf1");
        assert_eq!(light.muted_foreground.hex(), "#71717b");
        assert_eq!(dark.muted_foreground.hex(), "#a1a1a1");
        // The sidebar scope's `--input` is white at 18%, not the page's 11%.
        assert_eq!(dark.sidebar_input.hex(), "#ffffff2e");
    }

    #[test]
    fn the_reference_screenshot_colours_follow_from_the_tokens() {
        let dark = Palette::dark();
        let black = dark.background;
        // The composer is `surface_raised` as glass over the canvas: #0e0e0e in
        // `app-thread-dark.png` and `app-empty-dark.png`.
        let alpha = (GLASS_OPACITY * 255.).round() as u8;
        assert_eq!(
            dark.surface_raised.with_alpha(alpha).over(black),
            Color::rgb(0x0e0e0e)
        );
        // The right panel is `dropdown-glass`: #0f0f0f in `sidebar-right.png` (our #0e0e0e plus
        // the 1-2 levels of grain), its edge #252525 (the foreground at 10% over it).
        let panel = dark.panel_surface.over(black);
        assert_eq!(panel, Color::rgb(0x0e0e0e));
        assert_eq!(dark.panel_edge.over(panel), Color::rgb(0x25_25_25));
        assert_eq!(dark.panel_surface.a, (PANEL_ALPHA * 255.).round() as u8);
        // The open thread's row (`sidebar.png`, ~#1e1e1e over the ~#030303 grain).
        let row = dark.sidebar_row_active.over(Color::rgb(0x030303));
        assert!((0x1d..=0x20).contains(&row.r), "{}", row.hex());
    }

    #[test]
    fn every_token_is_set_in_both_themes_and_the_themes_differ() {
        let (light, dark) = (Palette::light().entries(), Palette::dark().entries());
        assert_eq!(light.len(), Palette::NAMES.len());
        assert_eq!(dark.len(), Palette::NAMES.len());
        for ((l_name, _), (d_name, _)) in light.iter().zip(&dark) {
            assert_eq!(l_name, d_name);
        }
        // The names are unique, so `get` and `set` reach every token.
        let mut names = Palette::NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Palette::NAMES.len());
        // The surfaces and the text flip between light and dark.
        for name in [
            "background",
            "foreground",
            "card",
            "muted_foreground",
            "border",
            "message_surface",
            "sidebar_foreground",
        ] {
            assert_ne!(
                Palette::light().get(name),
                Palette::dark().get(name),
                "{name} is the same in both themes"
            );
        }
        assert!(Palette::light().get("no_such_token").is_none());
    }

    #[test]
    fn text_contrasts_with_its_surface() {
        // WCAG contrast of the main text pairs, on the opaque surface under them.
        fn luminance(c: Color) -> f32 {
            let lin = |v: u8| {
                let v = f32::from(v) / 255.;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
        }
        fn contrast(a: Color, b: Color) -> f32 {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }
        for palette in [Palette::light(), Palette::dark()] {
            let canvas = palette.background;
            assert!(contrast(palette.foreground, canvas) > 12.);
            assert!(contrast(palette.muted_foreground.over(canvas), canvas) > 4.5);
            assert!(contrast(palette.primary_foreground, palette.primary) > 4.5);
            assert!(
                contrast(palette.sidebar_foreground, palette.sidebar.over(canvas)) > 12.,
                "sidebar text"
            );
        }
    }

    #[test]
    fn geometry_follows_the_4px_grid() {
        assert_eq!(space(1.), px(4.));
        assert_eq!(space(0.5), px(2.));
        assert_eq!(space(3.), px(12.));
        // Control heights are t3code's button scale.
        let heights = [
            control::MICRO,
            control::XS,
            control::SM,
            control::DEFAULT,
            control::LG,
        ];
        assert_eq!(heights.map(|h| h.as_f32()), [20., 24., 28., 32., 36.]);
        // Every control height and every radius except `md` is on the 4 px grid; `md` is 6 px,
        // t3code's `--radius-md` (radius − 2).
        for h in heights {
            assert_eq!(h.as_f32() % SPACING_UNIT, 0.);
        }
        assert_eq!(radius::LG, px(8.));
        assert_eq!(radius::SM.as_f32(), 4.);
        assert_eq!(radius::MD.as_f32(), 6.);
        assert_eq!(radius::XL.as_f32(), 12.);
        // Larger radii are larger.
        assert!(radius::SM < radius::MD && radius::MD < radius::LG && radius::LG < radius::XL);
        assert_eq!(text::BODY.size, px(14.));
        for size in [text::XXS3, text::XXS2, text::XS, text::SM, text::BASE] {
            assert!(size.line_height > size.size);
        }
    }
}
