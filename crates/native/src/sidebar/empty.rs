//! The two empty states of the main area that belong to the sidebar's world: `NoProjectsHero`
//! (nothing added yet: "What should we work on?", add a folder, or start without one) and
//! `NoActiveThreadState` ("Pick a thread to continue").
//!
//! t3code: `apps/web/src/components/NoProjectsHero.tsx` (hero size; the primary "Add project"
//! button and the outline "Start without a project"), `NoActiveThreadState.tsx` (default size,
//! with the muted "No active thread" label in the header, which the frame's title bar shows).
//! No screenshot of either in the reference set (`app-empty-dark.png` is the frame with an empty
//! thread); sizes and texts follow the source. Gallery story: `empty-states`.

use gpui_kit::{App, IntoElement, ParentElement as _, Window, assets::IconName};

use crate::design::{
    Button, EmptyState,
    button::{self, Variant},
    empty_state::Size,
};

/// The first-run state: no project, no thread.
pub fn no_projects_hero(
    on_add: impl Fn(&mut Window, &mut App) + 'static,
    on_scratch: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    EmptyState::new("no-projects", "What should we work on?")
        .description("Add a project, or start without one.")
        .size(Size::Hero)
        .child(
            Button::new("add-project-hero", "Add project")
                .icon(IconName::Plus)
                .size(button::Size::Sm)
                .on_click(move |_, window, cx| on_add(window, cx)),
        )
        .child(
            Button::new("start-without-project", "Start without a project")
                .icon(IconName::MessageSquare)
                .variant(Variant::Outline)
                .size(button::Size::Sm)
                .on_click(move |_, window, cx| on_scratch(window, cx)),
        )
}

/// Threads exist, none is open.
pub fn no_active_thread() -> impl IntoElement {
    EmptyState::new("no-active-thread", "Pick a thread to continue")
        .description("Select an existing thread or create a new one to get started.")
}
