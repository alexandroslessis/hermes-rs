//! hermes-rs: a small stand-in for the parts of Zed's `language` crate the markdown renderer
//! uses. Zed's crate brings tree-sitter, LSP, buffers and its whole project layer; markdown only
//! needs a language handle that can highlight code blocks, plus a few text helpers.
//!
//! Without a registry (the default), code blocks render unhighlighted. A highlighter plugs in by
//! implementing [`Highlighter`] and registering languages in a [`LanguageRegistry`].

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use collections::HashMap;
use gpui::SharedString;
use smallvec::SmallVec;

pub type LanguageName = SharedString;

/// An index into the active [`syntax_theme::SyntaxTheme`]'s highlight styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HighlightId(pub u32);

impl From<HighlightId> for usize {
    fn from(id: HighlightId) -> usize {
        id.0 as usize
    }
}

/// Highlight runs for one piece of text, in byte ranges relative to it.
#[derive(Clone, Debug, Default)]
pub struct ResolvedHighlights {
    pub sources: SmallVec<[(); 2]>,
    pub runs: Arc<[(Range<usize>, HighlightId)]>,
}

impl ResolvedHighlights {
    /// Highlights carry no grammar version here, so they never go stale.
    pub fn is_current(&self) -> bool {
        true
    }
}

pub trait Highlighter: Send + Sync {
    fn highlight(&self, text: &str) -> Vec<(Range<usize>, HighlightId)>;
}

pub struct Language {
    name: LanguageName,
    highlighter: Option<Arc<dyn Highlighter>>,
}

impl Language {
    pub fn new(name: impl Into<LanguageName>, highlighter: Option<Arc<dyn Highlighter>>) -> Self {
        Self {
            name: name.into(),
            highlighter,
        }
    }

    pub fn name(&self) -> LanguageName {
        self.name.clone()
    }

    pub fn highlight_text_resolved(&self, text: &str, range: Range<usize>) -> ResolvedHighlights {
        let runs = match &self.highlighter {
            Some(highlighter) => highlighter.highlight(&text[range]),
            None => Vec::new(),
        };
        ResolvedHighlights {
            sources: SmallVec::new(),
            runs: runs.into(),
        }
    }

    pub fn default_scope(&self) {}
}

/// Languages by name (`rust`) and file extension (`rs`).
#[derive(Default)]
pub struct LanguageRegistry {
    by_name: HashMap<String, Arc<Language>>,
}

impl LanguageRegistry {
    pub fn register(&mut self, names: &[&str], language: Arc<Language>) {
        for name in names {
            self.by_name.insert(name.to_lowercase(), language.clone());
        }
    }

    pub async fn language_for_name_or_extension(&self, name: &str) -> Result<Arc<Language>> {
        self.by_name
            .get(&name.to_lowercase())
            .cloned()
            .ok_or_else(|| anyhow!("no language {name}"))
    }

    pub async fn language_for_name(&self, name: &str) -> Result<Arc<Language>> {
        self.language_for_name_or_extension(name).await
    }

    pub async fn load_language_for_file_path(&self, path: &Path) -> Result<Arc<Language>> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .ok_or_else(|| anyhow!("no extension"))?;
        self.language_for_name_or_extension(extension).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OffsetUtf16(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Left,
    Right,
}

/// Just enough of Zed's rope for UTF-16 offset clipping on short rendered lines.
pub struct Rope(String);

impl From<&str> for Rope {
    fn from(text: &str) -> Self {
        Self(text.to_string())
    }
}

impl From<String> for Rope {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&String> for Rope {
    fn from(text: &String) -> Self {
        Self(text.clone())
    }
}

impl Rope {
    /// Rounds a UTF-16 offset to a character boundary and returns it as a UTF-8 byte offset
    /// wrapped in `OffsetUtf16` units (callers convert with `.0`).
    pub fn clip_offset_utf16(&self, offset: OffsetUtf16, bias: Bias) -> usize {
        let mut utf16 = 0;
        for (byte, ch) in self.0.char_indices() {
            let next = utf16 + ch.len_utf16();
            if offset.0 < next {
                return match bias {
                    Bias::Left => byte,
                    Bias::Right if offset.0 == utf16 => byte,
                    Bias::Right => byte + ch.len_utf8(),
                };
            }
            utf16 = next;
        }
        self.0.len()
    }
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
pub enum CharKind {
    Whitespace,
    Punctuation,
    Word,
}

pub struct CharClassifier;

impl CharClassifier {
    pub fn new(_scope: Option<()>) -> Self {
        Self
    }

    pub fn kind(&self, c: char) -> CharKind {
        if c.is_alphanumeric() || c == '_' {
            CharKind::Word
        } else if c.is_whitespace() {
            CharKind::Whitespace
        } else {
            CharKind::Punctuation
        }
    }
}
