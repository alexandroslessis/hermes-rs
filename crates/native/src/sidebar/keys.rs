//! The sidebar's keyboard: actions bound in the `Sidebar` key context (active while the focus is
//! anywhere inside the sidebar, its filter box and a rename field included: a binding the text
//! field handles itself, such as Delete or Enter, never reaches these).
//!
//! t3code: `components/sidebar/SidebarThreadRow.tsx` (`handleKeyDown`: F2 renames, Enter and
//! Space open; here Enter only), `Sidebar.tsx` (arrow-key navigation between `data-sidebar-row` elements, Delete
//! on a selected thread), `shortcuts/*` (`Ctrl+F` is the filter here; t3code's sidebar search is
//! the palette). The up and down keys, Home and End walk the rows ([`super::data::step`]).

use gpui_kit::{App, KeyBinding};

gpui_kit::actions!(
    hermes_sidebar,
    [
        /// Down: the next row.
        NextRow,
        /// Up: the previous row.
        PreviousRow,
        /// Home: the first row.
        FirstRow,
        /// End: the last row.
        LastRow,
        /// Enter: open the thread, open or close the folder.
        ActivateRow,
        /// Right: open a folder.
        ExpandRow,
        /// Left: close a folder.
        CollapseRow,
        /// F2: rename the thread or the project.
        RenameRow,
        /// Delete: delete the thread (asks first) or remove the project.
        DeleteRow,
        /// Ctrl+F: the filter box.
        FocusFilter,
        /// Escape: leave the filter, a rename or a question.
        CancelSidebar,
    ]
);

/// The key context the sidebar's root declares.
pub const CONTEXT: &str = "Sidebar";

/// Registers the bindings.
pub fn bind(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", NextRow, context),
        KeyBinding::new("up", PreviousRow, context),
        KeyBinding::new("home", FirstRow, context),
        KeyBinding::new("end", LastRow, context),
        KeyBinding::new("enter", ActivateRow, context),
        KeyBinding::new("right", ExpandRow, context),
        KeyBinding::new("left", CollapseRow, context),
        KeyBinding::new("f2", RenameRow, context),
        KeyBinding::new("delete", DeleteRow, context),
        KeyBinding::new("ctrl-f", FocusFilter, context),
        KeyBinding::new("escape", CancelSidebar, context),
    ]);
}
