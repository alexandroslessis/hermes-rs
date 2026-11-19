//! Drawing the sidebar: the header, the search row with its three buttons, the filter box, the
//! list (pinned threads, then each project as a folder header with its threads), the footer and
//! the "are you sure" card. Sizes are t3code's: a folder header is 32 px, a thread row 36 px with
//! its title lined up under the folder's name (36 px from the left), the status or the time at
//! the right, and on hover the row's actions (rename, archive, delete) over the right edge.
//!
//! t3code: `apps/web/src/components/sidebar/SidebarProjectFolderHeader.tsx` (the folder header:
//! `h-8 gap-2.5`, monogram, name, count and dot while closed, chevron, the new-thread button over
//! the count on hover), `sidebar/SidebarThreadRow.tsx` (the `folder` variant: `h-9 gap-2.5 px-2.5
//! ps-9`; title `text-sm`, `font-normal text-foreground/75` when the thread recedes
//! (`Sidebar.logic.ts` `shouldRecedeSidebarThread`), `font-medium` otherwise, end-truncated
//! (`truncate`); status or time `text-xs` in the right slot; the pin at the title's end), `sidebar/SidebarListMarkers.tsx`
//! (the "Pinned" boundary label), `Sidebar.tsx` (the search row, the footer), `ProjectFavicon.tsx`
//! (the monogram). References: `app-sidebar_closeup-dark.png` and `sidebar.png` (header, search
//! row with the filter, add-folder and new-thread buttons, the row rhythm, the status "Done"),
//! `button-session-variants-*.png` (rows and their hover), `buttons-sidebar-down.png` (the
//! footer), `app-sidebar_collapsed.png`. The grouped (folder) variant has no screenshot of its own
//! (t3code's screenshots show its flat card list): it follows the source above.
//!
//! Gallery stories: `sidebar`, `sidebar-more`, `empty-states`.

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, assets::IconName, component::Icon,
    component::Sizable as _, component::Size as KitSize, component::input::Input,
    component::menu::ContextMenuExt as _, component::menu::PopupMenuItem, div,
    prelude::FluentBuilder as _, px,
};

use super::{
    color::ProjectColor,
    data::{
        FolderDot, Group, NO_PROJECT, ProjectItem, ProjectKind, RowKey, RowStatus, Snapshot,
        ThreadItem, Tree, relative_time, status_of,
    },
    keys,
    view::{ConfirmKind, RenameTarget, Sidebar, SidebarEvent},
};
use crate::{
    design::{
        Button, Color, ListRow, Palette,
        button::{self, Variant, icon_button},
        list_row::{Status, row_fill},
        tokens::{Appearance, radius, text},
    },
    dev::ProbeExt as _,
    model::Selection,
    theme::DesignTheme as _,
    titlebar,
};

/// A folder header's height (`h-8`).
pub const FOLDER_HEIGHT: f32 = 32.;
/// A thread row's height (`h-9`).
pub const ROW_HEIGHT: f32 = 36.;
/// Where a thread's title starts inside a folder (`ps-9`).
pub const ROW_INSET: f32 = 36.;
/// The side padding of a row (`px-2.5`).
pub const ROW_PAD: f32 = 10.;
/// The left padding of Kit's (medium) input, which the rename field pulls back.
const RENAME_PAD: gpui_kit::Pixels = gpui_kit::px(10.);

/// What is dragged from a thread row.
#[derive(Clone)]
pub struct DragThread {
    pub id: String,
    pub title: SharedString,
}

/// What follows the pointer while a thread is dragged.
struct DragGhost {
    title: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.design_theme().palette;
        div()
            .id("drag-ghost")
            .max_w(px(220.))
            .px(px(10.))
            .py(px(6.))
            .rounded(radius::LG)
            .border_1()
            .border_color(palette.panel_edge.hsla())
            .bg(palette.panel_surface.over(palette.background).hsla())
            .text_color(palette.popover_foreground.hsla())
            .text_size(text::SM.size)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(self.title.clone())
    }
}

fn dom_id(prefix: &str, id: &str) -> SharedString {
    SharedString::from(format!("{prefix}-{id}"))
}

/// The monogram tile of a project: its first two letters on its colour.
pub fn monogram(name: &str, color: ProjectColor, appearance: Appearance) -> Stateful<Div> {
    monogram_with_id("monogram", name, color, appearance)
}

/// [`monogram`] with a name for the layout checks.
pub fn monogram_with_id(
    id: impl Into<gpui_kit::ElementId>,
    name: &str,
    color: ProjectColor,
    appearance: Appearance,
) -> Stateful<Div> {
    let letters = super::color::monogram(name);
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .size(px(16.))
        .rounded(px(4.))
        .bg(color.tile(appearance).hsla())
        .text_color(color.ink(appearance).hsla())
        .text_size(px(8.))
        .font_weight(FontWeight::BOLD)
        .child(letters)
}

fn status_of_row(status: RowStatus) -> Status {
    match status {
        RowStatus::Approval => Status::Approval,
        RowStatus::Working => Status::Working,
        RowStatus::Failed => Status::Failed,
        // t3code shows a finished thread nobody looked at as "Done".
        RowStatus::Unread => Status::Done,
    }
}

fn dot_color(dot: FolderDot, palette: &Palette) -> Color {
    match dot {
        FolderDot::Attention => palette.warning,
        FolderDot::Failed => palette.error,
        FolderDot::Unread => palette.success,
        FolderDot::Working => palette.info,
    }
}

/// Everything a row needs that is the same for the whole frame.
struct Paint {
    palette: Palette,
    appearance: Appearance,
    now_ms: i64,
    selected: Selection,
    /// The keyboard is in the sidebar and was last used: draw the cursor.
    cursor: Option<RowKey>,
}

impl Sidebar {
    fn color_of(&self, project: &ProjectItem) -> ProjectColor {
        self.prefs
            .colors
            .get(&project.id)
            .and_then(|name| ProjectColor::from_name(name))
            .unwrap_or_else(|| ProjectColor::for_name(&project.name))
    }

    fn renaming_thread(
        &self,
        id: &str,
    ) -> Option<&gpui_kit::Entity<gpui_kit::component::input::InputState>> {
        self.rename.as_ref().and_then(|r| match &r.target {
            RenameTarget::Thread(t) if t == id => Some(&r.input),
            _ => None,
        })
    }

    fn renaming_project(
        &self,
        id: &str,
    ) -> Option<&gpui_kit::Entity<gpui_kit::component::input::InputState>> {
        self.rename.as_ref().and_then(|r| match &r.target {
            RenameTarget::Project(p) if p == id => Some(&r.input),
            _ => None,
        })
    }

    /// The ring of the row the keyboard is on.
    fn cursor_ring(palette: &Palette) -> Div {
        div()
            .absolute()
            .inset_0()
            .rounded(radius::MD)
            .border_2()
            .border_color(palette.ring.hsla())
    }

    // --- rows ---------------------------------------------------------------------------------

    fn thread_row(
        &self,
        thread: &ThreadItem,
        project: Option<&ProjectItem>,
        in_folder: bool,
        paint: &Paint,
        snapshot: &Snapshot,
        marks: &crate::sidebar::data::Marks<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = &paint.palette;
        let id = thread.id.clone();
        let open = paint.selected == Selection::Thread(id.clone());
        let cursor = paint.cursor == Some(RowKey::Thread(id.clone()));
        let status = status_of(thread, marks);
        // t3code's `shouldRecedeSidebarThread`: a thread nobody needs to look at sits back
        // (`font-normal`, 75%): at rest and seen, or working; the open one, a question, an unseen
        // finish and a failure stand out (`font-medium`, full colour). Static Inter has no 500
        // (MEDIUM draws Regular), so standing out is SEMIBOLD, as `ListRow`'s title.
        let recede = !open && matches!(status, None | Some(RowStatus::Working));
        let group = dom_id("row", &id);
        let hover = palette.sidebar_row_hover.over(palette.sidebar);
        let fill = row_fill(palette, open, false);
        let actions_bg = fill.map_or(hover, |f| f.over(palette.sidebar));
        let title_color = if recede {
            palette.sidebar_foreground.with_alpha(191)
        } else {
            palette.sidebar_foreground
        };
        let full = palette.sidebar_foreground;
        let sidebar = cx.entity();

        let title: AnyElement = match self.renaming_thread(&id) {
            Some(input) => div()
                .id(dom_id("rename", &id))
                .flex_1()
                .min_w_0()
                // The field's own padding pulled back so the text stays where the name was
                // (t3code's rename input: `-mx-1 px-1`).
                .ml(-RENAME_PAD)
                .child(Input::new(input).appearance(false))
                .probe()
                .into_any_element(),
            None => div()
                .id(dom_id("title", &id))
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .when(!recede, |d| d.font_weight(FontWeight::SEMIBOLD))
                .text_color(title_color.hsla())
                .group_hover(group.clone(), move |s| s.text_color(full.hsla()))
                .child(thread.display_title().to_string())
                .probe_text(thread.display_title().to_string())
                .into_any_element(),
        };

        let slot = match status {
            Some(status) => {
                let status = status_of_row(status);
                let color = status.color(palette).hsla();
                div()
                    .id(dom_id("status", &id))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(text::XS.size)
                    .line_height(text::XS.line_height)
                    .text_color(color)
                    .child(
                        div()
                            .id(dom_id("status-icon", &id))
                            .flex_shrink_0()
                            .child(
                                Icon::new(status.icon())
                                    .with_size(KitSize::Size(px(12.)))
                                    .text_color(color),
                            )
                            .probe(),
                    )
                    .child(
                        div()
                            .id(dom_id("status-word", &id))
                            .font_weight(FontWeight::MEDIUM)
                            .child(status.label())
                            .probe_text(status.label()),
                    )
                    .probe()
                    .into_any_element()
            }
            None => {
                let time = relative_time(paint.now_ms, thread.updated_ms);
                div()
                    .id(dom_id("time", &id))
                    .text_size(text::XS.size)
                    .line_height(text::XS.line_height)
                    .text_color(palette.sidebar_secondary_label.hsla())
                    .child(time.clone())
                    .probe_text(time)
                    .into_any_element()
            }
        };

        let actions = div()
            .id(dom_id("actions", &id))
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .flex()
            .items_center()
            .gap(px(2.))
            .pl(px(10.))
            .pr(px(6.))
            .bg(actions_bg.hsla())
            .opacity(if cursor { 1. } else { 0. })
            .group_hover(group.clone(), |s| s.opacity(1.))
            .child(self.row_button("rename", IconName::Pencil, "Rename (F2)", {
                let id = id.clone();
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.start_rename_thread(&id, window, cx);
                })
            }))
            .child(self.row_button("archive", IconName::Archive, "Archive", {
                let id = id.clone();
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.archive(&id, cx);
                })
            }))
            .child(self.row_button("delete", IconName::Trash, "Delete (Del)", {
                let id = id.clone();
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.request_delete(&id, cx);
                })
            }))
            .probe();

        let dragged = DragThread {
            id: id.clone(),
            title: thread.display_title().to_string().into(),
        };
        let move_targets: Vec<(String, String)> = snapshot
            .projects
            .iter()
            .filter(|p| p.path.is_some() && p.id != thread.project)
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let pinned = thread.pinned;
        let menu_sidebar = sidebar;
        let menu_id = id.clone();

        let row = div()
            .id("row")
            .group(group)
            .relative()
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(10.))
            .h(px(ROW_HEIGHT))
            .pl(px(if in_folder { ROW_INSET } else { ROW_PAD }))
            .pr(px(ROW_PAD))
            .rounded(radius::MD)
            .overflow_hidden()
            .cursor_pointer()
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .text_color(palette.sidebar_foreground.hsla())
            .when_some(fill, |d, f| d.bg(f.hsla()))
            .when(!open, |d| d.hover(move |s| s.bg(hover.hsla())))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.focus(window, cx);
                        this.cursor = Some(RowKey::Thread(id.clone()));
                        cx.notify();
                    }
                }),
            )
            .on_click(cx.listener({
                let id = id.clone();
                move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.start_rename_thread(&id, window, cx);
                    } else {
                        this.open_thread(&id, cx);
                    }
                }
            }))
            .on_drag(dragged, |drag, _, _, cx| {
                cx.new(|_| DragGhost {
                    title: drag.title.clone(),
                })
            })
            .when(!in_folder, |d| {
                d.children(project.map(|p| {
                    monogram_with_id(
                        dom_id("monogram", &id),
                        &p.name,
                        self.color_of(p),
                        paint.appearance,
                    )
                    .opacity(if open { 1. } else { 0.7 })
                    .probe()
                }))
            })
            .child(title)
            .when(pinned, |d| {
                d.child(
                    div()
                        .id(dom_id("pin", &id))
                        .flex_shrink_0()
                        .child(
                            Icon::new(IconName::Pin)
                                .with_size(KitSize::Size(px(12.)))
                                .text_color(
                                    palette.sidebar_muted_foreground.with_alpha(166).hsla(),
                                ),
                        )
                        .probe(),
                )
            })
            .child(
                div()
                    .id(dom_id("slot", &id))
                    .flex()
                    .items_center()
                    .justify_end()
                    .flex_shrink_0()
                    .min_w(px(32.))
                    .h(px(24.))
                    .opacity(if cursor { 0. } else { 1. })
                    .group_hover(dom_id("row", &id), |s| s.opacity(0.))
                    .child(slot)
                    .probe(),
            )
            .child(actions)
            .when(cursor, |d| d.child(Self::cursor_ring(palette)));

        let row = row.context_menu(move |menu, window, cx| {
            let act = |sidebar: &gpui_kit::Entity<Sidebar>,
                       f: fn(&mut Sidebar, &str, &mut Window, &mut Context<Sidebar>),
                       id: &str| {
                let (sidebar, id) = (sidebar.clone(), id.to_string());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    sidebar.update(cx, |s, cx| f(s, &id, window, cx));
                }
            };
            let mut menu = menu
                .item(
                    PopupMenuItem::new("Rename")
                        .icon(Icon::new(IconName::Pencil))
                        .on_click(act(&menu_sidebar, Sidebar::start_rename_thread, &menu_id)),
                )
                .item(
                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" })
                        .icon(Icon::new(if pinned {
                            IconName::PinOff
                        } else {
                            IconName::Pin
                        }))
                        .on_click(act(
                            &menu_sidebar,
                            |s, id, _, cx| s.toggle_pin(id, cx),
                            &menu_id,
                        )),
                )
                .item(
                    PopupMenuItem::new("Archive")
                        .icon(Icon::new(IconName::Archive))
                        .on_click(act(
                            &menu_sidebar,
                            |s, id, _, cx| s.archive(id, cx),
                            &menu_id,
                        )),
                );
            if !move_targets.is_empty() {
                let (sidebar, id, targets) =
                    (menu_sidebar.clone(), menu_id.clone(), move_targets.clone());
                menu = menu.submenu_with_icon(
                    Some(Icon::new(IconName::FolderOpen)),
                    "Move to project",
                    window,
                    cx,
                    move |m, _, _| {
                        targets.iter().fold(m, |m, (project, name)| {
                            let (sidebar, id, project) =
                                (sidebar.clone(), id.clone(), project.clone());
                            m.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                                sidebar.update(cx, |s, cx| s.move_to_project(&id, &project, cx));
                            }))
                        })
                    },
                );
            }
            menu.separator()
                .item(
                    PopupMenuItem::new("Copy thread ID")
                        .icon(Icon::new(IconName::Copy))
                        .on_click(act(
                            &menu_sidebar,
                            |s, id, _, cx| s.copy_thread_id(id, cx),
                            &menu_id,
                        )),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Delete…")
                        .icon(Icon::new(IconName::Trash))
                        .on_click(act(
                            &menu_sidebar,
                            |s, id, _, cx| s.request_delete(id, cx),
                            &menu_id,
                        )),
                )
        });
        // The named element is the outer one (a context menu wraps the row).
        div()
            .id(dom_id("thread", &id))
            .flex_shrink_0()
            .w_full()
            .child(row)
            .probe()
            .into_any_element()
    }

    /// One of a row's hover buttons (20 px, muted ghost).
    fn row_button(
        &self,
        id: &'static str,
        icon: IconName,
        tip: &'static str,
        on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Button {
        icon_button(id, icon, tip)
            .variant(Variant::GhostMuted)
            .size(button::Size::Micro)
            .on_click(on_click)
    }

    fn folder_header(
        &self,
        project: &ProjectItem,
        group: &Group,
        first: bool,
        paint: &Paint,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = &paint.palette;
        let id = project.id.clone();
        let cursor = paint.cursor == Some(RowKey::Folder(id.clone()));
        let hover = palette.sidebar_row_hover.hsla();
        let user = project.kind == ProjectKind::User;
        let droppable = project.path.is_some();
        let color = self.color_of(project);
        let group_name = dom_id("folder-group", &id);
        let name: AnyElement = match self.renaming_project(&id) {
            Some(input) => div()
                .id(dom_id("rename", &id))
                .flex_1()
                .min_w_0()
                // The field's own padding pulled back so the text stays where the name was
                // (t3code's rename input: `-mx-1 px-1`).
                .ml(-RENAME_PAD)
                .child(Input::new(input).appearance(false))
                .probe()
                .into_any_element(),
            None => div()
                .id(dom_id("name", &id))
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.sidebar_foreground.with_alpha(230).hsla())
                .child(project.name.clone())
                .probe_text(project.name.clone())
                .into_any_element(),
        };
        let count = (group.collapsed && group.total > 0).then(|| {
            div()
                .id(dom_id("count", &id))
                .flex_shrink_0()
                .text_size(text::XS.size)
                .line_height(text::XS.line_height)
                .text_color(palette.sidebar_muted_foreground.hsla())
                .group_hover(group_name.clone(), |s| s.opacity(0.))
                .child(group.total.to_string())
                .probe()
        });
        let dot = group.dot.map(|dot| {
            div()
                .id(dom_id("dot", &id))
                .flex_shrink_0()
                .size(px(6.))
                .rounded_full()
                .bg(dot_color(dot, palette).hsla())
                .probe()
        });
        let chevron = Icon::new(if group.collapsed {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        })
        .with_size(KitSize::Size(px(12.)))
        .text_color(palette.sidebar_muted_foreground.hsla());
        let new_thread = div()
            .id(dom_id("new-thread-slot", &id))
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(24.))
            .flex()
            .items_center()
            .opacity(if cursor { 1. } else { 0. })
            .group_hover(group_name.clone(), |s| s.opacity(1.))
            .child({
                let id = id.clone();
                icon_button(
                    "new-thread",
                    IconName::SquarePen,
                    format!("New thread in {}", project.name),
                )
                .variant(Variant::GhostMuted)
                .size(button::Size::Xs)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.new_thread(&id, cx);
                }))
            })
            .probe();
        let sidebar = cx.entity();
        let menu_id = id.clone();
        let colors: Vec<(&'static str, &'static str)> = ProjectColor::ALL
            .iter()
            .map(|c| (c.name(), c.label()))
            .collect();
        let current_color = color.name();
        let tooltip = project.path.clone();

        let on_drop = {
            let pid = id.clone();
            cx.listener(move |this, drag: &DragThread, _, cx| {
                this.move_to_project(&drag.id, &pid, cx);
            })
        };
        let header = div()
            .id("header")
            .group(group_name)
            .relative()
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(10.))
            .h(px(FOLDER_HEIGHT))
            .px(px(ROW_PAD))
            .rounded(radius::MD)
            // `text-sm` (the name inherited the sidebar's larger size before the review).
            .text_size(text::SM.size)
            .line_height(text::SM.line_height)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .when(droppable, |d| {
                let primary = palette.primary;
                d.drag_over::<DragThread>(move |s, _, _, _| s.bg(primary.with_alpha(26).hsla()))
                    .on_drop(on_drop)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.focus(window, cx);
                        this.cursor = Some(RowKey::Folder(id.clone()));
                        cx.notify();
                    }
                }),
            )
            .on_click(cx.listener({
                let id = id.clone();
                move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    if event.click_count() >= 2 && user {
                        this.start_rename_project(&id, window, cx);
                    } else {
                        this.toggle_folder(&id, cx);
                    }
                }
            }))
            .when_some(tooltip, |d, path| {
                d.tooltip(move |window, cx| {
                    crate::design::tooltip::view(path.clone().into(), None, window, cx)
                })
            })
            .child(match project.kind {
                ProjectKind::None => div()
                    .id(dom_id("monogram", &id))
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .size(px(16.))
                    .child(
                        Icon::new(IconName::MessageSquare)
                            .with_size(KitSize::Size(px(14.)))
                            .text_color(palette.sidebar_muted_foreground.hsla()),
                    )
                    .probe(),
                _ => monogram_with_id(
                    dom_id("monogram", &id),
                    &project.name,
                    color,
                    paint.appearance,
                )
                .probe(),
            })
            .child(name)
            .children(dot)
            .children(count)
            .child(
                div()
                    .id(dom_id("chevron", &id))
                    .flex_shrink_0()
                    .child(chevron)
                    .probe(),
            )
            .child(new_thread)
            .when(cursor, |d| d.child(Self::cursor_ring(palette)));

        let header = header.context_menu(move |menu, window, cx| {
            let on = |sidebar: &gpui_kit::Entity<Sidebar>,
                      f: fn(&mut Sidebar, &str, &mut Window, &mut Context<Sidebar>),
                      id: &str| {
                let (sidebar, id) = (sidebar.clone(), id.to_string());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    sidebar.update(cx, |s, cx| f(s, &id, window, cx));
                }
            };
            let mut menu = menu.item(
                PopupMenuItem::new("New thread")
                    .icon(Icon::new(IconName::SquarePen))
                    .on_click(on(&sidebar, |s, id, _, cx| s.new_thread(id, cx), &menu_id)),
            );
            if user || menu_id != NO_PROJECT {
                menu = menu.item(
                    PopupMenuItem::new("Copy path")
                        .icon(Icon::new(IconName::Copy))
                        .on_click(on(
                            &sidebar,
                            |s, id, _, cx| s.copy_project_path(id, cx),
                            &menu_id,
                        )),
                );
            }
            if user {
                menu = menu.item(
                    PopupMenuItem::new("Rename project")
                        .icon(Icon::new(IconName::Pencil))
                        .on_click(on(&sidebar, Sidebar::start_rename_project, &menu_id)),
                );
                let (sidebar2, id2, colors2) = (sidebar.clone(), menu_id.clone(), colors.clone());
                menu = menu.submenu_with_icon(
                    Some(Icon::new(IconName::Palette)),
                    "Color",
                    window,
                    cx,
                    move |m, _, _| {
                        colors2.iter().fold(m, |m, (name, label)| {
                            let (sidebar, id, name) = (sidebar2.clone(), id2.clone(), *name);
                            m.item(
                                PopupMenuItem::new(*label)
                                    .checked(name == current_color)
                                    .on_click(move |_, _, cx| {
                                        sidebar.update(cx, |s, cx| {
                                            s.set_project_color(&id, Some(name), cx)
                                        });
                                    }),
                            )
                        })
                    },
                );
                menu = menu.separator().item(
                    PopupMenuItem::new("Remove project…")
                        .icon(Icon::new(IconName::Trash))
                        .on_click(on(
                            &sidebar,
                            |s, id, _, cx| s.request_remove_project(id, cx),
                            &menu_id,
                        )),
                );
            }
            menu
        });
        div()
            .id(dom_id("folder", &id))
            .flex_shrink_0()
            .w_full()
            .when(!first, |d| d.mt(px(8.)))
            .child(header)
            .probe()
            .into_any_element()
    }

    fn more_row(
        &self,
        project: &str,
        hidden: usize,
        paint: &Paint,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = &paint.palette;
        let cursor = paint.cursor == Some(RowKey::More(project.to_string()));
        let hover = palette.sidebar_row_hover.hsla();
        let id = project.to_string();
        div()
            .id(dom_id("more", project))
            .relative()
            .flex()
            .items_center()
            .flex_shrink_0()
            .h(px(28.))
            .pl(px(ROW_INSET))
            .pr(px(ROW_PAD))
            .rounded(radius::MD)
            .cursor_pointer()
            .text_size(text::XS.size)
            .line_height(text::XS.line_height)
            .text_color(palette.sidebar_muted_foreground.hsla())
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| this.show_more(&id, cx)))
            .child(format!("Show {hidden} more"))
            .when(cursor, |d| d.child(Self::cursor_ring(palette)))
            .probe()
            .into_any_element()
    }

    fn note_row(&self, id: SharedString, text: String, paint: &Paint) -> AnyElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .flex_shrink_0()
            .h(px(28.))
            .pl(px(ROW_INSET))
            .text_size(text::XS.size)
            .line_height(text::XS.line_height)
            .text_color(
                paint
                    .palette
                    .sidebar_muted_foreground
                    .with_alpha(166)
                    .hsla(),
            )
            .child(text)
            .probe()
            .into_any_element()
    }

    fn pinned_label(&self, paint: &Paint) -> AnyElement {
        let palette = &paint.palette;
        div()
            .id("pinned-label")
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(8.))
            .h(px(24.))
            .px(px(ROW_PAD))
            .text_size(text::XS.size)
            .line_height(text::XS.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(palette.sidebar_foreground.with_alpha(204).hsla())
            .child("Pinned")
            .child(
                div()
                    .h(px(1.))
                    .flex_1()
                    .bg(palette.sidebar_foreground.with_alpha(64).hsla()),
            )
            .probe()
            .into_any_element()
    }

    // --- regions ------------------------------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("sidebar-header")
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(8.))
            .h(px(titlebar::HEIGHT))
            .px(px(10.))
            .child(
                icon_button(
                    "toggle-sidebar",
                    IconName::PanelLeft,
                    "Toggle sidebar (Ctrl+B)",
                )
                .variant(Variant::Ghost)
                .size(button::Size::Sm)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::ToggleSidebar))),
            )
            .child(
                div()
                    .id("brand")
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Hermes"),
            )
            .child(
                div()
                    .id("sidebar-drag")
                    .flex_1()
                    .h_full()
                    .window_control_area(gpui_kit::WindowControlArea::Drag),
            )
            .probe()
            .into_any_element()
    }

    /// The fade over the top or the bottom edge of the list (12 px, the sidebar's colour).
    fn fade(sidebar: Color, bottom: bool) -> Div {
        let (from, to) = (sidebar.with_alpha(0).hsla(), sidebar.hsla());
        div()
            .absolute()
            .left_0()
            .right_0()
            .h(px(12.))
            .map(|d| if bottom { d.bottom_0() } else { d.top_0() })
            .bg(if bottom {
                gpui_kit::linear_gradient(
                    180.,
                    gpui_kit::linear_color_stop(from, 0.),
                    gpui_kit::linear_color_stop(to, 1.),
                )
            } else {
                gpui_kit::linear_gradient(
                    180.,
                    gpui_kit::linear_color_stop(to, 0.),
                    gpui_kit::linear_color_stop(from, 1.),
                )
            })
    }

    fn search_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let filter_open = self.filter.is_some();
        div()
            .id("sidebar-search")
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(4.))
            .px(px(8.))
            .pb(px(8.))
            .child(div().flex_1().min_w_0().child({
                let this = cx.entity();
                ListRow::new("search", "Search")
                    .icon(IconName::Search)
                    .on_click(move |_, cx| {
                        this.update(cx, |_, cx| cx.emit(SidebarEvent::OpenPalette));
                    })
            }))
            .child(
                icon_button("filter", IconName::ListFilter, "Filter threads (Ctrl+F)")
                    .variant(Variant::GhostMuted)
                    .size(button::Size::Sm)
                    .selected(filter_open)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_filter(window, cx))),
            )
            .child(
                icon_button("add-project", IconName::FolderPlus, "Add project")
                    .variant(Variant::GhostMuted)
                    .size(button::Size::Sm)
                    .on_click(cx.listener(|this, _, _, cx| this.add_project_dialog(cx))),
            )
            .child(
                icon_button("new-thread", IconName::SquarePen, "New thread (Ctrl+N)")
                    .variant(Variant::GhostMuted)
                    .size(button::Size::Sm)
                    .on_click(cx.listener(|this, _, _, cx| this.new_thread_here(cx))),
            )
            .probe()
            .into_any_element()
    }

    fn filter_box(&self, paint: &Paint, cx: &mut Context<Self>) -> Option<AnyElement> {
        let filter = self.filter.as_ref()?;
        let palette = &paint.palette;
        Some(
            div()
                .id("filter-row")
                .flex_shrink_0()
                .px(px(8.))
                .pb(px(8.))
                .child(
                    div()
                        .id("filter-box")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(32.))
                        .px(px(10.))
                        .rounded(radius::LG)
                        .border_1()
                        .border_color(palette.sidebar_input.hsla())
                        .child(
                            Icon::new(IconName::Search)
                                .with_size(KitSize::Size(px(14.)))
                                .text_color(palette.sidebar_muted_foreground.hsla()),
                        )
                        .child(
                            div()
                                .id("filter-input")
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&filter.input).appearance(false))
                                .probe(),
                        )
                        .child(
                            icon_button("filter-clear", IconName::Close, "Clear filter (Esc)")
                                .variant(Variant::GhostMuted)
                                .size(button::Size::Micro)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_filter(window, cx)
                                    }),
                                ),
                        )
                        .probe(),
                )
                .probe()
                .into_any_element(),
        )
    }

    fn footer(&self, paint: &Paint, cx: &mut Context<Self>) -> AnyElement {
        let dark = paint.appearance.is_dark();
        div()
            .id("sidebar-footer")
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(4.))
            .h(px(titlebar::HEIGHT))
            .px(px(10.))
            .child(
                icon_button("settings", IconName::Settings, "Settings (Ctrl+,)")
                    .variant(Variant::GhostMuted)
                    .size(button::Size::Sm)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::OpenSettings))),
            )
            .child(
                icon_button("usage", IconName::ChartColumn, "Usage")
                    .variant(Variant::GhostMuted)
                    .size(button::Size::Sm)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::OpenUsage))),
            )
            .child(div().flex_1())
            .child(
                icon_button(
                    "theme",
                    if dark { IconName::Sun } else { IconName::Moon },
                    if dark { "Light theme" } else { "Dark theme" },
                )
                .variant(Variant::GhostMuted)
                .size(button::Size::Sm)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::ToggleTheme))),
            )
            .probe()
            .into_any_element()
    }

    /// The question over the list: delete a thread, remove a project.
    fn confirm_card(&self, paint: &Paint, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.confirm.as_ref()?;
        let palette = &paint.palette;
        let danger = matches!(confirm.kind, ConfirmKind::DeleteThread(_));
        Some(
            div()
                .id("confirm-layer")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(12.))
                .bg(palette.sidebar.with_alpha(204).hsla())
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.cancel_confirm(cx)),
                )
                .child(
                    div()
                        .id("confirm")
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .w_full()
                        .p(px(16.))
                        .rounded(radius::XL)
                        .border_1()
                        .border_color(palette.panel_edge.hsla())
                        .bg(palette.panel_surface.over(palette.background).hsla())
                        .text_color(palette.popover_foreground.hsla())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .id("confirm-title")
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(confirm.title.clone())
                                .probe(),
                        )
                        .child(
                            div()
                                .id("confirm-detail")
                                .text_size(text::XS.size)
                                .line_height(text::XS.line_height)
                                .text_color(palette.muted_foreground.hsla())
                                .child(confirm.detail.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    Button::new("confirm-cancel", "Cancel")
                                        .variant(Variant::Outline)
                                        .size(button::Size::Sm)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.cancel_confirm(cx)),
                                        ),
                                )
                                .child(
                                    Button::new("confirm-accept", confirm.accept)
                                        .variant(if danger {
                                            Variant::Destructive
                                        } else {
                                            Variant::Default
                                        })
                                        .size(button::Size::Sm)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.accept_confirm(cx)),
                                        ),
                                ),
                        )
                        .probe(),
                )
                .probe()
                .into_any_element(),
        )
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.design_theme();
        let (palette, appearance) = (theme.palette, theme.appearance);
        let (snapshot, now_ms, selected, starting) = {
            let model = self.model.read(cx);
            (
                model.snapshot().clone(),
                model.now_ms(),
                model.selected().clone(),
                matches!(model.phase(), crate::model::Phase::Starting),
            )
        };
        let tree: Tree = self.tree(cx);
        let keyboard = window.last_input_was_keyboard() && self.focus.contains_focused(window, cx);
        let paint = Paint {
            palette,
            appearance,
            now_ms,
            selected,
            cursor: if keyboard { self.cursor.clone() } else { None },
        };
        let filter_text = self.filter_text(cx);

        // The list's children, in the order of the keys (the arrow keys scroll to them).
        let mut children: Vec<AnyElement> = Vec::new();
        let mut row_ix = std::collections::HashMap::new();
        {
            let (unread, failed) = {
                let model = self.model.read(cx);
                (model.unread().clone(), model.failed().clone())
            };
            let marks = crate::sidebar::data::Marks {
                unread: Some(&unread),
                failed: Some(&failed),
            };
            if !tree.pinned.is_empty() {
                children.push(self.pinned_label(&paint));
                for id in &tree.pinned {
                    if let Some(thread) = snapshot.thread(id) {
                        row_ix.insert(RowKey::Thread(id.clone()), children.len());
                        let project = snapshot.project(&thread.project);
                        children.push(
                            self.thread_row(thread, project, false, &paint, &snapshot, &marks, cx),
                        );
                    }
                }
            }
            for (ix, group) in tree.groups.iter().enumerate() {
                let Some(project) = snapshot.project(&group.project) else {
                    continue;
                };
                row_ix.insert(RowKey::Folder(group.project.clone()), children.len());
                children.push(self.folder_header(
                    project,
                    group,
                    ix == 0 && tree.pinned.is_empty(),
                    &paint,
                    cx,
                ));
                if group.collapsed {
                    continue;
                }
                if group.threads.is_empty() && filter_text.trim().is_empty() {
                    let note = if group.total == 0 && tree.pinned.is_empty() || group.total == 0 {
                        "No threads yet"
                    } else {
                        "All threads are pinned"
                    };
                    children.push(self.note_row(
                        dom_id("empty", &group.project),
                        note.to_string(),
                        &paint,
                    ));
                }
                for id in &group.threads {
                    if let Some(thread) = snapshot.thread(id) {
                        row_ix.insert(RowKey::Thread(id.clone()), children.len());
                        children.push(self.thread_row(
                            thread,
                            Some(project),
                            true,
                            &paint,
                            &snapshot,
                            &marks,
                            cx,
                        ));
                    }
                }
                if group.hidden > 0 {
                    row_ix.insert(RowKey::More(group.project.clone()), children.len());
                    children.push(self.more_row(&group.project, group.hidden, &paint, cx));
                }
            }
        }
        let empty_text = if starting {
            None
        } else if !filter_text.trim().is_empty() && tree.matches == 0 {
            Some(format!("No threads match “{}”", filter_text.trim()))
        } else if snapshot.is_empty() {
            Some("No threads yet. Add a project to start.".to_string())
        } else {
            None
        };
        if let Some(text) = empty_text {
            children.push(
                div()
                    .id("list-empty")
                    .px(px(ROW_PAD))
                    .py(px(12.))
                    .text_size(text::XS.size)
                    .line_height(text::XS.line_height)
                    .text_color(palette.sidebar_muted_foreground.hsla())
                    .child(text)
                    .probe()
                    .into_any_element(),
            );
        }
        self.row_ix = row_ix;

        let filter_box = self.filter_box(&paint, cx);
        let confirm = self.confirm_card(&paint, cx);
        let header = self.header(cx);
        let search = self.search_row(cx);
        let footer = self.footer(&paint, cx);

        div()
            .id("sidebar")
            .key_context(keys::CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &keys::NextRow, window, cx| this.on_move(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &keys::PreviousRow, window, cx| {
                this.on_move(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &keys::FirstRow, window, cx| this.on_jump(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keys::LastRow, window, cx| this.on_jump(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keys::ActivateRow, window, cx| this.on_activate(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keys::ExpandRow, window, cx| {
                    this.on_fold(false, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &keys::CollapseRow, window, cx| {
                    this.on_fold(true, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &keys::RenameRow, window, cx| this.on_rename(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keys::DeleteRow, window, cx| this.on_delete(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keys::FocusFilter, window, cx| this.open_filter(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keys::CancelSidebar, window, cx| this.on_cancel(window, cx)),
            )
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(palette.sidebar.hsla())
            .text_color(palette.sidebar_foreground.hsla())
            .border_r_1()
            .border_color(palette.sidebar_border.hsla())
            .child(header)
            .child(search)
            .children(filter_box)
            .child(
                // t3code's `SidebarContent`: a scroll area whose edges fade (`scrollFade`,
                // 0.75rem) so a row cut by the end of the list fades out instead of being sliced
                // mid-glyph against the footer; the group's padding lets the last row scroll clear.
                div()
                    .id("threads-area")
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("threads")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .gap(px(2.))
                            .px(px(8.))
                            .pb(px(8.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(children)
                            .probe(),
                    )
                    .child(Self::fade(palette.sidebar, false))
                    .child(Self::fade(palette.sidebar, true)),
            )
            .child(footer)
            .children(confirm)
            .probe()
    }
}
