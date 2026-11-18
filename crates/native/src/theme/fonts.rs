//! The interface and code fonts, and the decision behind them (CP2.1 step 4, 2026-11-18).
//!
//! **Interface: Inter, bundled** (`assets/fonts/inter/`, SIL OFL 1.1, static Regular, Bold,
//! Italic and Bold Italic). t3code draws its interface in Inter (`@fontsource-variable/inter`,
//! first in `--font-sans`) and the reference screenshots are Inter, so the text widths, the
//! look of numerals and the sidebar's truncation match only with Inter. Rendered side by side at
//! 14 px (Inter, Segoe UI Variable Text, Segoe UI, IBM Plex Sans), Segoe is narrower and draws
//! weight 500 as SemiBold; Inter's static files draw 500 as Regular and 600 as Bold, which keeps
//! the row titles close to t3code's. Cost: 1.4 MB in the binary. If Inter cannot be loaded the
//! stack falls back to Segoe UI Variable Text, then Segoe UI (always on Windows).
//!
//! **Code: Consolas, a Windows system font, not bundled.** t3code's `--font-mono` stack ends in
//! Consolas, which is what the screenshots show on Windows, and it ships with every Windows. Then
//! Cascadia Mono (Windows Terminal, Windows 11), then Lucida Console.
//!
//! Not done: Inter Variable would give real 500 and 600 weights; GPUI's DirectWrite path with a
//! variable font's weight axis was not tried (it needs the woff2 converted to TrueType).
//!
//! t3code: `apps/web/src/index.css` (`@theme` `--font-sans`, `--font-mono`),
//! `apps/web/src/appearanceFonts.ts` (the default stacks), `apps/web/src/main.tsx` (Inter import).

use std::borrow::Cow;

use gpui_kit::{App, Global};

/// The font families the app draws with, already checked to be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fonts {
    /// The interface font.
    pub ui: String,
    /// The code, path and terminal font.
    pub mono: String,
}

/// Interface font candidates, best first.
pub const UI_STACK: &[&str] = &["Inter", "Segoe UI Variable Text", "Segoe UI"];

/// Code font candidates, best first.
pub const MONO_STACK: &[&str] = &["Consolas", "Cascadia Mono", "Lucida Console"];

/// GPUI's own platform UI font, used when no candidate is installed.
pub const SYSTEM_UI: &str = ".SystemUIFont";

/// The bundled font files (Inter, unmodified; licence in `assets/fonts/inter/OFL.txt`).
const BUNDLED: &[&[u8]] = &[
    include_bytes!("../../assets/fonts/inter/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/inter/Inter-Bold.ttf"),
    include_bytes!("../../assets/fonts/inter/Inter-Italic.ttf"),
    include_bytes!("../../assets/fonts/inter/Inter-BoldItalic.ttf"),
];

/// The bundled fonts were added to this app's text system, and the stacks resolved then (listing
/// every system font is not free, and the gallery's token story asks on each render).
struct Installed(Fonts);

impl Global for Installed {}

impl Fonts {
    /// The first installed candidate of each stack; GPUI's own system UI font and Consolas when
    /// none is installed (naming a font that is missing would fail at draw time, so the stacks
    /// are checked here).
    pub fn resolve(installed: &[String]) -> Self {
        let pick = |stack: &[&str], fallback: &str| {
            stack
                .iter()
                .find(|name| installed.iter().any(|have| have == *name))
                .map_or_else(|| fallback.to_string(), |name| (*name).to_string())
        };
        Self {
            ui: pick(UI_STACK, SYSTEM_UI),
            mono: pick(MONO_STACK, "Consolas"),
        }
    }

    /// Adds the bundled fonts to the text system and resolves the stacks against what is then
    /// installed, once per app; later calls return the same answer. A text system that refuses
    /// them (a failed load) leaves the system fonts in charge.
    pub fn install(cx: &mut App) -> Self {
        if let Some(Installed(fonts)) = cx.try_global::<Installed>() {
            return fonts.clone();
        }
        let files: Vec<Cow<'static, [u8]>> = BUNDLED.iter().map(|f| Cow::Borrowed(*f)).collect();
        if let Err(error) = cx.text_system().add_fonts(files) {
            tracing::warn!("the bundled fonts could not be loaded: {error:#}");
        }
        let fonts = Self::resolve(&cx.text_system().all_font_names());
        cx.set_global(Installed(fonts.clone()));
        fonts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn the_first_installed_candidate_wins() {
        let fonts = Fonts::resolve(&installed(&[
            "Segoe UI",
            "Inter",
            "Consolas",
            "Cascadia Mono",
        ]));
        assert_eq!(
            (fonts.ui.as_str(), fonts.mono.as_str()),
            ("Inter", "Consolas")
        );
        let fonts = Fonts::resolve(&installed(&["Segoe UI", "Cascadia Mono"]));
        assert_eq!(
            (fonts.ui.as_str(), fonts.mono.as_str()),
            ("Segoe UI", "Cascadia Mono")
        );
    }

    #[test]
    fn nothing_installed_falls_back_to_the_system_fonts() {
        let fonts = Fonts::resolve(&[]);
        assert_eq!(fonts.ui, SYSTEM_UI);
        assert_eq!(fonts.mono, "Consolas");
    }

    #[test]
    fn the_bundled_files_are_truetype_fonts() {
        for file in BUNDLED {
            // TrueType scaler tag (0x00010000) at the start of the file.
            assert_eq!(&file[..4], &[0, 1, 0, 0]);
        }
        assert_eq!(BUNDLED.len(), 4);
    }
}
