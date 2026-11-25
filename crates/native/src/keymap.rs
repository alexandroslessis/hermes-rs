//! Actions and keybindings. Every user-visible action is a GPUI action with a name; the one
//! table here ([`COMMANDS`]) lists them with their title, their default shortcut and the t3code
//! command they follow, and is what the command palette lists and what [`bind`] registers.
//! Shortcuts are t3code's defaults with `mod` read as `Ctrl` (`DEFAULT_KEYBINDINGS` in
//! `packages/shared/src/keybindings.ts`), except where Windows already uses the key (see
//! [`RESERVED`]); `Ctrl+Shift+P` is added for the palette (the VS Code habit). Later phases add
//! commands to the table; a user keymap file is a later phase (README, "Zed's half").
//!
//! t3code: `packages/shared/src/keybindings.ts` (`DEFAULT_KEYBINDINGS`), `apps/web/src/keybindings.ts`
//! (`formatShortcutKeyLabel`), `apps/web/src/shortcuts/commandPaletteCommands.ts` (what the
//! palette lists). No screenshot: the shortcuts show in the palette and tooltips.

use gpui_kit::{Action, App, KeyBinding};

gpui_kit::actions!(
    hermes_native,
    [
        ToggleSidebar,
        ToggleRightPanel,
        ToggleDrawer,
        OpenCommandPalette,
        OpenActionPalette,
        OpenFileFinder,
        NewThread,
        OpenSettings,
        CycleTheme,
        FocusNextRegion,
        FocusPreviousRegion,
    ]
);

/// One command: what the palette shows and what runs.
pub struct CommandDef {
    /// Stable name (`sidebar.toggle`, t3code's where it has one).
    pub id: &'static str,
    /// The title in the palette.
    pub title: &'static str,
    /// The palette group.
    pub group: &'static str,
    /// The default shortcut in GPUI's spelling (`ctrl-b`), if any.
    pub keys: Option<&'static str>,
    /// A GPUI action to dispatch.
    pub make: fn() -> Box<dyn Action>,
    /// Builds the key binding for the shortcut.
    pub bind: fn(&'static str) -> KeyBinding,
}

macro_rules! command {
    ($id:literal, $title:literal, $group:literal, $keys:expr, $action:expr) => {
        CommandDef {
            id: $id,
            title: $title,
            group: $group,
            keys: $keys,
            make: || Box::new($action),
            bind: |keys| KeyBinding::new(keys, $action, None),
        }
    };
}

/// Every command, in the order the palette lists them.
pub const COMMANDS: &[CommandDef] = &[
    command!(
        "commandPalette.toggle",
        "Open command palette",
        "General",
        Some("ctrl-k"),
        OpenCommandPalette
    ),
    command!(
        "commandPalette.actions",
        "Show all actions",
        "General",
        Some("ctrl-shift-p"),
        OpenActionPalette
    ),
    command!(
        "chat.new",
        "New thread",
        "Threads",
        Some("ctrl-n"),
        NewThread
    ),
    command!(
        "sidebar.toggle",
        "Toggle sidebar",
        "View",
        Some("ctrl-b"),
        ToggleSidebar
    ),
    command!(
        "rightPanel.toggle",
        "Toggle right panel",
        "View",
        Some("ctrl-alt-b"),
        ToggleRightPanel
    ),
    command!(
        "terminal.toggle",
        "Toggle terminal drawer",
        "View",
        Some("ctrl-j"),
        ToggleDrawer
    ),
    // After the panel toggles: the palette's first rows (and its gallery story, which shows the
    // first five actions) stay the ones they were. `Ctrl+P` is bound globally; CP6.3's terminal
    // must let it through to the shell while it has the focus (a context binding there).
    command!(
        "files.find",
        "Go to file",
        "Files",
        Some("ctrl-p"),
        OpenFileFinder
    ),
    command!(
        "appearance.cycle",
        "Cycle theme",
        "View",
        Some("ctrl-alt-shift-a"),
        CycleTheme
    ),
    command!(
        "workspace.focusNext",
        "Focus next region",
        "View",
        Some("f6"),
        FocusNextRegion
    ),
    command!(
        "workspace.focusPrevious",
        "Focus previous region",
        "View",
        Some("shift-f6"),
        FocusPreviousRegion
    ),
    command!(
        "settings.open",
        "Open settings",
        "General",
        Some("ctrl-,"),
        OpenSettings
    ),
];

/// Keys Windows or the text widgets already use: no command may bind them (`Alt+F4` closes the
/// window, `Ctrl+C/V/X/Z/Y/A` edit text, `Ctrl+W` and `Ctrl+Tab` are for tabs we do not have yet
/// but a browser-minded user expects, `Win+...` belongs to the shell).
pub const RESERVED: &[&str] = &[
    "alt-f4",
    "ctrl-c",
    "ctrl-v",
    "ctrl-x",
    "ctrl-z",
    "ctrl-y",
    "ctrl-a",
    "alt-tab",
    "ctrl-alt-delete",
    "ctrl-shift-escape",
    "alt-space",
];

/// Registers every default binding with GPUI (global context, so they work wherever focus is).
pub fn bind(cx: &mut App) {
    let bindings: Vec<KeyBinding> = COMMANDS
        .iter()
        .filter_map(|c| c.keys.map(|keys| (c.bind)(keys)))
        .collect();
    cx.bind_keys(bindings);
}

/// The command called `id`.
pub fn find(id: &str) -> Option<&'static CommandDef> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// The shortcut of `command` as shown to the user (`Ctrl+Shift+P`).
pub fn shortcut_label(command: &CommandDef) -> Option<String> {
    let keys = command.keys?;
    let keystroke = gpui_kit::Keystroke::parse(keys).ok()?;
    Some(crate::design::kbd::chord_label(&keystroke))
}

/// A tooltip for the control that runs command `id`: `text` and the shortcut in parentheses
/// (`Toggle terminal drawer (Ctrl+J)`), from the keymap, so a rebinding changes the tooltip too.
/// Just `text` for a command without a shortcut.
pub fn tooltip(id: &str, text: &str) -> String {
    match find(id).and_then(shortcut_label) {
        Some(shortcut) => format!("{text} ({shortcut})"),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[core::prelude::v1::test]
    fn every_default_shortcut_parses_and_is_unique() {
        let mut seen = HashSet::new();
        for command in COMMANDS {
            let Some(keys) = command.keys else { continue };
            let parsed = gpui_kit::Keystroke::parse(keys)
                .unwrap_or_else(|e| panic!("{}: {keys:?} does not parse: {e}", command.id));
            // Normalised, so `ctrl-shift-p` and `shift-ctrl-p` count as one.
            let label = crate::design::kbd::chord_label(&parsed);
            assert!(
                seen.insert(label.clone()),
                "{label} is bound twice ({})",
                command.id
            );
        }
    }

    #[core::prelude::v1::test]
    fn command_ids_and_titles_are_unique() {
        let ids: HashSet<_> = COMMANDS.iter().map(|c| c.id).collect();
        let titles: HashSet<_> = COMMANDS.iter().map(|c| c.title).collect();
        assert_eq!(ids.len(), COMMANDS.len());
        assert_eq!(titles.len(), COMMANDS.len());
    }

    #[core::prelude::v1::test]
    fn no_command_takes_a_key_windows_owns() {
        let reserved: HashSet<String> = RESERVED
            .iter()
            .map(|k| crate::design::kbd::chord_label(&gpui_kit::Keystroke::parse(k).unwrap()))
            .collect();
        for command in COMMANDS {
            if let Some(label) = shortcut_label(command) {
                assert!(!reserved.contains(&label), "{} binds {label}", command.id);
                // The Windows key belongs to the shell.
                assert!(!label.contains("Win"), "{} binds {label}", command.id);
            }
        }
    }

    /// The defaults are t3code's with `mod` as Ctrl: sidebar Ctrl+B, terminal Ctrl+J, right panel
    /// Ctrl+Alt+B, palette Ctrl+K, new thread Ctrl+N, F6 to cycle focus.
    #[core::prelude::v1::test]
    fn the_defaults_follow_t3code() {
        let label = |id: &str| shortcut_label(find(id).unwrap()).unwrap();
        assert_eq!(label("sidebar.toggle"), "Ctrl+B");
        assert_eq!(label("terminal.toggle"), "Ctrl+J");
        assert_eq!(label("rightPanel.toggle"), "Ctrl+Alt+B");
        assert_eq!(label("commandPalette.toggle"), "Ctrl+K");
        assert_eq!(label("commandPalette.actions"), "Ctrl+Shift+P");
        assert_eq!(label("chat.new"), "Ctrl+N");
        assert_eq!(label("workspace.focusNext"), "F6");
        assert_eq!(label("workspace.focusPrevious"), "Shift+F6");
    }

    #[core::prelude::v1::test]
    fn every_command_makes_its_action() {
        for command in COMMANDS {
            let action = (command.make)();
            assert!(!action.name().is_empty(), "{}", command.id);
        }
    }
}
