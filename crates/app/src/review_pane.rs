//! The review pane beside the chat (after Zed's agent "Review Changes"): every file the session's
//! folder has changed, with its diff. With checkpoints on, a file can be put back the way it was
//! before the agent's first edit.

use std::collections::HashSet;

use gpui::{Context, EventEmitter, ScrollHandle, SharedString, Task, prelude::*};
use hermes_runtime::changes::{Changes, ChangesSource, FileChange};
use ui::{Callout, Disclosure, Tooltip, prelude::*};

use crate::backend::backend;

/// Lines of one file's diff shown before "… more lines".
const MAX_PATCH_LINES: usize = 400;

pub enum ReviewEvent {
    Close,
}

pub struct ReviewPane {
    session_id: Option<String>,
    changes: Option<Changes>,
    expanded: HashSet<String>,
    loading: bool,
    error: Option<String>,
    scroll: ScrollHandle,
    _load: Option<Task<()>>,
}

impl EventEmitter<ReviewEvent> for ReviewPane {}

impl ReviewPane {
    pub fn new() -> Self {
        Self {
            session_id: None,
            changes: None,
            expanded: HashSet::new(),
            loading: false,
            error: None,
            scroll: ScrollHandle::new(),
            _load: None,
        }
    }

    /// Shows `session_id`'s changes (reloads them when it's the same session).
    pub fn show(&mut self, session_id: Option<String>, cx: &mut Context<Self>) {
        if self.session_id != session_id {
            self.changes = None;
            self.expanded.clear();
        }
        self.session_id = session_id;
        self.reload(cx);
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.session_id.clone() else {
            return;
        };
        self.loading = true;
        let backend = backend(cx);
        // git runs off the UI thread.
        let load = cx
            .background_executor()
            .spawn(async move { backend.runtime.changes(&id) });
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(changes) => {
                        // A single file opens by itself.
                        if changes.files.len() == 1 {
                            this.expanded.insert(changes.files[0].path.clone());
                        }
                        this.changes = Some(changes);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn revert(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(id) = self.session_id.clone() else {
            return;
        };
        match backend(cx).runtime.revert_file(&id, &path) {
            Ok(()) => self.reload(cx),
            Err(error) => {
                self.error = Some(format!("Couldn't revert {path}: {error:#}"));
                cx.notify();
            }
        }
    }

    fn render_patch(patch: &str, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let lines: Vec<&str> = patch.lines().collect();
        let more = lines.len().saturating_sub(MAX_PATCH_LINES);
        v_flex()
            .w_full()
            .py_1()
            .font_buffer(cx)
            .text_buffer(cx)
            .children(lines.into_iter().take(MAX_PATCH_LINES).map(|line| {
                let (bg, color) = match line.chars().next() {
                    Some('+') => (Some(status.created_background), Color::Created),
                    Some('-') => (Some(status.deleted_background), Color::Deleted),
                    Some('@') => (None, Color::Muted),
                    _ => (None, Color::Default),
                };
                div()
                    .w_full()
                    .px_2()
                    .when_some(bg, |this, bg| this.bg(bg))
                    .child(
                        Label::new(if line.is_empty() {
                            " ".to_string()
                        } else {
                            line.replace('\t', "    ")
                        })
                        .buffer_font(cx)
                        .size(LabelSize::Small)
                        .color(color)
                        .single_line(),
                    )
            }))
            .when(more > 0, |this| {
                this.child(
                    div().px_2().py_1().child(
                        Label::new(format!("… {more} more lines"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .border_t_1()
            .border_color(colors.border_variant)
    }

    fn render_file(
        &self,
        index: usize,
        file: &FileChange,
        revertible: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let open = self.expanded.contains(&file.path);
        let path = file.path.clone();
        let toggle_path = file.path.clone();
        let (dir, name) = match file.path.rsplit_once('/') {
            Some((dir, name)) => (Some(dir.to_string()), name.to_string()),
            None => (None, file.path.clone()),
        };
        v_flex()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(colors.border_variant)
            .overflow_hidden()
            .child(
                h_flex()
                    .id(("review-file", index))
                    .px_2()
                    .py_1()
                    .gap_2()
                    .bg(colors.editor_background)
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.ghost_element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded.remove(&toggle_path) {
                            this.expanded.insert(toggle_path.clone());
                        }
                        cx.notify();
                    }))
                    .child(Disclosure::new(("review-disclosure", index), open))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(Label::new(name).size(LabelSize::Small))
                            .when_some(dir, |this, dir| {
                                this.child(
                                    Label::new(dir)
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted)
                                        .truncate(),
                                )
                            }),
                    )
                    .when(file.created, |this| {
                        this.child(
                            Label::new("new")
                                .size(LabelSize::XSmall)
                                .color(Color::Created),
                        )
                    })
                    .when(file.deleted, |this| {
                        this.child(
                            Label::new("deleted")
                                .size(LabelSize::XSmall)
                                .color(Color::Deleted),
                        )
                    })
                    .child(
                        Label::new(format!("+{}", file.added))
                            .size(LabelSize::Small)
                            .color(Color::Created),
                    )
                    .child(
                        Label::new(format!("−{}", file.removed))
                            .size(LabelSize::Small)
                            .color(Color::Deleted),
                    )
                    .when(revertible, |this| {
                        this.child(
                            IconButton::new(("review-revert", index), IconName::Undo)
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::text(
                                    "Put this file back as it was before the session's first edit",
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.revert(path.clone(), cx)
                                })),
                        )
                    }),
            )
            .when(open, |this| this.child(Self::render_patch(&file.patch, cx)))
    }
}

impl Render for ReviewPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        let colors = cx.theme().colors();
        let (source, files, note) = match &self.changes {
            Some(changes) => (changes.source, changes.files.clone(), changes.note.clone()),
            None => (None, Vec::new(), None),
        };
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let subtitle: SharedString = match source {
            Some(ChangesSource::Session) => "Since this session's first edit".into(),
            Some(ChangesSource::Git) => "Working tree vs. HEAD (git)".into(),
            None => "".into(),
        };
        let revertible = source == Some(ChangesSource::Session);
        let has_note = note.is_some();
        v_flex()
            .size_full()
            .bg(colors.panel_background)
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new("Review Changes"))
                            .when(!subtitle.is_empty(), |this| {
                                this.child(
                                    Label::new(subtitle)
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                            }),
                    )
                    .when(!files.is_empty(), |this| {
                        this.child(
                            Label::new(format!(
                                "{} file{} +{added} −{removed}",
                                files.len(),
                                if files.len() == 1 { "" } else { "s" }
                            ))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        )
                    })
                    .child(
                        IconButton::new("review-refresh", IconName::RotateCw)
                            .icon_size(IconSize::Small)
                            .disabled(self.loading)
                            .tooltip(Tooltip::text("Refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(
                        IconButton::new("review-close", IconName::Close)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Close (Ctrl+Shift+D)"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ReviewEvent::Close))),
                    ),
            )
            .child(
                v_flex()
                    .id("review-files")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_2()
                    .gap_2()
                    .when_some(self.error.clone(), |this, error| {
                        this.child(Callout::new().severity(Severity::Error).title(error))
                    })
                    .when_some(note, |this, note| {
                        this.child(Label::new(note).size(LabelSize::Small).color(Color::Muted))
                    })
                    .when(
                        self.changes.is_some()
                            && files.is_empty()
                            && self.error.is_none()
                            && !has_note,
                        |this| {
                            this.child(
                                Label::new("No changes.")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                        },
                    )
                    .when(self.changes.is_none() && self.loading, |this| {
                        this.child(
                            Label::new("Loading…")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    })
                    .children(
                        files
                            .iter()
                            .enumerate()
                            .map(|(index, file)| self.render_file(index, file, revertible, cx)),
                    ),
            )
    }
}
