//! The thread header's data, with no GPUI in it: what the backend answers about the open thread
//! ([`HeaderInfo`]: its folder, its model, the git state of the folder), how a git status becomes
//! the chips the header shows ([`GitSummary`]), what committing a renamed title means
//! ([`rename_outcome`]), and which entries the actions menu has and whether each is enabled
//! ([`menu_entries`]). Pure functions, tested without a window.
//!
//! t3code: `apps/web/src/components/chat/ChatHeader.tsx` (`resolveRenameCommit`: an empty title is
//! refused with a warning, an unchanged one is a no-op), `hooks/useThreadActionMenu.ts` (the
//! thread's actions), `components/GitActionsControl.tsx` and `BranchToolbar.tsx` (the branch and
//! the change counts). Reference: `app-thread-dark.png` (the breadcrumb), `app-thread-options.png`
//! (the actions menu), `sidebar-thread-details.png` (branch and "Changes +N -M").

use hermes_core::export::Format;
use hermes_core::git::RepoStatus;

/// How a git status of the thread's folder reads: no folder, not a repository, or a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitState {
    /// The thread's folder is not on disk any more (moved, deleted, a drive that is gone).
    NoFolder,
    /// The folder is not in a git repository, or `git` could not run (the core answers both the
    /// same way: `repo_status` is `None`).
    NoRepo,
    Repo(GitSummary),
}

/// A repository's state as the header shows it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GitSummary {
    /// The branch; `None` with `detached`.
    pub branch: Option<String>,
    /// `HEAD` is a commit, not a branch.
    pub detached: bool,
    /// Commits ahead of and behind the upstream (0 and 0 without one).
    pub ahead: i64,
    pub behind: i64,
    /// Changed paths, and how many of them are staged, changed only in the working tree,
    /// untracked, in conflict (a path can be both staged and unstaged).
    pub changed: usize,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    /// Lines added and removed against `HEAD` (untracked files' lines are in `added`).
    pub added: u64,
    pub removed: u64,
}

impl GitSummary {
    /// What the core's `repo_status` says, counted.
    pub fn from_status(status: &RepoStatus) -> Self {
        Self {
            branch: status.branch.clone(),
            detached: status.detached,
            ahead: status.ahead.max(0),
            behind: status.behind.max(0),
            changed: status.files.len(),
            staged: status.count(|f| f.staged),
            unstaged: status.count(|f| f.unstaged && !f.untracked),
            untracked: status.count(|f| f.untracked),
            conflicted: status.count(|f| f.conflicted),
            added: status.added,
            removed: status.removed,
        }
    }

    /// Nothing changed in the working tree.
    pub fn is_clean(&self) -> bool {
        self.changed == 0
    }

    /// The chip's text: the branch, or `detached HEAD`.
    pub fn branch_label(&self) -> String {
        match (&self.branch, self.detached) {
            (Some(branch), _) => branch.clone(),
            (None, true) => "detached HEAD".to_string(),
            // A repository before its first commit has a branch name; one with neither is
            // something git did not describe: say so plainly.
            (None, false) => "no branch".to_string(),
        }
    }

    /// `↑2 ↓1` for a branch ahead of and behind its upstream; `None` when level or without one.
    pub fn sync_label(&self) -> Option<String> {
        match (self.ahead > 0, self.behind > 0) {
            (false, false) => None,
            (true, false) => Some(format!("↑{}", self.ahead)),
            (false, true) => Some(format!("↓{}", self.behind)),
            (true, true) => Some(format!("↑{} ↓{}", self.ahead, self.behind)),
        }
    }

    /// `+120` and `-4`, as t3code's "Changes +6,770 -0" spells them (a thousands comma); `None`
    /// when the working tree is clean.
    pub fn change_counts(&self) -> Option<(String, String)> {
        if self.is_clean() {
            return None;
        }
        Some((
            format!("+{}", thousands(self.added)),
            format!("-{}", thousands(self.removed)),
        ))
    }

    /// The tooltip: the whole story in a sentence or two.
    pub fn tooltip(&self) -> String {
        let mut out = match (&self.branch, self.detached) {
            (Some(branch), _) => format!("Branch {branch}"),
            (None, true) => "HEAD is detached (not on a branch)".to_string(),
            (None, false) => "No branch".to_string(),
        };
        if self.is_clean() {
            out.push_str(" · no uncommitted changes");
        } else {
            let mut parts = Vec::new();
            for (n, what) in [
                (self.staged, "staged"),
                (self.unstaged, "modified"),
                (self.untracked, "untracked"),
                (self.conflicted, "in conflict"),
            ] {
                if n > 0 {
                    parts.push(format!("{n} {what}"));
                }
            }
            out.push_str(&format!(
                " · {} changed file{} ({})",
                self.changed,
                if self.changed == 1 { "" } else { "s" },
                parts.join(", ")
            ));
        }
        match (self.ahead, self.behind) {
            (0, 0) => {}
            (a, 0) => out.push_str(&format!(" · {a} ahead of upstream")),
            (0, b) => out.push_str(&format!(" · {b} behind upstream")),
            (a, b) => out.push_str(&format!(" · {a} ahead, {b} behind upstream")),
        }
        out
    }
}

/// `6770` as `6,770`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the header asks the backend about the open thread, in one blocking read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderInfo {
    /// The folder the thread works in (the store's `cwd`).
    pub cwd: String,
    /// The model and the endpoint the thread talks to, as stored (empty: none chosen).
    pub model: String,
    pub provider: String,
    /// The thread is in the archive (the list leaves archived threads out, so only a thread
    /// archived from elsewhere while it is open shows it).
    pub archived: bool,
    pub git: GitState,
}

impl HeaderInfo {
    /// The model's name for the chip: the stored model, or `None` when none is set.
    pub fn model_label(&self) -> Option<&str> {
        Some(self.model.trim()).filter(|m| !m.is_empty())
    }

    /// The model chip's tooltip.
    pub fn model_tooltip(&self) -> String {
        let model = self.model_label().unwrap_or("none");
        match self.provider.trim() {
            "" => format!("Model: {model}"),
            provider => format!("Model: {model} · endpoint {provider}"),
        }
    }
}

/// A saved conversation, ready for the save dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub content: String,
    /// The core's suggested file name (`hermes-<title words>.md`).
    pub file_name: String,
}

/// The kinds of file the menu exports. JSON is Hermes's transcript shape and is built by the page's
/// gateway (`crates/desktop/src/export.rs`), not by `hermes_core`, so the native app offers the
/// two formats the core builds.
pub const EXPORT_FORMATS: [(Format, &str); 2] =
    [(Format::Markdown, "Markdown"), (Format::Html, "HTML")];

/// What committing the title field means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameOutcome {
    /// Rename the thread to this (trimmed).
    Commit(String),
    /// An empty title: refused, with a warning.
    RejectEmpty,
    /// Nothing to do: the title is as it was.
    Noop,
}

/// t3code's `resolveRenameCommit`.
pub fn rename_outcome(typed: &str, original: &str) -> RenameOutcome {
    let typed = typed.trim();
    if typed.is_empty() {
        RenameOutcome::RejectEmpty
    } else if typed == original.trim() {
        RenameOutcome::Noop
    } else {
        RenameOutcome::Commit(typed.to_string())
    }
}

/// What a menu entry does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Rename,
    TogglePin,
    /// A new thread with the conversation through the latest reply.
    Branch,
    Export(Format),
    CopyId,
    Archive,
}

/// One line of the actions menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        action: MenuAction,
        label: String,
        enabled: bool,
    },
    Separator,
}

impl MenuEntry {
    /// The action, for an item.
    pub fn action(&self) -> Option<MenuAction> {
        match self {
            MenuEntry::Item { action, .. } => Some(*action),
            MenuEntry::Separator => None,
        }
    }

    /// Whether the entry can be chosen.
    pub fn enabled(&self) -> bool {
        matches!(self, MenuEntry::Item { enabled: true, .. })
    }

    /// The label, for an item.
    pub fn label(&self) -> Option<&str> {
        match self {
            MenuEntry::Item { label, .. } => Some(label),
            MenuEntry::Separator => None,
        }
    }
}

/// What decides which entries are on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MenuState {
    pub archived: bool,
    pub pinned: bool,
    /// The title field is open.
    pub renaming: bool,
    /// The thread has a finished reply to branch from, and nothing is running or being made.
    pub can_branch: bool,
    /// The thread has messages to save.
    pub can_export: bool,
}

/// The thread actions menu, in t3code's order (`app-thread-options.png`: rename, pin, then the
/// rest, archive last), with the entries that cannot be used now disabled.
pub fn menu_entries(state: &MenuState) -> Vec<MenuEntry> {
    let item = |action, label: &str, enabled| MenuEntry::Item {
        action,
        label: label.to_string(),
        enabled,
    };
    let mut entries = vec![
        item(MenuAction::Rename, "Rename thread", !state.renaming),
        item(
            MenuAction::TogglePin,
            if state.pinned {
                "Unpin thread"
            } else {
                "Pin thread"
            },
            !state.archived,
        ),
        MenuEntry::Separator,
        item(
            MenuAction::Branch,
            "Branch from latest reply",
            state.can_branch,
        ),
    ];
    for (format, name) in EXPORT_FORMATS {
        entries.push(item(
            MenuAction::Export(format),
            &format!("Export as {name}…"),
            state.can_export,
        ));
    }
    entries.push(item(MenuAction::CopyId, "Copy thread ID", true));
    entries.push(MenuEntry::Separator);
    entries.push(item(
        MenuAction::Archive,
        if state.archived {
            "Archived"
        } else {
            "Archive thread"
        },
        !state.archived,
    ));
    entries
}

/// How many breadcrumb chips fit: the header shows what it has room for, most useful first.
/// `room` is the width the header may use, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chips {
    pub git: bool,
    pub model: bool,
}

/// Below [`GIT_ROOM`] the branch chip would take the title's width; below [`MODEL_ROOM`] the
/// model's would.
pub const GIT_ROOM: f32 = 380.;
pub const MODEL_ROOM: f32 = 560.;

/// The widest the git chip gets (it never shrinks below its content: icon, arrows, counts and the
/// branch, the branch cut in the middle past 160 px), and the least the model chip keeps.
pub const GIT_CHIP_MAX: f32 = 220.;
pub const MODEL_CHIP_MIN: f32 = 56.;

impl Chips {
    /// The width the shown chips may hold on to when the bar is short: the title's claim
    /// ([`title_min_width`]) is a share of what is left.
    pub fn reserved(&self) -> f32 {
        f32::from(u8::from(self.git)) * GIT_CHIP_MAX
            + f32::from(u8::from(self.model)) * MODEL_CHIP_MIN
    }

    /// The chips that fit in `room` pixels.
    pub fn for_room(room: f32) -> Self {
        Self {
            git: room >= GIT_ROOM,
            model: room >= MODEL_ROOM,
        }
    }
}

/// The least the title (and its chevron) ever keeps.
pub const TITLE_MIN: f32 = 40.;
/// The share of the header's room the title may claim before the project crumb and the chips
/// give up theirs, and the most that claim is.
const TITLE_SHARE: f32 = 0.4;
const TITLE_CLAIM_MAX: f32 = 240.;

/// The title's minimum width in a header with `room` pixels: what the title needs (estimated from
/// its length, with the chevron), but no more than 40% of the room or 240 px, and never under
/// 40 px. A short title claims only its own width, so nothing is pushed away from it; a long one
/// keeps a readable share while the project crumb (which shrinks first) is cut in the middle.
/// t3code's title is `min-w-10 flex-1` beside a project that shrinks: the same order, with a share
/// instead of 40 px, since a 40 px title showed a bare ellipsis beside a long project name.
pub fn title_min_width(title: &str, room: f32) -> f32 {
    // About 7.5 px a character at the header's size and weight, plus the chevron and its gap.
    let needs = title.chars().count() as f32 * 7.5 + 22.;
    let claim = (room * TITLE_SHARE).clamp(TITLE_MIN, TITLE_CLAIM_MAX);
    needs.min(claim).max(TITLE_MIN)
}

#[cfg(test)]
mod tests {
    use hermes_core::git::FileStatus;

    use super::*;

    fn file(
        path: &str,
        staged: bool,
        unstaged: bool,
        untracked: bool,
        conflicted: bool,
    ) -> FileStatus {
        FileStatus {
            path: path.into(),
            staged,
            unstaged,
            untracked,
            conflicted,
        }
    }

    fn status(files: Vec<FileStatus>) -> RepoStatus {
        RepoStatus {
            branch: Some("main".into()),
            default_branch: Some("main".into()),
            detached: false,
            ahead: 0,
            behind: 0,
            files,
            added: 0,
            removed: 0,
        }
    }

    #[core::prelude::v1::test]
    fn a_clean_branch_shows_its_name_and_nothing_else() {
        let summary = GitSummary::from_status(&status(Vec::new()));
        assert!(summary.is_clean());
        assert_eq!(summary.branch_label(), "main");
        assert_eq!(summary.sync_label(), None);
        assert_eq!(summary.change_counts(), None);
        assert_eq!(summary.tooltip(), "Branch main · no uncommitted changes");
    }

    #[core::prelude::v1::test]
    fn dirty_counts_split_staged_modified_untracked_and_conflicted() {
        let mut raw = status(vec![
            file("a.rs", true, false, false, false),
            // Staged and changed again after: counts in both.
            file("b.rs", true, true, false, false),
            file("c.rs", false, true, false, false),
            file("new.txt", false, true, true, false),
            file("clash.rs", false, false, false, true),
        ]);
        raw.added = 6770;
        raw.removed = 3;
        let summary = GitSummary::from_status(&raw);
        assert_eq!(
            (
                summary.changed,
                summary.staged,
                summary.unstaged,
                summary.untracked,
                summary.conflicted
            ),
            (5, 2, 2, 1, 1)
        );
        assert!(!summary.is_clean());
        assert_eq!(
            summary.change_counts(),
            Some(("+6,770".to_string(), "-3".to_string()))
        );
        assert_eq!(
            summary.tooltip(),
            "Branch main · 5 changed files (2 staged, 2 modified, 1 untracked, 1 in conflict)"
        );
        // One file reads in the singular.
        let one = GitSummary::from_status(&status(vec![file("a", false, true, false, false)]));
        assert!(one.tooltip().contains("1 changed file ("));
    }

    #[core::prelude::v1::test]
    fn ahead_and_behind_read_as_arrows_and_never_negative() {
        let mut raw = status(Vec::new());
        raw.ahead = 2;
        assert_eq!(
            GitSummary::from_status(&raw).sync_label().as_deref(),
            Some("↑2")
        );
        raw.behind = 1;
        let both = GitSummary::from_status(&raw);
        assert_eq!(both.sync_label().as_deref(), Some("↑2 ↓1"));
        assert!(both.tooltip().ends_with("2 ahead, 1 behind upstream"));
        raw.ahead = 0;
        assert_eq!(
            GitSummary::from_status(&raw).sync_label().as_deref(),
            Some("↓1")
        );
        raw.behind = -4;
        assert_eq!(GitSummary::from_status(&raw).sync_label(), None);
    }

    #[core::prelude::v1::test]
    fn a_detached_head_has_no_branch_name() {
        let mut raw = status(Vec::new());
        raw.branch = None;
        raw.detached = true;
        let summary = GitSummary::from_status(&raw);
        assert!(summary.detached);
        assert_eq!(summary.branch_label(), "detached HEAD");
        assert!(summary.tooltip().starts_with("HEAD is detached"));
    }

    #[core::prelude::v1::test]
    fn thousands_commas_group_by_three() {
        for (n, text) in [
            (0, "0"),
            (7, "7"),
            (999, "999"),
            (1000, "1,000"),
            (6770, "6,770"),
            (1_234_567, "1,234,567"),
        ] {
            assert_eq!(thousands(n), text);
        }
    }

    #[core::prelude::v1::test]
    fn the_model_chip_names_the_model_and_its_endpoint() {
        let mut info = HeaderInfo {
            cwd: "C:/work".into(),
            model: "openrouter/free".into(),
            provider: "openrouter".into(),
            archived: false,
            git: GitState::NoRepo,
        };
        assert_eq!(info.model_label(), Some("openrouter/free"));
        assert_eq!(
            info.model_tooltip(),
            "Model: openrouter/free · endpoint openrouter"
        );
        info.model = "  ".into();
        info.provider = String::new();
        assert_eq!(info.model_label(), None);
        assert_eq!(info.model_tooltip(), "Model: none");
    }

    #[core::prelude::v1::test]
    fn committing_a_title_follows_t3codes_rules() {
        assert_eq!(
            rename_outcome("New name", "Old"),
            RenameOutcome::Commit("New name".into())
        );
        // Trimmed.
        assert_eq!(
            rename_outcome("  New  ", "Old"),
            RenameOutcome::Commit("New".into())
        );
        // Empty (or blanks) is refused, whatever the old title.
        assert_eq!(rename_outcome("", "Old"), RenameOutcome::RejectEmpty);
        assert_eq!(rename_outcome("   \t", "Old"), RenameOutcome::RejectEmpty);
        // Unchanged is nothing (also when only the spaces differ).
        assert_eq!(rename_outcome("Old", "Old"), RenameOutcome::Noop);
        assert_eq!(rename_outcome(" Old ", "Old"), RenameOutcome::Noop);
    }

    #[core::prelude::v1::test]
    fn the_menu_has_every_action_and_its_enabled_state_follows_the_thread() {
        let on = MenuState {
            can_branch: true,
            can_export: true,
            ..MenuState::default()
        };
        let entries = menu_entries(&on);
        let actions: Vec<_> = entries.iter().filter_map(MenuEntry::action).collect();
        assert_eq!(
            actions,
            vec![
                MenuAction::Rename,
                MenuAction::TogglePin,
                MenuAction::Branch,
                MenuAction::Export(Format::Markdown),
                MenuAction::Export(Format::Html),
                MenuAction::CopyId,
                MenuAction::Archive,
            ]
        );
        assert!(
            entries
                .iter()
                .filter(|e| e.action().is_some())
                .all(MenuEntry::enabled)
        );
        // Archive is the last item, after a separator (t3code: the way out is at the bottom).
        assert_eq!(entries[entries.len() - 2], MenuEntry::Separator);

        let enabled = |state: &MenuState, action| {
            menu_entries(state)
                .iter()
                .find(|e| e.action() == Some(action))
                .is_some_and(MenuEntry::enabled)
        };
        // An empty thread has nothing to export or to branch from.
        let empty = MenuState::default();
        assert!(!enabled(&empty, MenuAction::Export(Format::Markdown)));
        assert!(!enabled(&empty, MenuAction::Export(Format::Html)));
        assert!(!enabled(&empty, MenuAction::Branch));
        assert!(enabled(&empty, MenuAction::Archive) && enabled(&empty, MenuAction::Rename));
        // An archived thread cannot be archived again (or pinned).
        let archived = MenuState {
            archived: true,
            ..on
        };
        assert!(!enabled(&archived, MenuAction::Archive));
        assert!(!enabled(&archived, MenuAction::TogglePin));
        let entries = menu_entries(&archived);
        assert!(entries.iter().any(|e| e.label() == Some("Archived")));
        // Renaming in place disables the rename entry; pinned reads "Unpin".
        let renaming = MenuState {
            renaming: true,
            pinned: true,
            ..on
        };
        assert!(!enabled(&renaming, MenuAction::Rename));
        assert!(
            menu_entries(&renaming)
                .iter()
                .any(|e| e.label() == Some("Unpin thread"))
        );
    }

    #[core::prelude::v1::test]
    fn the_title_claims_its_width_up_to_a_share_of_the_room() {
        // Short: its own width.
        assert_eq!(title_min_width("halo", 600.), 4. * 7.5 + 22.);
        // Long: 40% of the room, at most 240 px.
        let long = "a".repeat(200);
        assert_eq!(title_min_width(&long, 400.), 160.);
        assert_eq!(title_min_width(&long, 2000.), 240.);
        assert_eq!(title_min_width(&long, f32::INFINITY), 240.);
        // The chips' hold comes off the room first.
        let both = Chips::for_room(MODEL_ROOM);
        assert_eq!(both.reserved(), GIT_CHIP_MAX + MODEL_CHIP_MIN);
        assert_eq!(Chips::for_room(0.).reserved(), 0.);
        // A tiny room: never under 40 px.
        assert_eq!(title_min_width(&long, 50.), TITLE_MIN);
        assert_eq!(title_min_width("", 600.), TITLE_MIN);
    }

    #[core::prelude::v1::test]
    fn chips_leave_when_the_room_is_short() {
        assert_eq!(
            Chips::for_room(100.),
            Chips {
                git: false,
                model: false
            }
        );
        assert_eq!(
            Chips::for_room(GIT_ROOM),
            Chips {
                git: true,
                model: false
            }
        );
        assert_eq!(
            Chips::for_room(MODEL_ROOM),
            Chips {
                git: true,
                model: true
            }
        );
    }
}
