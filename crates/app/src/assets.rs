use std::borrow::Cow;

use anyhow::Context as _;
use gpui::{App, AssetSource, Result, SharedString};
use rust_embed::RustEmbed;

/// Fonts, icons and themes from `assets/`, embedded into the executable at compile time.
#[derive(RustEmbed)]
#[folder = "../../assets"]
#[include = "fonts/**/*"]
#[include = "icons/**/*"]
#[include = "themes/**/*"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Self::get(path)
            .map(|file| Some(file.data))
            .with_context(|| format!("loading asset at path {path:?}"))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter(|asset| asset.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

impl Assets {
    /// Registers every bundled `.ttf` font with GPUI's text system.
    pub fn load_fonts(&self, cx: &App) -> Result<()> {
        let fonts = Self::iter()
            .filter(|path| path.starts_with("fonts/") && path.ends_with(".ttf"))
            .filter_map(|path| Self::get(&path).map(|file| file.data))
            .collect();
        cx.text_system().add_fonts(fonts)
    }
}
