//! The sidebar: projects as collapsible folders with their threads, pinned threads on top, a
//! filter, drag a thread onto a project, a footer with settings, usage and the theme toggle, and
//! the two empty states of the main area. It reads and edits only the app model
//! ([`crate::model::AppModel`]); the model talks to the core. Layout and behaviour follow
//! t3code's sidebar (module headers say where).
//!
//! | module | what |
//! |---|---|
//! | `data` | the data and the list built from it (sort, group, filter, status, time, keyboard steps) with no GPUI |
//! | `color` | a project's monogram colour |
//! | `keys` | the sidebar's keyboard actions and bindings |
//! | `view` | the view's state and every action |
//! | `render` | the elements |
//! | `empty` | `NoProjectsHero`, `NoActiveThreadState` |

pub mod color;
pub mod data;
pub mod empty;
pub mod keys;
mod render;
#[cfg(test)]
mod tests;
mod view;

pub use render::{DragThread, FOLDER_HEIGHT, ROW_HEIGHT, ROW_INSET, ROW_PAD, monogram};
pub use view::{Confirm, ConfirmKind, RenameTarget, Sidebar, SidebarEvent};
