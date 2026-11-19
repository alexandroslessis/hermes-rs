//! A project's colour and monogram: the tint and the two letters of its tile in the sidebar's
//! folder headers and wherever the project is named. t3code lets the user pick one of 18 Tailwind
//! colours per project (`ProjectIconColor`); we offer the same 18, the tile being the hue at 14%
//! (`color-mix(currentColor 14%)`) with the hue itself as the letters. A project nobody coloured
//! gets t3code's colour for its name (`projectColor`: a base-31 hash of the lower-cased name over
//! the 18), and t3code's letters (`projectMonogram`), so the same project looks the same in both
//! apps. (t3code NFKC-normalises the name first; we do not: it only matters for compatibility
//! characters such as full-width letters.)
//!
//! The choice is presentation, not data: it lives in `native-ui.json` (`sidebar_list.colors`,
//! project id -> name), not in the store (frozen schema, ground rule 6) and not in a second side
//! file.
//!
//! t3code: `apps/web/src/projectIdentity.ts` (`projectMonogram`, `projectColor`),
//! `projectIconColors.ts` (the 18 colours, 400 in dark and 600 in light),
//! `components/ProjectMonogram.tsx` (the tile). The hex values are Tailwind v3's table; v4's
//! oklch values differ by a few units. Reference: `button-session-variants-selected.png` and
//! `app-sidebar_closeup-dark.png` (violet "SE" structure, green "EE" engine, indigo "HR" hermes-rs).

use crate::design::{Color, tokens::Appearance};

/// One of t3code's 18 project colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectColor {
    Gray,
    Red,
    Orange,
    Amber,
    Yellow,
    Lime,
    Green,
    Emerald,
    Teal,
    Cyan,
    Sky,
    Blue,
    Indigo,
    Violet,
    Purple,
    Fuchsia,
    Pink,
    Rose,
}

type Row = (ProjectColor, &'static str, &'static str, u32, u32);

/// (colour, stored name, label, dark ink, light ink), in t3code's order (the hash indexes it).
const TABLE: [Row; 18] = [
    (ProjectColor::Gray, "gray", "Gray", 0x9ca3af, 0x4b5563),
    (ProjectColor::Red, "red", "Red", 0xf87171, 0xdc2626),
    (ProjectColor::Orange, "orange", "Orange", 0xfb923c, 0xea580c),
    (ProjectColor::Amber, "amber", "Amber", 0xfbbf24, 0xd97706),
    (ProjectColor::Yellow, "yellow", "Yellow", 0xfacc15, 0xca8a04),
    (ProjectColor::Lime, "lime", "Lime", 0xa3e635, 0x65a30d),
    (ProjectColor::Green, "green", "Green", 0x4ade80, 0x16a34a),
    (
        ProjectColor::Emerald,
        "emerald",
        "Emerald",
        0x34d399,
        0x059669,
    ),
    (ProjectColor::Teal, "teal", "Teal", 0x2dd4bf, 0x0d9488),
    (ProjectColor::Cyan, "cyan", "Cyan", 0x22d3ee, 0x0891b2),
    (ProjectColor::Sky, "sky", "Sky", 0x38bdf8, 0x0284c7),
    (ProjectColor::Blue, "blue", "Blue", 0x60a5fa, 0x2563eb),
    (ProjectColor::Indigo, "indigo", "Indigo", 0x818cf8, 0x4f46e5),
    (ProjectColor::Violet, "violet", "Violet", 0xa78bfa, 0x7c3aed),
    (ProjectColor::Purple, "purple", "Purple", 0xc084fc, 0x9333ea),
    (
        ProjectColor::Fuchsia,
        "fuchsia",
        "Fuchsia",
        0xe879f9,
        0xc026d3,
    ),
    (ProjectColor::Pink, "pink", "Pink", 0xf472b6, 0xdb2777),
    (ProjectColor::Rose, "rose", "Rose", 0xfb7185, 0xe11d48),
];

impl ProjectColor {
    /// Every colour, in the picker's (t3code's) order.
    pub const ALL: [ProjectColor; 18] = {
        let mut all = [ProjectColor::Gray; 18];
        let mut ix = 0;
        while ix < 18 {
            all[ix] = TABLE[ix].0;
            ix += 1;
        }
        all
    };

    fn row(self) -> &'static Row {
        // The enum's order is the table's (checked by a test).
        &TABLE[self as usize]
    }

    /// The stored name (t3code's `ProjectIconColor` value).
    pub fn name(self) -> &'static str {
        self.row().1
    }

    /// The label in the picker.
    pub fn label(self) -> &'static str {
        self.row().2
    }

    pub fn from_name(name: &str) -> Option<Self> {
        TABLE.iter().find(|row| row.1 == name).map(|row| row.0)
    }

    /// The colour a project gets when nobody picked one: t3code's `projectColor`.
    pub fn for_name(name: &str) -> Self {
        let seed = name.trim().to_lowercase();
        let seed = if seed.is_empty() {
            "project".to_string()
        } else {
            seed
        };
        let index = seed
            .chars()
            .fold(0u64, |ix, c| (ix * 31 + u64::from(u32::from(c))) % 18);
        TABLE[index as usize].0
    }

    /// The hue itself: the letters of the tile, and a swatch.
    pub fn ink(self, appearance: Appearance) -> Color {
        let row = self.row();
        Color::rgb(if appearance.is_dark() { row.3 } else { row.4 })
    }

    /// The tile behind the letters: the hue at 14%.
    pub fn tile(self, appearance: Appearance) -> Color {
        self.ink(appearance).with_alpha(0x24)
    }
}

/// A project's two letters (t3code's `projectMonogram`): the first letter of its first word, then
/// the first digit after it, else the first letter of its last word when it has several, else the
/// last letter of its first word. "engine" is "EE", "hermes-rs" "HR", "v2" "V2"; no word at all is
/// "PR".
pub fn monogram(name: &str) -> String {
    let words: Vec<&str> = name
        .trim()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let Some(first_word) = words.first() else {
        return "PR".to_string();
    };
    let glyphs: Vec<char> = first_word.chars().collect();
    let first = glyphs[0];
    let second = glyphs[1..]
        .iter()
        .copied()
        .find(|c| c.is_numeric())
        .or_else(|| {
            if words.len() > 1 {
                words.last().and_then(|w| w.chars().next())
            } else {
                glyphs.last().copied()
            }
        })
        .unwrap_or(first);
    format!("{first}{second}")
        .to_uppercase()
        .chars()
        .take(2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn names_round_trip_and_hues_differ() {
        for (ix, color) in ProjectColor::ALL.into_iter().enumerate() {
            assert_eq!(color as usize, ix, "the enum is in the table's order");
            assert_eq!(ProjectColor::from_name(color.name()), Some(color));
        }
        assert_eq!(ProjectColor::from_name("chartreuse"), None);
        for appearance in [Appearance::Light, Appearance::Dark] {
            let inks: std::collections::HashSet<_> = ProjectColor::ALL
                .iter()
                .map(|c| c.ink(appearance))
                .collect();
            assert_eq!(inks.len(), ProjectColor::ALL.len());
        }
    }

    #[core::prelude::v1::test]
    fn a_name_gets_t3codes_colour_and_all_eighteen_are_used() {
        // As t3code computes them (node, `projectColor`): the screenshots show these tints.
        assert_eq!(ProjectColor::for_name("structure"), ProjectColor::Violet);
        assert_eq!(ProjectColor::for_name("engine"), ProjectColor::Green);
        assert_eq!(ProjectColor::for_name("hermes-rs"), ProjectColor::Indigo);
        assert_eq!(
            ProjectColor::for_name(" Engine "),
            ProjectColor::for_name("engine")
        );
        let used: std::collections::HashSet<_> = (0..400)
            .map(|i| ProjectColor::for_name(&format!("project {i}")))
            .collect();
        assert_eq!(used.len(), 18);
    }

    #[core::prelude::v1::test]
    fn monograms_are_t3codes() {
        assert_eq!(monogram("structure"), "SE");
        assert_eq!(monogram("engine"), "EE");
        assert_eq!(monogram("hermes-rs"), "HR");
        assert_eq!(monogram("v2 tools"), "V2");
        assert_eq!(monogram("x"), "XX");
        assert_eq!(monogram("--"), "PR");
        assert_eq!(monogram("élan vital"), "ÉV");
    }
}
