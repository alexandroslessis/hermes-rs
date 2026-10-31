//! Syntax highlighting for code blocks in chat markdown, with tree-sitter grammars and their
//! bundled highlight queries. Capture names (`keyword`, `string.special`, …) map to the active
//! theme's syntax colors through Zed's `SyntaxTheme`, so code looks the way it does in Zed.

use std::ops::Range;
use std::sync::Arc;

use markdown::language::{HighlightId, Highlighter, Language, LanguageRegistry};
use syntax_theme::SyntaxTheme;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent};

/// Capture names the highlight queries use; dotted names fall back to their prefix.
const CAPTURES: &[&str] = &[
    "attribute",
    "boolean",
    "character",
    "comment",
    "comment.doc",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "function",
    "function.builtin",
    "function.method",
    "keyword",
    "label",
    "module",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.regex",
    "string.special",
    "tag",
    "title",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

struct TreeSitterHighlighter {
    config: HighlightConfiguration,
    /// For each entry of `CAPTURES`, the theme's highlight style index (if the theme styles it).
    theme_ids: Vec<Option<HighlightId>>,
}

impl Highlighter for TreeSitterHighlighter {
    fn highlight(&self, text: &str) -> Vec<(Range<usize>, HighlightId)> {
        let mut highlighter = tree_sitter_highlight::Highlighter::new();
        let Ok(events) = highlighter.highlight(&self.config, text.as_bytes(), None, None, |_| None)
        else {
            return Vec::new();
        };
        let mut runs = Vec::new();
        let mut stack: Vec<Option<HighlightId>> = Vec::new();
        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(highlight)) => {
                    stack.push(self.theme_ids.get(highlight.0).copied().flatten());
                }
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    if let Some(Some(id)) = stack.iter().rev().find(|id| id.is_some()) {
                        runs.push((start..end, *id));
                    }
                }
                Err(_) => return Vec::new(),
            }
        }
        runs
    }
}

fn language(
    name: &str,
    grammar: tree_sitter::Language,
    highlights: &str,
    injections: &str,
    locals: &str,
    theme: &SyntaxTheme,
) -> Option<Arc<Language>> {
    let mut config =
        HighlightConfiguration::new(grammar, name, highlights, injections, locals).ok()?;
    config.configure(CAPTURES);
    let theme_ids = CAPTURES
        .iter()
        .map(|capture| theme.highlight_id(capture).map(HighlightId))
        .collect();
    let highlighter = TreeSitterHighlighter { config, theme_ids };
    Some(Arc::new(Language::new(
        name.to_string(),
        Some(Arc::new(highlighter)),
    )))
}

/// Languages for fenced code blocks, by fence name and file extension, colored with `theme`.
pub fn language_registry(theme: &SyntaxTheme) -> Arc<LanguageRegistry> {
    let mut registry = LanguageRegistry::default();
    let js_highlights = tree_sitter_javascript::HIGHLIGHT_QUERY;
    let ts_highlights = format!(
        "{}\n{}",
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        js_highlights
    );
    let cpp_highlights = format!(
        "{}\n{}",
        tree_sitter_cpp::HIGHLIGHT_QUERY,
        tree_sitter_c::HIGHLIGHT_QUERY
    );
    let entries: Vec<(&[&str], Option<Arc<Language>>)> = vec![
        (
            &["rust", "rs"],
            language(
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
                theme,
            ),
        ),
        (
            &["python", "py"],
            language(
                "python",
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["javascript", "js", "jsx", "mjs", "cjs"],
            language(
                "javascript",
                tree_sitter_javascript::LANGUAGE.into(),
                &format!(
                    "{}\n{}",
                    js_highlights,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                ),
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
                theme,
            ),
        ),
        (
            &["typescript", "ts", "mts", "cts"],
            language(
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                &ts_highlights,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
                theme,
            ),
        ),
        (
            &["tsx"],
            language(
                "tsx",
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                &format!(
                    "{}\n{}",
                    ts_highlights,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                ),
                "",
                tree_sitter_typescript::LOCALS_QUERY,
                theme,
            ),
        ),
        (
            &["json", "jsonc"],
            language(
                "json",
                tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["bash", "sh", "shell", "zsh", "console"],
            language(
                "bash",
                tree_sitter_bash::LANGUAGE.into(),
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["c", "h"],
            language(
                "c",
                tree_sitter_c::LANGUAGE.into(),
                tree_sitter_c::HIGHLIGHT_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["cpp", "c++", "cc", "cxx", "hpp"],
            language(
                "cpp",
                tree_sitter_cpp::LANGUAGE.into(),
                &cpp_highlights,
                "",
                "",
                theme,
            ),
        ),
        (
            &["csharp", "c#", "cs"],
            language(
                "csharp",
                tree_sitter_c_sharp::LANGUAGE.into(),
                tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["go", "golang"],
            language(
                "go",
                tree_sitter_go::LANGUAGE.into(),
                tree_sitter_go::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["java"],
            language(
                "java",
                tree_sitter_java::LANGUAGE.into(),
                tree_sitter_java::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["html", "htm", "xml", "svg"],
            language(
                "html",
                tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY,
                tree_sitter_html::INJECTIONS_QUERY,
                "",
                theme,
            ),
        ),
        (
            &["css"],
            language(
                "css",
                tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["toml"],
            language(
                "toml",
                tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
        (
            &["yaml", "yml"],
            language(
                "yaml",
                tree_sitter_yaml::LANGUAGE.into(),
                tree_sitter_yaml::HIGHLIGHTS_QUERY,
                "",
                "",
                theme,
            ),
        ),
    ];
    for (names, language) in entries {
        if let Some(language) = language {
            registry.register(names, language);
        }
    }
    Arc::new(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> SyntaxTheme {
        let style = gpui::HighlightStyle::default();
        SyntaxTheme::new(CAPTURES.iter().map(|name| (name.to_string(), style)))
    }

    #[test]
    fn every_language_compiles_its_queries() {
        let theme = theme();
        let registry = language_registry(&theme);
        for name in [
            "rust", "py", "js", "ts", "tsx", "json", "sh", "c", "cpp", "cs", "go", "java", "html",
            "css", "toml", "yaml",
        ] {
            let language =
                futures::executor::block_on(registry.language_for_name_or_extension(name));
            assert!(language.is_ok(), "{name} failed to load");
        }
    }

    #[test]
    fn rust_code_gets_keyword_and_string_runs() {
        let theme = theme();
        let registry = language_registry(&theme);
        let rust =
            futures::executor::block_on(registry.language_for_name_or_extension("rust")).unwrap();
        let text = "fn main() { let s = \"hi\"; }";
        let runs = rust.highlight_text_resolved(text, 0..text.len()).runs;
        let keyword = theme.highlight_id("keyword").map(HighlightId).unwrap();
        let string = theme.highlight_id("string").map(HighlightId).unwrap();
        assert!(
            runs.iter()
                .any(|(range, id)| &text[range.clone()] == "fn" && *id == keyword)
        );
        assert!(
            runs.iter()
                .any(|(range, id)| &text[range.clone()] == "\"hi\"" && *id == string)
        );
    }
}
