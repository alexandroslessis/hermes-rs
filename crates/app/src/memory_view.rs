//! The Memory screen (Hermes's memory manager): what the agent remembers in `MEMORY.md` and
//! `USER.md`, writes waiting for approval, and the approval switch.

use std::path::PathBuf;

use gpui::{App, Context, Entity, FocusHandle, Focusable, Subscription, Window, prelude::*};
use hermes_memory::{ENTRY_DELIMITER, MemoryStore, Operation, PendingWrite, Target, TurnFailures};
use std::sync::Arc;
use ui::{Callout, Checkbox, Divider, ToggleState, Tooltip, WithScrollbar, prelude::*};

use crate::backend::backend;
use crate::state::AppState;
use crate::text_area::{TextArea, TextAreaEvent};

const TARGETS: [Target; 2] = [Target::User, Target::Memory];

pub struct MemoryView {
    fields: [Entity<TextArea>; 2],
    /// The entry each field is editing, when it isn't adding a new one.
    editing: [Option<String>; 2],
    entries: [Vec<String>; 2],
    usage: [String; 2],
    pending: Vec<PendingWrite>,
    notice: Option<(Severity, String)>,
    scroll: gpui::ScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

fn slot(target: Target) -> usize {
    match target {
        Target::User => 0,
        Target::Memory => 1,
    }
}

/// Where Hermes keeps its memories (`HERMES_HOME`, else `~/.hermes`).
fn hermes_memories() -> Option<PathBuf> {
    let home = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".hermes")))?;
    let dir = home.join("memories");
    dir.is_dir().then_some(dir)
}

impl MemoryView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = |placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| TextArea::new(placeholder, cx))
        };
        let fields = [
            field("Add a fact about yourself (name, role, preferences)…", cx),
            field(
                "Add a note for the agent (environment, conventions, lessons)…",
                cx,
            ),
        ];
        let subscriptions = TARGETS
            .iter()
            .map(|&target| {
                cx.subscribe_in(
                    &fields[slot(target)],
                    window,
                    move |this: &mut Self, _, event, _, cx| {
                        if let TextAreaEvent::Submit = event {
                            this.save(target, cx);
                        }
                    },
                )
            })
            .collect();
        let mut view = Self {
            fields,
            editing: [None, None],
            entries: Default::default(),
            usage: Default::default(),
            pending: Vec::new(),
            notice: None,
            scroll: gpui::ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        view.refresh(cx);
        view
    }

    fn store(cx: &App) -> Arc<MemoryStore> {
        backend(cx).runtime.memory()
    }

    /// Re-reads the files (the agent may have written since the screen was last shown).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = Self::store(cx);
        for target in TARGETS {
            self.entries[slot(target)] = store.entries(target);
            self.usage[slot(target)] = store.usage(target);
        }
        self.pending = store.pending();
        cx.notify();
    }

    fn report(&mut self, result: serde_json::Value, done: &str, cx: &mut Context<Self>) -> bool {
        let ok = result["success"] == serde_json::json!(true);
        self.notice = Some(if ok {
            (Severity::Success, done.to_string())
        } else {
            (
                Severity::Error,
                result["error"]
                    .as_str()
                    .unwrap_or("The change failed.")
                    .to_string(),
            )
        });
        self.refresh(cx);
        ok
    }

    /// Adds the field's text, or saves the edit of the entry being edited.
    fn save(&mut self, target: Target, cx: &mut Context<Self>) {
        let field = self.fields[slot(target)].clone();
        let text = field.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let op = match self.editing[slot(target)].clone() {
            // A patch whose span is the whole entry rewrites it.
            Some(original) => Operation {
                action: "patch".into(),
                content: Some(text),
                old_text: Some(original.clone()),
                matched_entry: Some(original),
            },
            None => Operation {
                action: "add".into(),
                content: Some(text),
                ..Default::default()
            },
        };
        let result = Self::store(cx).apply(target, &[op], &mut TurnFailures::default());
        if self.report(result, "Saved.", cx) {
            self.editing[slot(target)] = None;
            field.update(cx, |field, cx| field.set_text("", cx));
        }
    }

    fn remove(&mut self, target: Target, entry: String, cx: &mut Context<Self>) {
        let op = Operation {
            action: "remove".into(),
            old_text: Some(entry.clone()),
            matched_entry: Some(entry),
            ..Default::default()
        };
        let result = Self::store(cx).apply(target, &[op], &mut TurnFailures::default());
        self.report(result, "Removed.", cx);
    }

    fn edit(&mut self, target: Target, entry: String, window: &mut Window, cx: &mut Context<Self>) {
        let field = self.fields[slot(target)].clone();
        field.update(cx, |field, cx| field.set_text(entry.clone(), cx));
        field.focus_handle(cx).focus(window, cx);
        self.editing[slot(target)] = Some(entry);
        cx.notify();
    }

    fn set_approval(&mut self, on: bool, cx: &mut Context<Self>) {
        let store = Self::store(cx);
        let mut config = store.config();
        config.write_approval = on;
        store.set_config(config);
        AppState::global(cx).update(cx, |state, cx| {
            state.config.memory_write_approval = on;
            state.save(cx);
        });
        cx.notify();
    }

    /// One-way import of Hermes's `MEMORY.md` / `USER.md` (entries already here are skipped).
    fn import_from_hermes(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = hermes_memories() else {
            return;
        };
        let store = Self::store(cx);
        let mut added = 0;
        let mut problems = Vec::new();
        for target in TARGETS {
            let Ok(raw) = std::fs::read_to_string(dir.join(target.file_name())) else {
                continue;
            };
            let existing = store.entries(target);
            let ops: Vec<Operation> = raw
                .trim_start_matches('\u{feff}')
                .split(ENTRY_DELIMITER)
                .map(str::trim)
                .filter(|entry| !entry.is_empty() && !existing.iter().any(|e| e == entry))
                .map(|entry| Operation {
                    action: "add".into(),
                    content: Some(entry.to_string()),
                    ..Default::default()
                })
                .collect();
            if ops.is_empty() {
                continue;
            }
            let result = store.apply(target, &ops, &mut TurnFailures::default());
            if result["success"] == serde_json::json!(true) {
                added += ops.len();
            } else {
                problems.push(format!(
                    "{}: {}",
                    target.file_name(),
                    result["error"].as_str().unwrap_or("failed")
                ));
            }
        }
        self.notice = Some(if problems.is_empty() {
            (
                Severity::Success,
                format!("Imported {added} entries from Hermes."),
            )
        } else {
            (Severity::Warning, problems.join("\n"))
        });
        self.refresh(cx);
    }

    fn render_store(&self, target: Target, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (title, description) = match target {
            Target::User => (
                "User profile",
                "Who you are: name, role, preferences, style.",
            ),
            Target::Memory => (
                "Agent notes",
                "Environment facts, conventions, tool quirks, lessons.",
            ),
        };
        let editing = self.editing[slot(target)].is_some();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Label::new(title).size(LabelSize::Large))
                            .child(
                                Label::new(target.file_name())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        Label::new(format!("{} chars", self.usage[slot(target)]))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                Label::new(description)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .children(
                self.entries[slot(target)]
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| {
                        let id = format!("{}-{i}", target.name());
                        let (edit_entry, remove_entry) = (entry.clone(), entry.clone());
                        h_flex()
                            .id(SharedString::from(format!("entry-{id}")))
                            .group("memory-entry")
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(colors.border_variant)
                            .bg(colors.editor_background)
                            .child(div().flex_1().min_w_0().text_sm().child(entry.clone()))
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("edit-{id}")),
                                    IconName::Pencil,
                                )
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text("Edit"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.edit(target, edit_entry.clone(), window, cx)
                                    },
                                )),
                            )
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("remove-{id}")),
                                    IconName::Trash,
                                )
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text("Delete"))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.remove(target, remove_entry.clone(), cx)
                                    },
                                )),
                            )
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .flex_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.editor_background)
                            .child(self.fields[slot(target)].clone()),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("save-{}", target.name())),
                            if editing { "Save" } else { "Add" },
                        )
                        .style(ButtonStyle::Filled)
                        .on_click(cx.listener(move |this, _, _, cx| this.save(target, cx))),
                    )
                    .when(editing, |this| {
                        this.child(
                            Button::new(
                                SharedString::from(format!("cancel-{}", target.name())),
                                "Cancel",
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.editing[slot(target)] = None;
                                    this.fields[slot(target)]
                                        .update(cx, |f, cx| f.set_text("", cx));
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            )
    }

    fn render_pending(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.pending.is_empty() {
            return None;
        }
        let colors = cx.theme().colors();
        Some(
            v_flex()
                .gap_2()
                .child(
                    Label::new(format!("Waiting for approval ({})", self.pending.len()))
                        .size(LabelSize::Large),
                )
                .children(self.pending.iter().map(|write| {
                    let (approve_id, discard_id) = (write.id.clone(), write.id.clone());
                    v_flex()
                        .gap_1()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(colors.border_variant)
                        .bg(colors.editor_background)
                        .child(div().text_sm().child(write.summary.clone()))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new(
                                        SharedString::from(format!("approve-{}", write.id)),
                                        "Approve",
                                    )
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let result = Self::store(cx).approve(&approve_id);
                                            this.report(result, "Approved and saved.", cx);
                                        },
                                    )),
                                )
                                .child(
                                    Button::new(
                                        SharedString::from(format!("discard-{}", write.id)),
                                        "Discard",
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            Self::store(cx).discard(&discard_id);
                                            this.notice =
                                                Some((Severity::Info, "Discarded.".into()));
                                            this.refresh(cx);
                                        },
                                    )),
                                ),
                        )
                })),
        )
    }
}

impl Focusable for MemoryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MemoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let approval = AppState::global(cx).read(cx).config.memory_write_approval;
        let dir = Self::store(cx).dir().to_path_buf();
        v_flex()
            .id("memory")
            .size_full()
            .p_6()
            .gap_4()
            .bg(colors.panel_background)
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .vertical_scrollbar_for(&self.scroll, window, cx)
            .child(Headline::new("Memory").size(HeadlineSize::Large))
            .child(
                Label::new("What the agent remembers across sessions. New sessions start with these entries in their prompt; changes apply from the next session.")
                    .color(Color::Muted),
            )
            .child(
                Checkbox::new(
                    "write-approval",
                    if approval { ToggleState::Selected } else { ToggleState::Unselected },
                )
                .label("Ask before the agent saves a memory")
                .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                    this.set_approval(*state == ToggleState::Selected, cx)
                })),
            )
            .when_some(self.notice.clone(), |this, (severity, text)| {
                this.child(Callout::new().severity(severity).title(text))
            })
            .children(self.render_pending(cx))
            .child(self.render_store(Target::User, cx))
            .child(Divider::horizontal())
            .child(self.render_store(Target::Memory, cx))
            .child(Divider::horizontal())
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("open-memory-folder", "Open folder")
                            .start_icon(Icon::new(IconName::Folder).size(IconSize::Small))
                            .tooltip(Tooltip::text(dir.display().to_string()))
                            .on_click(move |_, _, _| {
                                let _ = std::fs::create_dir_all(&dir);
                                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                            }),
                    )
                    .when(hermes_memories().is_some(), |this| {
                        this.child(
                            Button::new("import-hermes", "Import from Hermes")
                                .start_icon(Icon::new(IconName::Download).size(IconSize::Small))
                                .tooltip(Tooltip::text("Copy the entries of Hermes's MEMORY.md and USER.md here"))
                                .on_click(cx.listener(|this, _, _, cx| this.import_from_hermes(cx))),
                        )
                    }),
            )
    }
}
