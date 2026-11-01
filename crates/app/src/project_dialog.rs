//! The new/edit project dialog, after Hermes's `ProjectDialog`
//! (`apps/desktop/src/app/chat/sidebar/project-dialog.tsx`): a name and the folders the project
//! spans, picked with the Windows folder dialog. Picking a folder with no name typed names the
//! project after it, so one pick and Create is enough. The first folder is the primary one.

use std::path::PathBuf;

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    PathPromptOptions, Subscription, Window, prelude::*,
};
use hermes_store::Project;
use ui::{TintColor, Tooltip, prelude::*};

use crate::backend::backend;
use crate::sidebar::folder_name;
use crate::text_area::{TextArea, TextAreaEvent};

pub enum ProjectDialogEvent {
    /// The project was created or saved; carries its id.
    Saved(String),
}

pub struct ProjectDialog {
    /// The project being edited; `None` creates one.
    editing: Option<String>,
    name: Entity<TextArea>,
    folders: Vec<String>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProjectDialogEvent> for ProjectDialog {}
impl EventEmitter<DismissEvent> for ProjectDialog {}

impl ProjectDialog {
    /// A dialog for `project`, or for a new project starting with `folders` and `name`.
    pub fn new(
        project: Option<&Project>,
        name: String,
        folders: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_field = cx.new(|cx| {
            let mut area = TextArea::new("Project name", cx).single_line();
            area.set_text(project.map(|p| p.name.clone()).unwrap_or(name), cx);
            area
        });
        name_field.focus_handle(cx).focus(window, cx);
        let subscriptions = vec![cx.subscribe(&name_field, |this: &mut Self, _, event, cx| {
            if let TextAreaEvent::Submit = event {
                this.save(cx);
            }
        })];
        Self {
            editing: project.map(|p| p.id.clone()),
            name: name_field,
            folders: project.map(|p| p.folders.clone()).unwrap_or(folders),
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Opens the Windows folder dialog; every folder picked joins the project.
    fn add_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add folders to the project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update(cx, |this, cx| this.push_folders(paths, cx))
                .ok();
        })
        .detach();
    }

    fn push_folders(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in &paths {
            let folder = path.display().to_string();
            if !self.folders.iter().any(|f| f.eq_ignore_ascii_case(&folder)) {
                self.folders.push(folder);
            }
        }
        if self.name.read(cx).text().trim().is_empty() {
            if let Some(first) = paths.first() {
                let name = folder_name(first);
                self.name.update(cx, |field, cx| field.set_text(name, cx));
            }
        }
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_string();
        if name.is_empty() {
            self.error = Some("Give the project a name.".into());
            cx.notify();
            return;
        }
        // A project owns sessions by folder, so it needs at least one (as in Hermes).
        if self.folders.is_empty() {
            self.error = Some("Add at least one folder.".into());
            cx.notify();
            return;
        }
        let backend = backend(cx);
        let store = backend.runtime.store();
        let result = match &self.editing {
            Some(id) => store
                .update_project(id, &name, &self.folders)
                .map(|()| id.clone()),
            None => store
                .create_project(&name, &self.folders)
                .map(|project| project.id),
        };
        match result {
            Ok(id) => {
                cx.emit(ProjectDialogEvent::Saved(id));
                cx.emit(DismissEvent);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn render_folder(
        &self,
        index: usize,
        folder: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let primary = index == 0;
        h_flex()
            .id(("project-folder", index))
            .group("project-folder")
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .rounded_md()
            .hover(|style| style.bg(colors.ghost_element_hover))
            .child(
                Icon::new(IconName::Folder)
                    .size(IconSize::Small)
                    .color(if primary { Color::Accent } else { Color::Muted }),
            )
            .child(
                Label::new(folder.to_string())
                    .size(LabelSize::Small)
                    .buffer_font(cx)
                    .truncate_start()
                    .flex_1(),
            )
            .when(primary, |this| {
                this.child(
                    Label::new("Primary")
                        .size(LabelSize::XSmall)
                        .color(Color::Accent),
                )
            })
            .when(!primary, |this| {
                this.child(
                    div().visible_on_hover("project-folder").child(
                        Button::new(("make-primary", index), "Make Primary")
                            .label_size(LabelSize::XSmall)
                            .tooltip(Tooltip::text("New sessions start in the primary folder"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let folder = this.folders.remove(index);
                                this.folders.insert(0, folder);
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(
                IconButton::new(("remove-folder", index), IconName::Close)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Remove Folder"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.folders.remove(index);
                        cx.notify();
                    })),
            )
    }
}

impl Focusable for ProjectDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

impl Render for ProjectDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = if self.editing.is_some() {
            "Edit Project"
        } else {
            "New Project"
        };
        let folders: Vec<AnyElement> = self
            .folders
            .clone()
            .iter()
            .enumerate()
            .map(|(index, folder)| self.render_folder(index, folder, cx).into_any_element())
            .collect();
        let colors = cx.theme().colors();
        v_flex()
            .id("project-dialog")
            .w(rems(32.))
            .max_w_full()
            .p_4()
            .gap_3()
            .elevation_3(cx)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(DismissEvent);
                    cx.stop_propagation();
                }
            }))
            .child(
                h_flex()
                    .justify_between()
                    .child(Headline::new(title).size(HeadlineSize::Small))
                    .child(
                        IconButton::new("close-project-dialog", IconName::Close)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Close"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                Label::new(
                    "A project groups the folders you work in. Its sessions start in the \
                     primary folder, and the agent is told about the others.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new("Name")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.editor_background)
                            .child(self.name.clone()),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                Label::new("Folders")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Button::new("add-folders", "Add Folders…")
                                    .style(ButtonStyle::Outlined)
                                    .label_size(LabelSize::Small)
                                    .start_icon(Icon::new(IconName::Plus).size(IconSize::XSmall))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_folders(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .p_1()
                            .min_h(px(64.))
                            .rounded_md()
                            .border_1()
                            .border_color(colors.border_variant)
                            .bg(colors.editor_background)
                            .when(folders.is_empty(), |this| {
                                this.items_center().justify_center().child(
                                    Label::new("No folders yet")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            })
                            .children(folders),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .child(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-project", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new(
                            "save-project",
                            if self.editing.is_some() {
                                "Save Project"
                            } else {
                                "Create Project"
                            },
                        )
                        .style(ButtonStyle::Tinted(TintColor::Accent))
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}
