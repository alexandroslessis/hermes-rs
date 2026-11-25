//! The file view of the Files tab: one file, read-only. Source with syntax colours and line
//! numbers (GPUI Kit's code editor in its read-only mode: selection, copy, search with Ctrl+F,
//! horizontal scroll for long lines, scrolling that stays fast on tens of thousands of lines),
//! markdown shown rendered with a switch to its source, an SVG shown as a picture with a switch to
//! its XML, pictures (decoded and, when large, shrunk on a background thread; the view draws the
//! result), and a note for what is not text ("Binary file, N bytes") or may not be shown (outside
//! the folder, credentials, too large) with the editor offered where that makes sense.
//!
//! What it reads and what it refuses is decided by [`super::fs`] behind the backend; this view
//! only draws the answer. It asks the [`crate::model::Backend`] (through the app model) on a
//! background task, one read at a time, and drops an answer for a file it is no longer asked
//! for. A file over [`super::fs::HIGHLIGHT_MAX`] arrives as plain text (no syntax work); one over
//! [`super::fs::OPEN_MAX`] is refused with the editor offered. Invalid UTF-8 is shown with
//! replacement characters and says so. A link in rendered markdown follows the thread's safe
//! rules ([`crate::thread::markdown::link_action`]): web links open in the browser, a relative
//! link opens that file here (resolved against the file's own folder), anything else does nothing.
//!
//! **Jump to line** (`path:line` from a link): the line is selected and scrolled into view
//! (a few rows of context above it).
//!
//! t3code: `apps/web/src/components/files/FilePreviewPanel.tsx` (the header: path, actions; the
//! body by kind), `ReadOnlySourcePreview.tsx` (the read-only source), `FileMarkdownPreview.tsx`
//! (rendered markdown), `FileBreadcrumbs.tsx` (the path), `BinaryFilePreview.tsx`,
//! `decodeImagePreview.ts` / `PreviewImage.tsx` (images decoded off the main thread),
//! `fileLineReveal.ts` (jump to a line). Gallery story: `panel-files`.
//!
//! Not copied: editing and saving, PDF / office / audio / font / notebook / table previews, the
//! breadcrumb menus, comments on lines.

use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render, RenderImage,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _,
        input::{Editor, EditorState},
        text::{TextView, TextViewState},
    },
    div, img,
    prelude::FluentBuilder as _,
    px,
};

use super::fs::{Content, FileRead, Refused, TextFile, human_size};
use crate::design::{
    MiddleTruncate, ToastSpec,
    button::{Button, Size, Variant, icon_button},
    toast,
    tokens::{radius, text},
};
use crate::dev::ProbeExt as _;
use crate::model::AppModel;
use crate::theme::DesignTheme as _;
use crate::thread::markdown;

/// The longest edge of a picture the view keeps (larger ones are shrunk when read).
pub const PICTURE_EDGE: u32 = 2048;
/// The most memory a picture's decoder may take.
const DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// What the view tells the panel.
#[derive(Clone, Debug)]
pub enum ViewEvent {
    /// The back button.
    Back,
    Notice(ToastSpec),
    /// "View diff" for the file shown.
    ViewDiff(String),
    /// A file was read: the tree can show where it is (its path from the project's folder).
    Shown(String),
}

/// Source or the rendered form of a file that has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Preview,
    Source,
}

/// What the body shows, for the tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Code,
    Markdown,
    Svg,
    Picture,
    Binary,
}

/// A picture ready to draw.
#[derive(Clone)]
pub struct Picture {
    image: Arc<RenderImage>,
    pub width: u32,
    pub height: u32,
    /// It was larger than [`PICTURE_EDGE`] and was shrunk.
    pub shrunk: bool,
}

enum PictureState {
    /// Being decoded on a background thread.
    Decoding,
    Ready(Picture),
    Bad(String),
}

enum Body {
    Code(Entity<EditorState>),
    Markdown {
        text: SharedString,
        preview: Entity<TextViewState>,
        source: Option<Entity<EditorState>>,
    },
    Svg {
        text: SharedString,
        /// The picture GPUI rasterises (on its background executor, at most 8192 px a side),
        /// made once from [`svg_without_references`].
        image: Arc<gpui_kit::Image>,
        source: Option<Entity<EditorState>>,
    },
    Picture(PictureState),
    Binary,
}

struct Shown {
    rel: String,
    size: u64,
    lines: Option<usize>,
    lossy: bool,
    /// Over the highlight limit: plain text.
    plain: bool,
    /// Lines over [`super::fs::LINE_MAX`] were cut.
    cut: bool,
    line: Option<u32>,
    language: String,
    body: Body,
}

enum Phase {
    Empty,
    Loading(String),
    Failed { asked: String, refused: Refused },
    Ready(Box<Shown>),
}

pub struct FileView {
    app: Entity<AppModel>,
    session: Option<String>,
    phase: Phase,
    mode: ViewMode,
    /// The file shown is among the working tree's changes (the panel knows).
    in_diff: bool,
    generation: u64,
    reads: usize,
    /// Decode pictures where they are read (the gallery's stories cannot wait for a task).
    inline_decode: bool,
    /// The threads pictures were decoded on (tests assert none is the UI's).
    #[cfg(test)]
    pub decoded_on: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>,
}

impl EventEmitter<ViewEvent> for FileView {}

// --- pictures, decoded off the UI thread ---------------------------------------------------------

/// Decodes `bytes` into what the view draws: oriented, at most [`PICTURE_EDGE`] on a side, in the
/// renderer's channel order. Runs on a background thread.
pub fn prepare_picture(bytes: &[u8]) -> Result<Picture, String> {
    use image::{ImageDecoder as _, ImageReader, Limits};
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("This is not an image it can read ({e})."))?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(DECODE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| match e {
        image::ImageError::Limits(_) => "This image is too large to decode here.".to_string(),
        other => format!("This is not an image it can read ({other})."),
    })?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|e| match e {
        image::ImageError::Limits(_) => "This image is too large to decode here.".to_string(),
        other => format!("This is not an image it can read ({other})."),
    })?;
    decoded.apply_orientation(orientation);
    let (width, height) = (decoded.width(), decoded.height());
    let shrunk = width.max(height) > PICTURE_EDGE;
    if shrunk {
        decoded = decoded.thumbnail(PICTURE_EDGE, PICTURE_EDGE);
    }
    let (shown_w, shown_h) = (decoded.width(), decoded.height());
    let mut pixels = decoded.into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Picture {
        image: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])),
        // The size the file has, not the shrunk copy's.
        width,
        height,
        shrunk: shrunk && (shown_w, shown_h) != (width, height),
    })
}

// --- the view ---------------------------------------------------------------------------------------

impl FileView {
    pub fn new(app: Entity<AppModel>) -> Self {
        Self {
            app,
            session: None,
            phase: Phase::Empty,
            mode: ViewMode::Preview,
            in_diff: false,
            generation: 0,
            reads: 0,
            inline_decode: false,
            #[cfg(test)]
            decoded_on: Arc::default(),
        }
    }

    /// Which thread's folder files are read from.
    pub fn set_session(&mut self, session: Option<String>, cx: &mut Context<Self>) {
        if self.session != session {
            self.session = session;
            self.generation += 1;
            self.phase = Phase::Empty;
            cx.notify();
        }
    }

    pub fn set_in_diff(&mut self, in_diff: bool, cx: &mut Context<Self>) {
        if self.in_diff != in_diff {
            self.in_diff = in_diff;
            cx.notify();
        }
    }

    /// Pictures are decoded inline (stories only).
    pub fn set_inline_decode(&mut self, inline: bool) {
        self.inline_decode = inline;
    }

    /// Shows a file already read (the gallery's stories: a story is drawn twice and cannot wait
    /// for a background read).
    pub fn show_read(
        &mut self,
        read: FileRead,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.generation += 1;
        let asked = read.rel.clone();
        self.loaded(self.generation, asked, line, Ok(read), window, cx);
    }

    /// Shows a refusal (stories).
    pub fn show_refused(&mut self, asked: &str, refused: Refused, cx: &mut Context<Self>) {
        self.generation += 1;
        self.phase = Phase::Failed {
            asked: asked.to_string(),
            refused,
        };
        cx.notify();
    }

    pub fn reads(&self) -> usize {
        self.reads
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.phase, Phase::Empty)
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.phase, Phase::Loading(_))
    }

    /// The canonical path of the file shown.
    pub fn shown(&self) -> Option<&str> {
        match &self.phase {
            Phase::Ready(shown) => Some(&shown.rel),
            _ => None,
        }
    }

    /// The path of the file being read, or shown, or refused.
    pub fn asked(&self) -> Option<&str> {
        match &self.phase {
            Phase::Loading(asked) | Phase::Failed { asked, .. } => Some(asked),
            Phase::Ready(shown) => Some(&shown.rel),
            Phase::Empty => None,
        }
    }

    pub fn refused(&self) -> Option<&Refused> {
        match &self.phase {
            Phase::Failed { refused, .. } => Some(refused),
            _ => None,
        }
    }

    pub fn kind(&self) -> Option<Kind> {
        match &self.phase {
            Phase::Ready(shown) => Some(match &shown.body {
                Body::Code(_) => Kind::Code,
                Body::Markdown { .. } => Kind::Markdown,
                Body::Svg { .. } => Kind::Svg,
                Body::Picture(_) => Kind::Picture,
                Body::Binary => Kind::Binary,
            }),
            _ => None,
        }
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    pub fn is_plain(&self) -> bool {
        matches!(&self.phase, Phase::Ready(s) if s.plain)
    }

    pub fn is_lossy(&self) -> bool {
        matches!(&self.phase, Phase::Ready(s) if s.lossy)
    }

    pub fn language(&self) -> Option<&str> {
        match &self.phase {
            Phase::Ready(shown) => Some(&shown.language),
            _ => None,
        }
    }

    /// The code editor holding the source, when the body shows one (tests read its text, its
    /// selection and the rows it has laid out).
    pub fn editor(&self) -> Option<&Entity<EditorState>> {
        match &self.phase {
            Phase::Ready(shown) => match &shown.body {
                Body::Code(editor) => Some(editor),
                Body::Markdown { source, .. } | Body::Svg { source, .. } => source.as_ref(),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn picture(&self) -> Option<&Picture> {
        match &self.phase {
            Phase::Ready(shown) => match &shown.body {
                Body::Picture(PictureState::Ready(picture)) => Some(picture),
                _ => None,
            },
            _ => None,
        }
    }

    /// Empties the view (no file).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.phase = Phase::Empty;
        cx.notify();
    }

    /// Reads `path` (relative to the thread's folder, or absolute) and shows it, with `line`
    /// selected. The read runs in the background; what is shown stays until it lands.
    pub fn open(
        &mut self,
        path: &str,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(session), Some(backend)) = (self.session.clone(), self.app.read(cx).backend())
        else {
            self.phase = Phase::Failed {
                asked: path.to_string(),
                refused: Refused::Unreadable("Open a thread to see its files.".to_string()),
            };
            cx.notify();
            return;
        };
        self.generation += 1;
        self.reads += 1;
        let generation = self.generation;
        self.phase = Phase::Loading(path.to_string());
        self.mode = ViewMode::Preview;
        cx.notify();
        let asked = path.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let read = {
                let asked = asked.clone();
                cx.background_spawn(async move { backend.project_read(&session, &asked) })
                    .await
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.loaded(generation, asked, line, read, window, cx)
            });
        })
        .detach();
    }

    fn loaded(
        &mut self,
        generation: u64,
        asked: String,
        line: Option<u32>,
        read: Result<FileRead, Refused>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        let read = match read {
            Ok(read) => read,
            Err(refused) => {
                self.phase = Phase::Failed { asked, refused };
                cx.notify();
                return;
            }
        };
        let FileRead {
            rel, size, content, ..
        } = read;
        let mut shown = Shown {
            rel: rel.clone(),
            size,
            lines: None,
            lossy: false,
            plain: false,
            cut: false,
            line,
            language: "text".to_string(),
            body: Body::Binary,
        };
        match content {
            Content::Text(file) => {
                let TextFile {
                    text,
                    lossy,
                    language,
                    highlight,
                    markdown,
                    svg,
                    lines,
                    cut_lines,
                } = file;
                shown.cut = cut_lines;
                shown.lossy = lossy;
                shown.plain = !highlight;
                shown.lines = Some(lines);
                shown.language = language.clone();
                let text: SharedString = text.into();
                shown.body = if markdown {
                    let preview = cx.new(|cx| TextViewState::markdown(&text, cx).selectable(true));
                    Body::Markdown {
                        text: text.clone(),
                        preview,
                        source: None,
                    }
                } else if svg {
                    Body::Svg {
                        image: Arc::new(gpui_kit::Image::from_bytes(
                            gpui_kit::ImageFormat::Svg,
                            svg_without_references(&text).into_bytes(),
                        )),
                        text: text.clone(),
                        source: None,
                    }
                } else {
                    Body::Code(code_editor(&text, &language, window, cx))
                };
                if matches!(shown.body, Body::Code(_)) {
                    if let (Some(line), Body::Code(editor)) = (line, &shown.body) {
                        jump_to_line(editor, &text, line, cx);
                    }
                }
            }
            Content::Image(bytes) => {
                shown.body = Body::Picture(PictureState::Decoding);
                if self.inline_decode {
                    shown.body = Body::Picture(match prepare_picture(&bytes) {
                        Ok(picture) => PictureState::Ready(picture),
                        Err(why) => PictureState::Bad(why),
                    });
                } else {
                    self.decode_picture(generation, bytes, cx);
                }
            }
            Content::Binary => {}
        }
        self.phase = Phase::Ready(Box::new(shown));
        // A markdown file or an SVG opens rendered; a jump to a line wants the source.
        self.mode = if line.is_some() {
            ViewMode::Source
        } else {
            ViewMode::Preview
        };
        if line.is_some() {
            self.ensure_source(window, cx);
        }
        cx.emit(ViewEvent::Shown(rel));
        cx.notify();
    }

    fn decode_picture(&mut self, generation: u64, bytes: Vec<u8>, cx: &mut Context<Self>) {
        #[cfg(test)]
        let decoded_on = self.decoded_on.clone();
        cx.spawn(async move |this, cx| {
            let picture = cx
                .background_spawn(async move {
                    #[cfg(test)]
                    decoded_on
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(std::thread::current().id());
                    prepare_picture(&bytes)
                })
                .await;
            let _ = this.update(cx, |this, cx| this.pictured(generation, picture, cx));
        })
        .detach();
    }

    fn pictured(
        &mut self,
        generation: u64,
        picture: Result<Picture, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        if let Phase::Ready(shown) = &mut self.phase {
            let state = match picture {
                Ok(picture) => PictureState::Ready(picture),
                Err(why) => PictureState::Bad(why),
            };
            match &mut shown.body {
                Body::Picture(slot) => *slot = state,
                _ => {}
            }
        }
        cx.notify();
    }

    /// Source or rendered, for markdown and SVG.
    pub fn set_mode(&mut self, mode: ViewMode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        if mode == ViewMode::Source {
            self.ensure_source(window, cx);
        }
        cx.notify();
    }

    /// Makes the source editor of a markdown file or an SVG (they open rendered, so it is made
    /// when first wanted).
    fn ensure_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Phase::Ready(shown) = &mut self.phase else {
            return;
        };
        let language = shown.language.clone();
        let line = shown.line;
        let (text, source) = match &mut shown.body {
            Body::Markdown { text, source, .. } | Body::Svg { text, source, .. } => (text, source),
            _ => return,
        };
        if source.is_none() {
            let editor = code_editor(text, &language, window, cx);
            if let Some(line) = line {
                jump_to_line(&editor, text, line, cx);
            }
            *source = Some(editor);
        }
    }

    // --- actions ---

    /// Opens the file in the user's editor (Zed, VS Code, or the system's default program).
    pub fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Some(backend)) = (self.session.clone(), self.app.read(cx).backend())
        else {
            return;
        };
        let (path, line) = match &self.phase {
            Phase::Ready(shown) => (shown.rel.clone(), shown.line),
            Phase::Failed { asked, .. } => (asked.clone(), None),
            _ => return,
        };
        cx.spawn(async move |this, cx| {
            let opened = {
                let path = path.clone();
                cx.background_spawn(async move { backend.project_open(&session, &path, line) })
                    .await
            };
            let _ = this.update(cx, |_, cx| {
                let notice = match opened {
                    Ok(name) => ToastSpec::new(toast::Kind::Info, format!("Opened in {name}"))
                        .description(path),
                    Err(why) => ToastSpec::new(toast::Kind::Error, "Could not open the file")
                        .description(why),
                };
                cx.emit(ViewEvent::Notice(notice));
            });
        })
        .detach();
    }

    fn copy_path(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.asked() {
            cx.write_to_clipboard(ClipboardItem::new_string(path.to_string()));
            cx.emit(ViewEvent::Notice(ToastSpec::new(
                toast::Kind::Info,
                "Path copied",
            )));
        }
    }
}

/// A read-only code editor over `text`, with line numbers and no wrapping.
fn code_editor(
    text: &SharedString,
    language: &str,
    window: &mut Window,
    cx: &mut Context<FileView>,
) -> Entity<EditorState> {
    cx.new(|cx| {
        EditorState::new(window, cx)
            .language(language.to_string())
            .line_number(true)
            .soft_wrap(false)
            .default_value(text.clone())
    })
}

/// Selects line `line` (1-based) of `text` and scrolls it into view with a few rows above it.
fn jump_to_line(editor: &Entity<EditorState>, text: &str, line: u32, cx: &mut Context<FileView>) {
    let (start, end, row) = line_span(text, line);
    // The editor's rows are 1.5 times the monospace size (`Editor`'s own rule).
    let row_height = cx.theme().mono_font_size * 1.5;
    let above = row.saturating_sub(JUMP_CONTEXT) as f32;
    editor.update(cx, |state, cx| {
        state.set_selected_range(start..end, cx);
        state.set_scroll_offset(gpui_kit::point(px(0.), -(row_height * above)), cx);
    });
}

/// Rows of context kept above a line a jump scrolls to.
pub const JUMP_CONTEXT: usize = 4;

/// The byte range of line `line` (1-based, clamped to the text) of `text`, without its newline,
/// and its 0-based row.
pub fn line_span(text: &str, line: u32) -> (usize, usize, usize) {
    let wanted = (line.max(1) - 1) as usize;
    let mut start = 0;
    let mut row = 0;
    while row < wanted {
        match text[start..].find('\n') {
            Some(at) => {
                start += at + 1;
                row += 1;
            }
            // Past the last line: the last one.
            None => break,
        }
    }
    let rest = &text[start..];
    let end = start + rest.find('\n').unwrap_or(rest.len());
    let end = if text[..end].ends_with('\r') {
        end - 1
    } else {
        end
    };
    (start, end.max(start), row)
}

// --- drawing ----------------------------------------------------------------------------------------

impl FileView {
    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let path = super::fs::shown_name(self.asked().unwrap_or_default());
        let shown = matches!(self.phase, Phase::Ready(_));
        let failed = matches!(self.phase, Phase::Failed { .. });
        let can_edit = match &self.phase {
            Phase::Ready(_) => true,
            Phase::Failed { refused, .. } => refused.can_open_in_editor(),
            _ => false,
        };
        let rel = self.shown().map(str::to_string);
        div()
            .id("file-header")
            .flex()
            .items_center()
            .gap(px(4.))
            .h(px(crate::design::panel::HEADER_HEIGHT))
            .flex_shrink_0()
            .px(px(8.))
            .border_b_1()
            .border_color(palette.border.hsla())
            .child(
                icon_button("file-back", IconName::ArrowLeft, "Back to the files")
                    .variant(Variant::GhostMuted)
                    .size(Size::Xs)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ViewEvent::Back))),
            )
            .child(
                div()
                    .id("file-path")
                    .flex_1()
                    .min_w_0()
                    .text_size(text::XS.size)
                    .text_color(if failed {
                        palette.muted_foreground.hsla()
                    } else {
                        palette.foreground.hsla()
                    })
                    .font_weight(FontWeight::MEDIUM)
                    .child(MiddleTruncate::new("file-path-text", path).tail(24))
                    .probe(),
            )
            .when(self.in_diff && shown, |d| {
                d.child(
                    Button::new("file-diff", "Diff")
                        .icon(IconName::FileDiff)
                        .variant(Variant::Outline)
                        .size(Size::Xs)
                        .tooltip("This file has changes: view its diff")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            if let Some(rel) = rel.clone() {
                                cx.emit(ViewEvent::ViewDiff(rel));
                            }
                        })),
                )
            })
            .when(shown || failed, |d| {
                d.child(
                    icon_button("file-copy-path", IconName::Copy, "Copy the path")
                        .variant(Variant::GhostMuted)
                        .size(Size::Xs)
                        .on_click(cx.listener(|this, _, _, cx| this.copy_path(cx))),
                )
            })
            .when(can_edit, |d| {
                d.child(
                    icon_button("file-editor", IconName::ExternalLink, "Open in editor")
                        .variant(Variant::GhostMuted)
                        .size(Size::Xs)
                        .on_click(cx.listener(|this, _, _, cx| this.open_in_editor(cx))),
                )
            })
            .probe()
            .into_any_element()
    }

    /// The line under the header: size, lines, what the view did to the text, and the switch
    /// between source and the rendered form.
    fn subbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Phase::Ready(shown) = &self.phase else {
            return None;
        };
        let palette = cx.design_theme().palette;
        let mut meta = vec![human_size(shown.size)];
        if let Some(lines) = shown.lines {
            meta.push(match lines {
                1 => "1 line".to_string(),
                n => format!("{} lines", super::finder::group_digits(n)),
            });
        }
        if let Some(picture) = self.picture() {
            meta.push(format!("{} x {}", picture.width, picture.height));
        }
        let has_switch = matches!(shown.body, Body::Markdown { .. } | Body::Svg { .. });
        let mode = self.mode;
        Some(
            div()
                .id("file-subbar")
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .h(px(32.))
                .flex_shrink_0()
                .px(px(12.))
                .border_b_1()
                .border_color(palette.border.hsla())
                .child(
                    div()
                        .id("file-meta")
                        .min_w_0()
                        .truncate()
                        .text_size(text::XXS2.size)
                        .text_color(palette.muted_foreground.hsla())
                        .child(meta.join(" - ")),
                )
                .when(has_switch, |d| {
                    d.child(
                        div()
                            .id("file-modes")
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .p(px(2.))
                            .rounded(radius::MD)
                            .bg(palette.muted.hsla())
                            .child(
                                Button::new("file-mode-preview", "Preview")
                                    .variant(if mode == ViewMode::Preview {
                                        Variant::Outline
                                    } else {
                                        Variant::Ghost
                                    })
                                    .size(Size::Xs)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_mode(ViewMode::Preview, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("file-mode-source", "Source")
                                    .variant(if mode == ViewMode::Source {
                                        Variant::Outline
                                    } else {
                                        Variant::Ghost
                                    })
                                    .size(Size::Xs)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_mode(ViewMode::Source, window, cx)
                                    })),
                            )
                            .probe(),
                    )
                })
                .probe()
                .into_any_element(),
        )
    }

    /// Notes about how the text was shown.
    fn notes(&self, cx: &App) -> Option<AnyElement> {
        let Phase::Ready(shown) = &self.phase else {
            return None;
        };
        let palette = cx.design_theme().palette;
        let mut lines: Vec<&str> = Vec::new();
        if shown.lossy {
            lines.push("This file is not valid UTF-8: unreadable bytes are shown as replacement characters.");
        }
        if shown.plain && matches!(shown.body, Body::Code(_)) {
            lines.push("This file is large, so it is shown as plain text, without syntax colours.");
        }
        if shown.cut {
            lines.push("Lines over 10,000 bytes are cut here; Open in editor shows them whole.");
        }
        (!lines.is_empty()).then(|| {
            div()
                .id("file-notes")
                .flex()
                .flex_col()
                .gap(px(2.))
                .flex_shrink_0()
                .px(px(12.))
                .py(px(6.))
                .border_b_1()
                .border_color(palette.border.hsla())
                .bg(palette.warning_surface.hsla())
                .text_size(text::XXS2.size)
                .text_color(palette.muted_foreground.hsla())
                .children(lines.into_iter().map(SharedString::from))
                .probe()
                .into_any_element()
        })
    }

    fn message(
        &self,
        id: &'static str,
        lines: Vec<String>,
        cx: &App,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let palette = cx.design_theme().palette;
        div()
            .id(id)
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .p(px(24.))
            .text_size(text::XS.size)
            .text_center()
            .text_color(palette.muted_foreground.hsla())
            .children(lines.into_iter().map(SharedString::from))
    }

    fn body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = cx.design_theme().palette;
        let can_edit = |cx: &mut Context<Self>| {
            Button::new("file-open-editor", "Open in editor")
                .icon(IconName::ExternalLink)
                .variant(Variant::Outline)
                .size(Size::Xs)
                .on_click(cx.listener(|this, _, _, cx| this.open_in_editor(cx)))
        };
        let _ = window;
        match &self.phase {
            Phase::Empty => self
                .message(
                    "file-empty",
                    vec!["Select a file to view it.".to_string()],
                    cx,
                )
                .probe()
                .into_any_element(),
            Phase::Loading(_) => self
                .message("file-loading", vec!["Reading the file...".to_string()], cx)
                .probe()
                .into_any_element(),
            Phase::Failed { refused, .. } => {
                let edit = refused.can_open_in_editor();
                self.message("file-refused", vec![refused.message()], cx)
                    .when(edit, |d| d.child(can_edit(cx)))
                    .probe()
                    .into_any_element()
            }
            Phase::Ready(shown) => match &shown.body {
                Body::Binary => self
                    .message(
                        "file-binary",
                        vec![format!(
                            "Binary file, {} bytes",
                            super::finder::group_digits(shown.size as usize)
                        )],
                        cx,
                    )
                    .child(can_edit(cx))
                    .probe()
                    .into_any_element(),
                Body::Code(editor) => code_view(editor),
                Body::Markdown {
                    preview, source, ..
                } => match (self.mode, source) {
                    (ViewMode::Source, Some(editor)) => code_view(editor),
                    _ => {
                        let base = parent_dir(&shown.rel);
                        let view = markdown::preview(TextView::new(preview), cx, base);
                        div()
                            .id("file-markdown")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(px(14.))
                            .text_size(text::SM.size)
                            .text_color(palette.foreground.hsla())
                            .child(view)
                            .probe()
                            .into_any_element()
                    }
                },
                Body::Svg { image, source, .. } => match (self.mode, source) {
                    (ViewMode::Source, Some(editor)) => code_view(editor),
                    _ => {
                        picture_frame(
                            "file-svg",
                            img(gpui_kit::ImageSource::Image(image.clone()))
                                .max_w_full()
                                .max_h_full()
                                .object_fit(ObjectFit::Contain),
                            cx,
                        )
                    }
                },
                Body::Picture(PictureState::Decoding) => self
                    .message(
                        "file-decoding",
                        vec!["Decoding the picture...".to_string()],
                        cx,
                    )
                    .probe()
                    .into_any_element(),
                Body::Picture(PictureState::Bad(why)) => self
                    .message("file-picture-bad", vec![why.clone()], cx)
                    .child(can_edit(cx))
                    .probe()
                    .into_any_element(),
                Body::Picture(PictureState::Ready(picture)) => picture_frame(
                    "file-picture",
                    img(gpui_kit::ImageSource::Render(picture.image.clone()))
                        .max_w_full()
                        .max_h_full()
                        .object_fit(ObjectFit::Contain),
                    cx,
                ),
            },
        }
    }
}

/// `svg` with every reference to something outside it emptied: an `href` (`xlink:href`, any
/// prefix) whose value does not start with `#` (a part of the same picture) or `data:` becomes
/// `""`. GPUI's SVG renderer (usvg) would otherwise read an `<image href="C:\...">` from the disk,
/// or from `\\host\share` (sending the Windows sign-in there). A value that hides its start
/// behind an entity (`&#92;&#92;host`) does not start with `#` or `data:`, so it goes too. usvg
/// fetches nothing over HTTP and runs no script; the source view shows the file as it is.
pub fn svg_without_references(svg: &str) -> String {
    let lower = svg.to_ascii_lowercase();
    let mut out = String::with_capacity(svg.len());
    let mut copied = 0;
    let mut from = 0;
    while let Some(found) = lower[from..].find("href") {
        let after = from + found + 4;
        from = after;
        let rest = &svg[after..];
        let eq = rest.len() - rest.trim_start().len();
        if !rest[eq..].starts_with('=') {
            continue;
        }
        let value_at = after + eq + 1;
        let rest = &svg[value_at..];
        let space = rest.len() - rest.trim_start().len();
        let open = value_at + space;
        let Some(quote) = svg[open..].chars().next().filter(|q| *q == '"' || *q == '\'') else {
            continue;
        };
        let Some(len) = svg[open + 1..].find(quote) else {
            break;
        };
        let value = &svg[open + 1..open + 1 + len];
        let close = open + 1 + len;
        if !(value.starts_with('#') || value.get(..5).is_some_and(|v| v.eq_ignore_ascii_case("data:"))) {
            out.push_str(&svg[copied..open + 1]);
            copied = close;
        }
        from = close;
    }
    out.push_str(&svg[copied..]);
    out
}

/// `docs/guide/intro.md` -> `docs/guide`.
fn parent_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(dir, _)| dir.to_string())
}

fn code_view(editor: &Entity<EditorState>) -> AnyElement {
    div()
        .id("file-code")
        .flex_1()
        .min_h_0()
        .child(
            Editor::new(editor)
                .readonly(true)
                .appearance(false)
                .bordered(false)
                .size_full(),
        )
        .probe()
        .into_any_element()
}

fn picture_frame(id: &'static str, picture: impl IntoElement, cx: &App) -> AnyElement {
    let palette = cx.design_theme().palette;
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(16.))
        .bg(palette.muted.hsla().opacity(0.3))
        .child(picture)
        .probe()
        .into_any_element()
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        div()
            .id("file-view")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(palette.background.hsla())
            .text_color(palette.foreground.hsla())
            .child(self.header(cx))
            .children(self.subbar(cx))
            .children(self.notes(cx))
            .child(self.body(window, cx))
            .probe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn a_line_is_found_by_number_and_clamped() {
        let text = "one\ntwo\r\nthree\n";
        assert_eq!(line_span(text, 1), (0, 3, 0));
        assert_eq!(&text[line_span(text, 2).0..line_span(text, 2).1], "two");
        assert_eq!(&text[line_span(text, 3).0..line_span(text, 3).1], "three");
        // Past the end: the last line (the empty one after the final newline).
        assert_eq!(line_span(text, 99).2, 3);
        assert_eq!(line_span("", 5), (0, 0, 0));
        assert_eq!(line_span("a\nb", 0), (0, 1, 0), "line 0 is line 1");
    }

    #[core::prelude::v1::test]
    fn a_picture_is_decoded_oriented_shrunk_and_swapped_for_the_renderer() {
        let png = crate::thread::composer::attach::test_png(30, 20);
        let picture = prepare_picture(&png).unwrap();
        assert_eq!(
            (picture.width, picture.height, picture.shrunk),
            (30, 20, false)
        );
        assert_eq!(
            picture.image.as_bytes(0).unwrap().len(),
            30 * 20 * 4,
            "BGRA"
        );
        // The first pixel of `test_png` is (0, 0, 128): blue and red swapped in memory.
        assert_eq!(&picture.image.as_bytes(0).unwrap()[..4], &[128, 0, 0, 255]);
        // A large one is shrunk but keeps the size the file has.
        let big = crate::thread::composer::attach::test_png(3000, 100);
        let picture = prepare_picture(&big).unwrap();
        assert_eq!(
            (picture.width, picture.height, picture.shrunk),
            (3000, 100, true)
        );
        // Not an image.
        assert!(prepare_picture(b"not a picture").is_err());
        assert!(prepare_picture(&png[..40]).is_err(), "cut off");
    }

    #[core::prelude::v1::test]
    fn an_svg_keeps_its_own_references_and_loses_every_other() {
        let svg = r##"<svg><use href="#dot"/><image xlink:href="\\host\share\a.png"/><image HREF = 'C:/Users/me/p.png'/><image href="data:image/png;base64,AA"/><image href="&#92;&#92;host\s.png"/><a href="https://x.y">t</a></svg>"##;
        let clean = svg_without_references(svg);
        assert_eq!(
            clean,
            r##"<svg><use href="#dot"/><image xlink:href=""/><image HREF = ''/><image href="data:image/png;base64,AA"/><image href=""/><a href="">t</a></svg>"##
        );
        // Nothing to do, an unclosed quote, a word "href" in text: unchanged.
        assert_eq!(svg_without_references("<svg/>"), "<svg/>");
        assert_eq!(svg_without_references("<a href=\"x"), "<a href=\"x");
        assert_eq!(svg_without_references("<text>href</text>"), "<text>href</text>");
        // Non-ASCII around it is kept whole.
        assert_eq!(
            svg_without_references("<text>καλη</text><image href='x.png'/>"),
            "<text>καλη</text><image href=''/>"
        );
    }

    #[core::prelude::v1::test]
    fn the_folder_a_relative_link_is_read_from_is_the_files_own() {
        assert_eq!(parent_dir("docs/guide/intro.md"), "docs/guide");
        assert_eq!(parent_dir("README.md"), "");
    }
}
