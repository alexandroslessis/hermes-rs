//! The thread header: what the title bar shows while a thread is open. t3code's header is a
//! breadcrumb in the topbar, next to the panel toggles and the caption buttons (`app-thread-dark.png`:
//! the project's monogram and name, a slash, the thread's title), not a row of the thread area, so
//! this is the title bar's middle ([`crate::titlebar::TitleBar::header`]), and the panel toggles
//! it already has are the toggles (they live in the workspace: they work the right panel and the
//! drawer there, and show their shortcuts in their tooltips, from [`crate::keymap`]).
//!
//! What it shows, left to right:
//! - **The project** (monogram and name; its tooltip gives the thread's folder; a click starts a
//!   new thread in the project, as in t3code) and a slash.
//! - **The thread's title**, in place editable: double click (or the menu's "Rename thread") turns
//!   it into a field; Enter or leaving the field commits, Escape cancels, an empty title is refused
//!   with a warning and an unchanged one does nothing (t3code's `resolveRenameCommit`). A commit
//!   is the sidebar's rename: [`Op::Rename`] through the app model, so the two cannot disagree.
//!   A chevron beside it opens **the thread's actions menu** (rename, pin, branch from the latest
//!   reply, export as Markdown or HTML, copy the id, archive); a right click anywhere on the header
//!   opens the same menu. Entries that cannot be used now are disabled ([`menu_entries`]).
//! - **The git branch and status** of the thread's folder: the branch (or "detached HEAD"), ahead
//!   and behind arrows, `+N -M` for the uncommitted lines, a tooltip with the counts. Nothing for
//!   a folder that is not a repository; "folder missing" for one that is gone.
//! - **The model in use**, read only (the picker is Phase 5).
//!
//! The data ([`HeaderInfo`]) is read by the thread model in the background when the thread opens,
//! when a turn ends and when the window comes back to the front (at most every two seconds): never
//! from `render`, never per frame. The chips leave (the model first, then git) when the window leaves the header
//! no room ([`Chips`]); the title keeps at least 40 px and shows an ellipsis when it must.
//!
//! The actions go through the same seam as everything else: rename, pin and archive are [`Op`]s
//! of the [`AppModel`] (the backend's queue, optimistic list), branch is the turn footer's own
//! operation (`ThreadModel::branch_latest`: `Core::branch` through the latest reply), export is
//! `hermes_core::export` over the stored messages (`Backend::export`), written to the file a
//! save dialog picks.
//!
//! t3code: `apps/web/src/components/chat/ChatHeader.tsx` (the breadcrumb, `resolveRenameCommit`,
//! the rename field, the title's menu and its double click), `hooks/useThreadActionMenu.ts`,
//! `components/GitActionsControl.tsx` (the branch and change counts). References:
//! `app-thread-dark.png` (the breadcrumb), `app-thread-options.png` (the menu),
//! `sidebar-thread-details.png` (branch and "Changes +N -M"), `button-sidebar_toggle-hover.png`
//! (the toggles). Gallery story: `thread-header`.
//!
//! Not copied: the menu opens from a chevron that is always visible (t3code's appears on hover and
//! the whole title is the button); the git state is a chip in the bar (t3code has a details card);
//! the parent-thread crumb and the guest-share indicator (we have neither); the menu lacks
//! "Find in thread", "Settle", "Snooze", "Regenerate title", "Mark unread", "Project settings" and
//! "Delete" (the sidebar's confirmed flow is the way to delete).

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, EventEmitter,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    assets::IconName,
    component::{
        Icon, Sizable as _, Size as KitSize,
        button::{Button as KitButton, ButtonVariants as _},
        input::{Input, InputEvent, InputState},
        menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use super::header_data::{
    Chips, Exported, GIT_CHIP_MAX, GitState, HeaderInfo, MODEL_CHIP_MIN, MenuAction, MenuEntry,
    MenuState, RenameOutcome, menu_entries, rename_outcome, title_min_width,
};
use super::model::{ThreadEvent, ThreadModel};
use crate::{
    design::{
        MiddleTruncate, Palette, ToastSpec, toast,
        tokens::{radius, text},
    },
    dev::ProbeExt as _,
    model::AppModel,
    sidebar::{color::ProjectColor, data::Op, monogram},
    theme::DesignTheme as _,
};

/// The widest the project crumb gets.
const PROJECT_MAX: f32 = 180.;
/// The widest the git and model chips get.
const CHIP_MAX: f32 = 160.;
/// The title field's width while renaming.
const FIELD_WIDTH: f32 = 260.;

gpui_kit::actions!(
    hermes_thread_header,
    [
        /// Escape in the title field: keep the old title.
        CancelRename
    ]
);

/// The key context of the header (active while the focus is inside it: the title field).
pub const CONTEXT: &str = "ThreadHeader";

/// Registers the header's key binding.
pub fn bind(cx: &mut App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        "escape",
        CancelRename,
        Some(CONTEXT),
    )]);
}

/// What the header tells the frame around it.
#[derive(Clone, Debug, PartialEq)]
pub enum HeaderEvent {
    /// A notice to show as a toast.
    Notice(ToastSpec),
    /// The title field closed: the frame takes the keyboard focus back.
    RenameEnded,
}

impl EventEmitter<HeaderEvent> for ThreadHeader {}

/// Where an export is saved: the user's save dialog, or (tests) a fixed answer.
#[derive(Clone)]
enum SavePicker {
    Dialog,
    Fixed(Option<PathBuf>),
}

struct Rename {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The open thread's header.
pub struct ThreadHeader {
    app: Entity<AppModel>,
    thread: Entity<ThreadModel>,
    rename: Option<Rename>,
    /// The width the header may use, set by the frame each frame ([`Chips::for_room`]).
    room: f32,
    /// The user's project colours (the sidebar's), set by the frame.
    colors: BTreeMap<String, String>,
    picker: SavePicker,
    _subscriptions: Vec<Subscription>,
}

impl ThreadHeader {
    /// The header of the thread `thread` is the model of, over the app model (title, project,
    /// pin).
    pub fn new(
        app: Entity<AppModel>,
        thread: Entity<ThreadModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.observe(&thread, |_, _, cx| cx.notify()),
            cx.subscribe_in(&thread, window, Self::on_thread_event),
            // The window coming forward: the folder may have changed while it was away.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.thread
                        .update(cx, |thread, cx| thread.refresh_header_if_stale(cx));
                }
            }),
        ];
        Self {
            app,
            thread,
            rename: None,
            room: f32::INFINITY,
            colors: BTreeMap::new(),
            picker: SavePicker::Dialog,
            _subscriptions: subscriptions,
        }
    }

    /// Saves exports to `path` (or nowhere: the dialog was cancelled) without asking (tests).
    pub fn set_save_path(&mut self, path: Option<PathBuf>) {
        self.picker = SavePicker::Fixed(path);
    }

    /// The thread model the header reads.
    pub fn thread(&self) -> &Entity<ThreadModel> {
        &self.thread
    }

    /// Tells the header how much width it has (the frame, every frame; no redraw is asked for:
    /// this is called while drawing).
    pub fn set_room(&mut self, room: f32) {
        self.room = room;
    }

    /// The width the frame said the header may use.
    pub fn room(&self) -> f32 {
        self.room
    }

    /// The user's project colours, as the sidebar keeps them (the frame, every frame; copied
    /// only when they differ).
    pub fn set_colors(&mut self, colors: &BTreeMap<String, String>) {
        if &self.colors != colors {
            self.colors = colors.clone();
        }
    }

    /// Whether the title field is open.
    pub fn renaming(&self) -> bool {
        self.rename.is_some()
    }

    /// The title field's state while renaming (tests type into it).
    pub fn rename_input(&self) -> Option<&Entity<InputState>> {
        self.rename.as_ref().map(|r| &r.input)
    }

    // --- what the thread is ------------------------------------------------------------------

    fn session(&self, cx: &App) -> String {
        self.thread.read(cx).session().to_string()
    }

    /// `(title as shown, project id, project name, pinned)` of the open thread.
    fn crumbs(&self, cx: &App) -> Option<Crumbs> {
        let session = self.session(cx);
        let app = self.app.read(cx);
        let snapshot = app.snapshot();
        let thread = snapshot.thread(&session)?;
        let project = snapshot.project(&thread.project);
        Some(Crumbs {
            title: thread.display_title().to_string(),
            project: thread.project.clone(),
            project_name: project.map_or_else(String::new, |p| p.name.clone()),
            pinned: thread.pinned,
        })
    }

    /// What decides which menu entries are on.
    pub fn menu_state(&self, cx: &App) -> MenuState {
        let thread = self.thread.read(cx);
        MenuState {
            archived: thread.header().is_some_and(|h| h.archived),
            pinned: self.crumbs(cx).is_some_and(|c| c.pinned),
            renaming: self.rename.is_some(),
            can_branch: thread.can_branch(),
            can_export: thread.has_messages() && !thread.exporting(),
        }
    }

    // --- renaming ----------------------------------------------------------------------------

    /// Turns the title into a field holding it.
    pub fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.is_some() {
            return;
        }
        let Some(crumbs) = self.crumbs(cx) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |input, cx| input.set_value(crumbs.title, window, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(window, cx),
                _ => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.rename = Some(Rename {
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Enter or leaving the field: renames the thread unless the title is empty (a warning) or
    /// unchanged.
    pub fn commit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let typed = rename.input.read(cx).value().to_string();
        if let Some(crumbs) = self.crumbs(cx) {
            match rename_outcome(&typed, &crumbs.title) {
                RenameOutcome::Commit(title) => {
                    let id = self.session(cx);
                    // The sidebar's rename: optimistic in the list, then the core's.
                    self.app
                        .update(cx, |app, cx| app.apply(Op::Rename { id, title }, cx));
                }
                RenameOutcome::RejectEmpty => cx.emit(HeaderEvent::Notice(ToastSpec::new(
                    toast::Kind::Warning,
                    "Thread title cannot be empty",
                ))),
                RenameOutcome::Noop => {}
            }
        }
        cx.emit(HeaderEvent::RenameEnded);
        cx.notify();
    }

    /// Escape in the field: the old title stays.
    pub fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            cx.emit(HeaderEvent::RenameEnded);
            cx.notify();
        }
    }

    // --- the actions menu --------------------------------------------------------------------

    /// Does a menu entry (an entry that is off does nothing, whoever asks).
    pub fn run(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        let allowed = menu_entries(&self.menu_state(cx))
            .iter()
            .any(|e| e.action() == Some(action) && e.enabled());
        if !allowed {
            return;
        }
        let id = self.session(cx);
        match action {
            MenuAction::Rename => self.start_rename(window, cx),
            MenuAction::TogglePin => {
                let pinned = self.crumbs(cx).is_some_and(|c| c.pinned);
                self.app.update(cx, |app, cx| {
                    app.apply(
                        Op::Pin {
                            id,
                            pinned: !pinned,
                        },
                        cx,
                    )
                });
            }
            MenuAction::Branch => self
                .thread
                .update(cx, |thread, cx| thread.branch_latest(cx)),
            MenuAction::Export(format) => self
                .thread
                .update(cx, |thread, cx| thread.export(format, cx)),
            MenuAction::CopyId => cx.write_to_clipboard(ClipboardItem::new_string(id)),
            MenuAction::Archive => {
                // The app model closes the thread; this header goes with it.
                self.app
                    .update(cx, |app, cx| app.apply(Op::Archive { id }, cx));
            }
        }
    }

    fn on_thread_event(
        &mut self,
        _: &Entity<ThreadModel>,
        event: &ThreadEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ThreadEvent::ExportReady(file) => self.save_export(file.clone(), cx),
            ThreadEvent::ExportFailed(why) => cx.emit(HeaderEvent::Notice(
                ToastSpec::new(toast::Kind::Error, "Could not export the thread")
                    .description(why.clone()),
            )),
            _ => {}
        }
    }

    /// Asks where to save the export and writes it there (off the UI thread).
    fn save_export(&mut self, file: Exported, cx: &mut Context<Self>) {
        match self.picker.clone() {
            SavePicker::Fixed(path) => self.write_export(file, path, cx),
            SavePicker::Dialog => {
                let folder = dirs::document_dir()
                    .or_else(dirs::home_dir)
                    .unwrap_or_else(|| PathBuf::from("."));
                let picked = cx.prompt_for_new_path(&folder, Some(&file.file_name));
                cx.spawn(async move |this, cx| {
                    let _ = match picked.await {
                        Ok(Ok(path)) => {
                            this.update(cx, |this, cx| this.write_export(file, path, cx))
                        }
                        // The dialog could not open (or went away): say so, write nothing.
                        Ok(Err(error)) => this.update(cx, |_, cx| {
                            cx.emit(HeaderEvent::Notice(
                                ToastSpec::new(
                                    toast::Kind::Error,
                                    "Could not open the save dialog",
                                )
                                .description(format!("{error:#}")),
                            ))
                        }),
                        Err(_) => Ok(()),
                    };
                })
                .detach();
            }
        }
    }

    /// Writes `file` to `path` in the background; `None`: the dialog was cancelled.
    pub fn write_export(&mut self, file: Exported, path: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(path) = path else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let target = path.clone();
            let written = cx
                .background_spawn(async move { write_whole(&target, file.content.as_bytes()) })
                .await;
            let _ = this.update(cx, |_, cx| {
                cx.emit(HeaderEvent::Notice(match written {
                    Ok(()) => ToastSpec::new(toast::Kind::Success, "Thread exported")
                        .description(path.display().to_string()),
                    Err(error) => ToastSpec::new(toast::Kind::Error, "Could not save the file")
                        .description(format!("{}: {error}", path.display())),
                }));
            });
        })
        .detach();
    }
}

/// Writes `bytes` to `path` whole or not at all: into a hidden file beside it, then renamed over
/// it (an existing file the save dialog agreed to replace is only replaced once the new one is
/// complete; a failed write leaves it as it was and removes the partial file).
pub fn write_whole(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".to_string());
    let partial = path.with_file_name(format!(".{name}.hermes-partial"));
    let written = std::fs::write(&partial, bytes).and_then(|()| std::fs::rename(&partial, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written
}

/// The open thread as the breadcrumb names it.
struct Crumbs {
    title: String,
    project: String,
    project_name: String,
    pinned: bool,
}

/// The menu's icon for an entry.
fn icon_of(action: MenuAction, pinned: bool) -> IconName {
    match action {
        MenuAction::Rename => IconName::Pencil,
        MenuAction::TogglePin if pinned => IconName::PinOff,
        MenuAction::TogglePin => IconName::Pin,
        MenuAction::Branch => IconName::GitBranch,
        MenuAction::Export(_) => IconName::Download,
        MenuAction::CopyId => IconName::Copy,
        MenuAction::Archive => IconName::Archive,
    }
}

/// Fills a popup menu with the thread's actions, as the state is when it opens.
fn build_menu(
    header: &Entity<ThreadHeader>,
    menu: PopupMenu,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let state = header.read(cx).menu_state(cx);
    menu_entries(&state)
        .into_iter()
        .fold(menu, |menu, entry| match entry {
            MenuEntry::Separator => menu.separator(),
            MenuEntry::Item {
                action,
                label,
                enabled,
            } => {
                let header = header.clone();
                menu.item(
                    PopupMenuItem::new(label)
                        .icon(Icon::new(icon_of(action, state.pinned)))
                        .disabled(!enabled)
                        .on_click(move |_, window, cx| {
                            header.update(cx, |h, cx| h.run(action, window, cx));
                        }),
                )
            }
        })
}

/// A small tooltip on a stateful element.
fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView {
    let text: SharedString = text.into();
    move |window, cx| crate::design::tooltip::view(text.clone(), None, window, cx)
}

/// A chip of the bar: a muted, borderless label with an optional icon, `CONTROL - 4` high.
fn chip(id: &'static str, palette: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(4.))
        .flex_shrink_0()
        .h(px(20.))
        .px(px(6.))
        .rounded(radius::MD)
        .text_size(text::XS.size)
        .line_height(text::XS.line_height)
        .text_color(palette.muted_foreground.hsla())
}

impl ThreadHeader {
    fn project_color(&self, crumbs: &Crumbs) -> ProjectColor {
        self.colors
            .get(&crumbs.project)
            .and_then(|name| ProjectColor::from_name(name))
            .unwrap_or_else(|| ProjectColor::for_name(&crumbs.project_name))
    }

    fn git_chip(&self, info: &HeaderInfo, palette: &Palette) -> Option<AnyElement> {
        match &info.git {
            // Not a repository (or no `git`): nothing to say.
            GitState::NoRepo => None,
            GitState::NoFolder => Some(
                chip("header-git", palette)
                    .text_color(palette.warning_foreground.hsla())
                    .tooltip(tip(format!("The folder {} is not on disk.", info.cwd)))
                    .child(Icon::new(IconName::TriangleAlert).with_size(KitSize::Size(px(12.))))
                    .child(div().id("label").child("folder missing").probe())
                    .probe()
                    .into_any_element(),
            ),
            GitState::Repo(git) => {
                let detached = git.detached || git.branch.is_none();
                let ink = if detached {
                    palette.warning_foreground
                } else {
                    palette.muted_foreground
                };
                let mut row =
                    chip("header-git", palette)
                        .text_color(ink.hsla())
                        // Shrinks down to its icon, arrows and counts (the branch name is
                        // the part that truncates), never below.
                        .flex_shrink_1()
                        .max_w(px(GIT_CHIP_MAX))
                        .tooltip(tip(git.tooltip()))
                        .child(div().flex_shrink_0().child(
                            Icon::new(IconName::GitBranch).with_size(KitSize::Size(px(12.))),
                        ))
                        .child(
                            div()
                                .id("branch")
                                .flex()
                                .min_w_0()
                                .max_w(px(CHIP_MAX))
                                .child(MiddleTruncate::new("name", git.branch_label()))
                                .probe(),
                        );
                if let Some(sync) = git.sync_label() {
                    row = row.child(div().id("sync").flex_shrink_0().child(sync).probe());
                }
                if let Some((added, removed)) = git.change_counts() {
                    row = row.child(
                        div()
                            .id("changes")
                            .flex()
                            .flex_shrink_0()
                            .gap(px(4.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(
                                div()
                                    .id("added")
                                    .text_color(palette.success_foreground.hsla())
                                    .child(added)
                                    .probe(),
                            )
                            .child(
                                div()
                                    .id("removed")
                                    .text_color(palette.error_foreground.hsla())
                                    .child(removed)
                                    .probe(),
                            )
                            .probe(),
                    );
                }
                Some(row.probe().into_any_element())
            }
        }
    }

    fn model_chip(&self, info: &HeaderInfo, palette: &Palette) -> Option<AnyElement> {
        let model = info.model_label()?;
        Some(
            chip("header-model", palette)
                .flex_shrink_1()
                .min_w(px(MODEL_CHIP_MIN))
                .max_w(px(CHIP_MAX))
                .tooltip(tip(info.model_tooltip()))
                .child(
                    div()
                        .flex_shrink_0()
                        .child(Icon::new(IconName::Bot).with_size(KitSize::Size(px(12.)))),
                )
                .child(
                    div()
                        .id("model-name")
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(model.to_string())
                        .probe(),
                )
                .probe()
                .into_any_element(),
        )
    }
}

impl Render for ThreadHeader {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        let appearance = cx.design_theme().appearance;
        let Some(crumbs) = self.crumbs(cx) else {
            return div().id("thread-header").into_any_element();
        };
        let this = cx.entity();
        let chips = Chips::for_room(self.room);
        let info = self.thread.read(cx).header().cloned();
        let folder = info.as_ref().map(|i| i.cwd.clone()).unwrap_or_default();

        // The project crumb: monogram and name; a click starts a thread in the project.
        let project = {
            let color = self.project_color(&crumbs);
            let project_id = crumbs.project.clone();
            let name = if crumbs.project_name.is_empty() {
                "No project".to_string()
            } else {
                crumbs.project_name.clone()
            };
            let hint = match folder.as_str() {
                "" => format!("New thread in {name}"),
                folder => format!("New thread in {name} · {folder}"),
            };
            let app = self.app.clone();
            div()
                .id("header-project")
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w_0()
                .max_w(px(PROJECT_MAX))
                .rounded(radius::SM)
                .cursor_pointer()
                .text_color(palette.muted_foreground.hsla())
                .hover(move |s| s.text_color(palette.foreground.hsla()))
                .tooltip(tip(hint))
                .on_click(move |_, _, cx| {
                    app.update(cx, |app, cx| app.new_thread(&project_id, cx));
                })
                .child(monogram(&name, color, appearance).probe())
                .child(
                    div()
                        .id("project-name")
                        .flex()
                        .min_w_0()
                        .child(MiddleTruncate::new("name", name))
                        .probe(),
                )
                .probe()
        };

        let separator = div()
            .id("header-sep")
            .flex_shrink_0()
            .text_color(palette.muted_foreground.hsla())
            .child("/")
            .probe();

        // The title: text with a chevron menu, or the field while renaming.
        let title: AnyElement = match &self.rename {
            Some(rename) => div()
                .id("header-title-field")
                .flex_1()
                .min_w(px(120.))
                .max_w(px(FIELD_WIDTH))
                .h(px(28.))
                .rounded(radius::SM)
                .border_1()
                .border_color(palette.ring.hsla())
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.foreground.hsla())
                .child(Input::new(&rename.input).appearance(false))
                .probe()
                .into_any_element(),
            None => {
                let menu_header = this.clone();
                let rename_header = this.clone();
                div()
                    .id("header-title-wrap")
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .flex_1()
                    // The title keeps a fair share before the project crumb gives up its width
                    // ([`title_min_width`]: CP4.3 review, a long title was cut to a bare "...").
                    .min_w(px(title_min_width(
                        &crumbs.title,
                        self.room - chips.reserved(),
                    )))
                    .child(
                        div()
                            .id("header-title")
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(palette.foreground.hsla())
                            .tooltip(tip(crumbs.title.clone()))
                            .on_click(move |event: &ClickEvent, window, cx| {
                                if event.click_count() == 2 {
                                    rename_header.update(cx, |h, cx| h.start_rename(window, cx));
                                }
                            })
                            .child(crumbs.title.clone())
                            .probe_text(crumbs.title),
                    )
                    .child(
                        div()
                            .id("header-actions-slot")
                            .flex_shrink_0()
                            .child(
                                KitButton::new("header-actions")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::ChevronDown)
                                    .accessibility_label("Thread actions")
                                    .dropdown_menu(move |menu, _, cx| {
                                        build_menu(&menu_header, menu, cx)
                                    }),
                            )
                            .probe(),
                    )
                    .probe()
                    .into_any_element()
            }
        };

        let context_header = this;
        let content = div()
            .id("thread-header-row")
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &CancelRename, _, cx| this.cancel_rename(cx)))
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .h_full()
            .pl(px(12.))
            .pr(px(8.))
            .child(project)
            .child(separator)
            .child(title)
            .when_some(
                chips
                    .git
                    .then(|| info.as_ref().and_then(|i| self.git_chip(i, &palette)))
                    .flatten(),
                |row, chip| row.child(chip),
            )
            .when_some(
                chips
                    .model
                    .then(|| info.as_ref().and_then(|i| self.model_chip(i, &palette)))
                    .flatten(),
                |row, chip| row.child(chip),
            )
            // A right click anywhere on the header opens the same menu as the chevron.
            .context_menu(move |menu, _, cx| build_menu(&context_header, menu, cx));
        div()
            .id("thread-header")
            .flex()
            .items_center()
            .min_w_0()
            .h_full()
            .child(content)
            .probe()
            .into_any_element()
    }
}
