//! Messages as markdown: GPUI Kit's `TextView` with our extensions, set up the same way for every
//! message (history, streaming, reasoning).
//!
//! - **Code blocks** get a header row: the language and a copy button in the block's top right
//!   corner (Kit's `code_block_actions`), the code moved down to leave the row clear, t3code's
//!   colours (`bg-secondary`, `border-border/70`; dark: `bg-input/32`, no border). t3code puts a
//!   language icon at the left and the wrap and copy buttons at the right; Kit anchors the actions
//!   at the right only, so the language label sits beside the copy button.
//! - **GitHub alerts** (`> [!NOTE]`, `TIP`, `IMPORTANT`, `WARNING`, `CAUTION`) are Kit block
//!   surfaces: a coloured left border and an icon with the label, the blocks inside stay the
//!   document's (selection and copy go through them).
//! - **Tables** scroll sideways instead of squeezing their columns (Kit's `overflow.x` scroll).
//! - **Links to files** (`src/main.rs#L12`, `C:\x\y.rs:3`, `file://`) dispatch
//!   [`super::OpenFileInPanel`]; the right panel will open them (Phase 6; until then the
//!   workspace says where it would go). Web links open in the browser, as Kit does.
//!
//! Parsing is Kit's: a streamed message's text is pushed onto its `TextViewState`, which parses
//! only its last block again with the new text (`parse_content`'s append path); the blocks before
//! it keep their parsed nodes and their measured heights. [`probe`] counts parses in tests.
//!
//! t3code: `apps/web/src/components/ChatMarkdown.tsx` (`GITHUB_ALERT_PRESENTATIONS`, the
//! blockquote alert, links), `chat/MarkdownCodeBlock.tsx` (the code block's frame and header,
//! the copy button), `chat/MarkdownFileLink.tsx` (file links). Reference:
//! `app-codeblock_closeup-dark.png`.

use gpui_kit::{
    AnyElement, App, ClickEvent, ClipboardItem, FontWeight, InteractiveElement as _, IntoElement,
    Overflow, ParentElement as _, SharedString, StyleRefinement, Styled as _, Window,
    assets::IconName,
    base::text::CodeBlock,
    component::ActiveTheme as _,
    component::Sizable as _,
    component::Size as KitSize,
    component::text::{MarkdownExtensions, TextView, TextViewStyle},
    div, px,
};

use super::OpenFileInPanel;
use super::data::file_target;
use crate::design::{
    Color,
    button::{self, Variant, icon_button},
    icons::icon,
    tokens::{radius, text},
};
use crate::dev::ProbeExt as _;
use crate::theme::DesignTheme as _;

/// The height of a code block's header row, which the code starts below.
pub const CODE_HEADER: f32 = 36.;

/// The kinds of GitHub alert t3code draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl Alert {
    pub const ALL: [Alert; 5] = [
        Alert::Note,
        Alert::Tip,
        Alert::Important,
        Alert::Warning,
        Alert::Caution,
    ];

    /// The marker's name (`[!note]`).
    pub fn name(self) -> &'static str {
        match self {
            Alert::Note => "note",
            Alert::Tip => "tip",
            Alert::Important => "important",
            Alert::Warning => "warning",
            Alert::Caution => "caution",
        }
    }

    /// The title it shows.
    pub fn label(self) -> &'static str {
        match self {
            Alert::Note => "Note",
            Alert::Tip => "Tip",
            Alert::Important => "Important",
            Alert::Warning => "Warning",
            Alert::Caution => "Caution",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Alert::Note => IconName::Info,
            Alert::Tip => IconName::Lightbulb,
            Alert::Important => IconName::MessageSquareWarning,
            Alert::Warning => IconName::TriangleAlert,
            Alert::Caution => IconName::OctagonAlert,
        }
    }

    /// The border (the 500 shade at 70%) and the title colour (600 light, 400 dark; amber 500
    /// dark), Tailwind's palette as t3code names it.
    pub fn colors(self, dark: bool) -> (Color, Color) {
        let (c400, c500, c600) = match self {
            Alert::Note => (0x60a5fa, 0x3b82f6, 0x2563eb),
            Alert::Tip => (0x34d399, 0x10b981, 0x059669),
            Alert::Important => (0xc084fc, 0xa855f7, 0x9333ea),
            Alert::Warning => (0xf59e0b, 0xf59e0b, 0xd97706),
            Alert::Caution => (0xf87171, 0xef4444, 0xdc2626),
        };
        let title = if dark { c400 } else { c600 };
        (Color::rgb(c500).with_alpha(179), Color::rgb(title))
    }
}

/// The extensions every message parses with: the alert surfaces, and in tests the parse counter
/// (registered in test builds only). Built once per thread (a fixed parser revision: rebuilding
/// it each frame does not reparse).
fn extensions() -> MarkdownExtensions {
    let mut extensions = MarkdownExtensions::default().parser_revision(1);
    #[cfg(test)]
    {
        extensions = extensions.block_parser(|node, cx| {
            probe::record(node, cx);
            None
        });
    }
    for alert in Alert::ALL {
        extensions = extensions.block_surface(alert.name(), move |_, cx| {
            let dark = cx.design_theme().appearance.is_dark();
            let (border, title) = alert.colors(dark);
            div()
                .flex()
                .flex_col()
                .my(px(4.))
                .pl(px(12.))
                .border_l_2()
                .border_color(border.hsla())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(title.hsla())
                        .child(
                            icon(alert.icon())
                                .with_size(KitSize::Size(px(14.)))
                                .text_color(title.hsla()),
                        )
                        .child(alert.label()),
                )
        });
    }
    extensions
}

thread_local! {
    static EXTENSIONS: MarkdownExtensions = extensions();
}

/// The text style: code blocks with room for their header, tables that scroll sideways.
fn style(cx: &App) -> TextViewStyle {
    let theme = cx.design_theme();
    let (palette, dark) = (theme.palette, theme.appearance.is_dark());
    let mut code = StyleRefinement::default()
        .rounded(radius::LG)
        .pt(px(CODE_HEADER))
        .text_color(palette.foreground.hsla());
    code = if dark {
        // `dark:bg-input/32 dark:border-transparent`
        code.bg(palette
            .input
            .with_alpha((f32::from(palette.input.a) * 0.32).round() as u8)
            .over(palette.background)
            .hsla())
            .border_color(gpui_kit::transparent_black())
    } else {
        // `bg-secondary border-border/70`
        code.bg(palette.secondary.hsla())
            .border_color(palette.border.with_alpha(179).hsla())
    };
    let mut table = StyleRefinement::default().rounded(radius::LG);
    table.overflow.x = Some(Overflow::Scroll);
    TextViewStyle::default().code_block(code).table(table)
}

/// `view` set up as a message: the style, the extensions, the code header, the links.
pub fn message(view: TextView, cx: &App) -> TextView {
    let extensions = EXTENSIONS.with(Clone::clone);
    view.style(style(cx))
        .markdown_extensions(extensions)
        .code_block_actions(code_header)
        .on_link_click(link_clicked)
        .selectable(true)
}

/// `view` set up as a rendered markdown *file* of the Files tab (`folder`: the file's own folder,
/// relative to the project, `""` at the top): the same style, extensions and link rules as a
/// message, except that a relative link names a file next to this one ([`preview_link`]).
pub fn preview(view: TextView, cx: &App, folder: String) -> TextView {
    let extensions = EXTENSIONS.with(Clone::clone);
    view.style(style(cx))
        .markdown_extensions(extensions)
        .code_block_actions(code_header)
        .on_link_click(move |url, _, window, cx| match preview_link(url, &folder) {
            LinkAction::OpenFile(action) => window.dispatch_action(Box::new(action), cx),
            LinkAction::OpenUrl(url) => cx.open_url(&url),
            LinkAction::Ignore => {}
        })
        .selectable(true)
}

/// What a click on `url` in a markdown file of `folder` does: [`link_action`], with a relative
/// file link read from `folder` (`../README.md` in `docs/guide` is `docs/README.md`). A link that
/// climbs out of the project keeps its `..`, which the backend refuses.
pub fn preview_link(url: &str, folder: &str) -> LinkAction {
    match link_action(url) {
        LinkAction::OpenFile(mut file) => {
            let path = file.path.replace('\\', "/");
            let absolute = path.starts_with('/')
                || path.starts_with('~')
                || path.chars().nth(1) == Some(':')
                || folder.is_empty();
            if !absolute {
                file.path = join_folder(folder, &path);
            }
            LinkAction::OpenFile(file)
        }
        other => other,
    }
}

/// `folder/path` with `.` and `..` folded; `..` past the top stays.
fn join_folder(folder: &str, path: &str) -> String {
    let mut parts: Vec<&str> = folder.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// A code block's header: the language and the copy button.
fn code_header(block: &CodeBlock, _: &mut Window, cx: &mut App) -> AnyElement {
    let palette = cx.design_theme().palette;
    let code = block.code();
    div()
        .id("code-header")
        .flex()
        .items_center()
        .gap(px(6.))
        .children(block.lang().filter(|l| !l.is_empty()).map(|lang| {
            // `font-mono text-2xs`
            div()
                .id("code-lang")
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(text::XXS2.size)
                .line_height(text::XXS2.line_height)
                .text_color(palette.muted_foreground.hsla())
                .child(lang)
                .probe()
        }))
        .child(
            icon_button("copy-code", IconName::Copy, "Copy code")
                .variant(Variant::GhostMuted)
                .size(button::Size::Xs)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                }),
        )
        .probe()
        .into_any_element()
}

/// What a click on a link does.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkAction {
    /// A file: the right panel opens it.
    OpenFile(OpenFileInPanel),
    /// A web or mail link: the browser (or the mail app).
    OpenUrl(String),
    /// Anything else (`javascript:`, `file:C:/x`, `ms-settings:`, `search-ms:`, an anchor):
    /// nothing. A message is the model's text: a link in it must not hand an arbitrary URL to
    /// the shell, which would run whatever program the scheme or the file names.
    Ignore,
}

/// What a click on `url` does.
pub fn link_action(url: &str) -> LinkAction {
    if let Some(target) = file_target(url) {
        return LinkAction::OpenFile(OpenFileInPanel {
            path: target.path,
            line: target.line,
        });
    }
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
    {
        LinkAction::OpenUrl(url.to_string())
    } else if lower.starts_with("www.") {
        LinkAction::OpenUrl(format!("https://{url}"))
    } else {
        LinkAction::Ignore
    }
}

fn link_clicked(url: &SharedString, _: &ClickEvent, window: &mut Window, cx: &mut App) {
    match link_action(url) {
        LinkAction::OpenFile(action) => window.dispatch_action(Box::new(action), cx),
        LinkAction::OpenUrl(url) => cx.open_url(&url),
        LinkAction::Ignore => {}
    }
}

/// The parse counter of the tests: which top-level paragraphs a parse visited, by their offset
/// in the whole message, for paragraphs that carry the probe's mark.
#[cfg(test)]
pub mod probe {
    use std::sync::Mutex;

    use gpui_kit::component::text::{MarkdownParseContext, markdown_ast::Node};

    /// Paragraphs with this text in them are counted.
    pub const MARK: &str = "PARSEPROBE";

    static LOG: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    pub(super) fn record(node: &Node, cx: &MarkdownParseContext<'_>) {
        if !matches!(node, Node::Paragraph(_)) {
            return;
        }
        let (Some(source), Some(position)) = (cx.node_source(node), node.position()) else {
            return;
        };
        if source.contains(MARK) {
            LOG.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(cx.offset() + position.start.offset);
        }
    }

    /// How often each offset was parsed since the last call, and forgets it.
    pub fn take() -> std::collections::BTreeMap<usize, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for offset in std::mem::take(&mut *LOG.lock().unwrap_or_else(|e| e.into_inner())) {
            *counts.entry(offset).or_default() += 1;
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_links_open_the_panel_and_web_links_the_browser() {
        assert_eq!(
            link_action("src/stream.rs#L42"),
            LinkAction::OpenFile(OpenFileInPanel {
                path: "src/stream.rs".into(),
                line: Some(42)
            })
        );
        assert_eq!(
            link_action("https://example.com/plan"),
            LinkAction::OpenUrl("https://example.com/plan".into())
        );
        assert_eq!(
            link_action("www.example.com"),
            LinkAction::OpenUrl("https://www.example.com".into())
        );
        assert_eq!(
            link_action("mailto:me@example.com"),
            LinkAction::OpenUrl("mailto:me@example.com".into())
        );
        // Schemes the shell would act on open nothing.
        for unsafe_link in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "file:C:/Windows/System32/calc.exe",
            "ms-settings:privacy",
            "search-ms:query=x",
            "ms-msdt:/id PCWDiagnostic",
            "vbscript:msgbox",
            "#section",
            "",
        ] {
            assert_eq!(
                link_action(unsafe_link),
                LinkAction::Ignore,
                "{unsafe_link}"
            );
        }
    }

    #[test]
    fn links_in_a_markdown_file_are_read_from_its_own_folder_by_the_same_safe_rules() {
        let open = |path: &str, line: Option<u32>| {
            LinkAction::OpenFile(OpenFileInPanel {
                path: path.into(),
                line,
            })
        };
        assert_eq!(
            preview_link("../../README.md", "docs/guide"),
            open("README.md", None)
        );
        assert_eq!(preview_link("intro.md#L4", "docs/guide"), open("docs/guide/intro.md", Some(4)));
        assert_eq!(preview_link("./a/../b.md", "docs"), open("docs/b.md", None));
        // Up and out of the project: kept, so the backend says it is outside.
        assert_eq!(preview_link("../../x.md", "docs"), open("../x.md", None));
        // At the top of the project nothing is prefixed.
        assert_eq!(preview_link("docs/a.md", ""), open("docs/a.md", None));
        // Absolute paths are not joined.
        assert_eq!(preview_link("C:/proj/a.md", "docs"), open("C:/proj/a.md", None));
        // The thread's rules for everything else.
        assert_eq!(
            preview_link("https://example.com/x", "docs"),
            LinkAction::OpenUrl("https://example.com/x".into())
        );
        assert_eq!(preview_link("www.example.com", "docs"), LinkAction::OpenUrl("https://www.example.com".into()));
        assert_eq!(preview_link("mailto:a@b.c", "docs"), LinkAction::OpenUrl("mailto:a@b.c".into()));
        for unsafe_link in [
            "javascript:alert(1)",
            "file:C:/Windows/System32/calc.exe",
            "ms-settings:privacy",
            "#section",
            "",
        ] {
            assert_eq!(preview_link(unsafe_link, "docs"), LinkAction::Ignore, "{unsafe_link}");
        }
    }

    #[test]
    fn alert_colours_are_tailwinds_and_differ_by_theme() {
        let (border, light) = Alert::Note.colors(false);
        let (_, dark) = Alert::Note.colors(true);
        assert_eq!(light, Color::rgb(0x2563eb));
        assert_eq!(dark, Color::rgb(0x60a5fa));
        assert_eq!(border.hex(), "#3b82f6b3");
        let names: Vec<_> = Alert::ALL.iter().map(|a| a.name()).collect();
        assert_eq!(names, ["note", "tip", "important", "warning", "caution"]);
    }
}
