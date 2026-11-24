//! What the composer's `@` and `/` menus work from, with no window in them: where in the text the
//! user is typing a mention or a command ([`trigger`]), the entries the menus offer
//! ([`Suggestion`], from the core's path completion; [`CommandSpec`], from its command catalog and
//! the commands that are ours), what a picked mention becomes in the text ([`Chip`]: the reference
//! the runtime expands, `@file:src/a.rs`), and how a text's references are found again to be
//! shown as chips when a draft is loaded or text is pasted ([`reference_chips`]).
//!
//! A chip is only a way to draw and to delete a reference: the text under it IS the reference
//! (`@file:src/a.rs`), exactly what the Tauri page sends (it inserts the completion's `text`), so
//! a draft, a copy and the prompt the runtime reads need no other format. A draft saved before
//! chips existed is plain text and loads as it was.
//!
//! The command catalog is the core's (`Core::commands`: the coding commands and the skills and
//! bundles). The core's reports (`/usage`, `/diff`, ...) run through `Core::run_command` but are
//! not listed in that catalog (the Tauri page knows them itself), and the app's own commands
//! (`/new`, `/settings`, ...) are no business of the core: both lists are here, with the
//! argument hints the core has no field for.
//!
//! t3code: `apps/web/src/composer-logic.ts` (`detectComposerTrigger`: a trigger is the word before
//! the caret that starts with `@` or `/`), `composer-editor-mentions.ts` (a reference becomes a
//! chip), `components/chat/ComposerCommandMenu.tsx` (the entries: icon, label, description).
//!
//! Hermes: `tui_gateway/methods_complete.py` (`complete.path`, `complete.slash`: the entries and
//! their `text`), `hermes_cli/commands.py` (the commands).

use std::ops::Range;

use hermes_tools::references::{format_value, spans};

// --- where the user is typing ----------------------------------------------------------------

/// What the text before the caret asks a menu for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The caret ends a word that starts with `@`: `range` is the word (it is replaced by the
    /// pick), `word` its text (what the core completes).
    Mention { range: Range<usize>, word: String },
    /// The text starts with `/` and the caret is in that first word: `query` is what follows the
    /// slash up to the caret.
    Command { query: String },
    /// The text starts with `/name` and the caret is past it: the command's arguments are being
    /// typed (the menu shows their hint).
    Arguments { name: String },
}

impl Trigger {
    /// Where the trigger starts, to tell one trigger from the next (a dismissed menu stays
    /// closed until the trigger changes).
    pub fn key(&self) -> (u8, usize) {
        match self {
            Trigger::Mention { range, .. } => (0, range.start),
            Trigger::Command { .. } => (1, 0),
            Trigger::Arguments { .. } => (2, 0),
        }
    }
}

/// The trigger at `cursor` in `text`. `tokens` are the byte ranges of the chips: a caret inside
/// one, or a word that starts in one, is no trigger.
pub fn trigger(text: &str, cursor: usize, tokens: &[Range<usize>]) -> Option<Trigger> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    if tokens.iter().any(|t| t.start < cursor && cursor < t.end) {
        return None;
    }
    if let Some(rest) = text.strip_prefix('/') {
        let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let name = &rest[..name_end];
        if cursor <= 1 + name_end {
            return Some(Trigger::Command {
                query: text[1..cursor.max(1)].to_string(),
            });
        }
        if !name.is_empty() {
            return Some(Trigger::Arguments {
                name: name.to_string(),
            });
        }
    }
    let before = &text[..cursor];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(ix, c)| ix + c.len_utf8());
    let word = &before[start..];
    if !word.starts_with('@') || tokens.iter().any(|t| t.contains(&start)) {
        return None;
    }
    Some(Trigger::Mention {
        range: start..cursor,
        word: word.to_string(),
    })
}

/// The command a text is, if it starts with `/`: its name and the rest.
pub fn parse_command(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix('/')?;
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..name_end];
    // A path (`/home/me/file`, `/c/x`) is text, not a command.
    let is_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
    is_name.then(|| (name, rest[name_end..].trim()))
}

// --- the entries of the `@` menu ----------------------------------------------------------------

/// What an entry of the `@` menu is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Folder,
    /// `@diff`, `@staged`: a reference with nothing to name.
    Directive,
    /// `@file:`, `@folder:`, `@url:`, `@git:`: the start of a reference, finished by typing.
    Prefix,
}

/// One entry of the `@` menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: MentionKind,
    /// The core's completion text (`@file:src/a.rs`, `@folder:src/`, `@diff`, `@file:`).
    pub text: String,
    /// The name (`a.rs`, `src/`).
    pub label: String,
    /// Where it is (`src`) or what it does (`git diff`).
    pub description: String,
}

impl Suggestion {
    /// An entry from the core's `PathItem` fields.
    pub fn from_item(text: &str, display: &str, meta: &str) -> Self {
        let (kind, description) = if text.ends_with(':') {
            (MentionKind::Prefix, meta.to_string())
        } else if let Some(path) = text.strip_prefix("@folder:") {
            (MentionKind::Folder, parent_of(path.trim_end_matches('/')))
        } else if let Some(path) = text.strip_prefix("@file:") {
            (MentionKind::File, parent_of(path))
        } else {
            (MentionKind::Directive, meta.to_string())
        };
        let label = match kind {
            MentionKind::Prefix | MentionKind::Directive => text.to_string(),
            _ if display.is_empty() => text.to_string(),
            _ => display.to_string(),
        };
        Self {
            kind,
            text: text.to_string(),
            label,
            description,
        }
    }

    /// What picking it with Tab does to a folder: walk into it (`@src/`), so the next entries
    /// are its own. Everything else is picked as a chip.
    pub fn drill_text(&self) -> Option<String> {
        match self.kind {
            MentionKind::Folder => self
                .text
                .strip_prefix("@folder:")
                .map(|path| format!("@{path}")),
            _ => None,
        }
    }

    /// The chip a pick makes; `None` for a prefix (it is typed on, not picked).
    pub fn chip(&self) -> Option<Chip> {
        match self.kind {
            MentionKind::Prefix => None,
            MentionKind::Directive => Some(Chip::directive(&self.text)),
            MentionKind::File => self
                .text
                .strip_prefix("@file:")
                .map(|path| Chip::of(ChipKind::File, path)),
            MentionKind::Folder => self
                .text
                .strip_prefix("@folder:")
                .map(|path| Chip::of(ChipKind::Folder, path)),
        }
    }
}

fn parent_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

// --- chips --------------------------------------------------------------------------------------

/// What a chip refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipKind {
    File,
    Folder,
    /// `@diff`, `@staged`, `@git:3`, `@url:https://...`.
    Other,
}

/// A mention shown as one unit in the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub kind: ChipKind,
    /// The text it stands for, the reference the runtime reads: `@file:src/a.rs`.
    pub text: String,
    /// What it shows: `a.rs`, `src/`, `diff`.
    pub label: String,
}

impl Chip {
    /// The chip of a path (quoted in the text when it holds a space).
    pub fn of(kind: ChipKind, path: &str) -> Self {
        let tag = if kind == ChipKind::Folder {
            "folder"
        } else {
            "file"
        };
        let trimmed = path.trim_end_matches('/');
        let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
        Self {
            kind,
            text: format!("@{tag}:{}", format_value(path)),
            label: if kind == ChipKind::Folder {
                format!("{name}/")
            } else {
                name.to_string()
            },
        }
    }

    fn directive(text: &str) -> Self {
        Self {
            kind: ChipKind::Other,
            text: text.to_string(),
            label: text.trim_start_matches('@').to_string(),
        }
    }

    /// The id the input's token carries (`file:src/a.rs`): what it names, not how it looks.
    pub fn id(&self) -> String {
        let tag = match self.kind {
            ChipKind::File => "file",
            ChipKind::Folder => "folder",
            ChipKind::Other => "ref",
        };
        format!("{tag}:{}", self.text)
    }

    /// The kind an input token's id names.
    pub fn kind_of_id(id: &str) -> ChipKind {
        match id.split(':').next() {
            Some("file") => ChipKind::File,
            Some("folder") => ChipKind::Folder,
            _ => ChipKind::Other,
        }
    }
}

/// The longest text scanned for references, in bytes.
const MAX_SCANNED: usize = 200_000;

/// The references in `text`, each with the byte range it fills, as chips: for a draft that is
/// loaded and for text that was pasted (a manual `@file:x` stays plain text while it is typed).
pub fn reference_chips(text: &str) -> Vec<(Range<usize>, Chip)> {
    // No `@`, no reference; and a giant paste is left as the plain text it is (the reference
    // pattern is not cheap, and nothing waits on a chip in a megabyte).
    if text.len() > MAX_SCANNED || !text.contains('@') {
        return Vec::new();
    }
    spans(text)
        .into_iter()
        .filter_map(|(range, reference)| {
            let slice = text.get(range.clone())?;
            if slice.chars().any(|c| c.is_control()) || slice.is_empty() {
                return None;
            }
            let chip = match reference.kind.as_str() {
                "file" => {
                    let mut chip = Chip::of(ChipKind::File, &reference.target);
                    if let Some(start) = reference.line_start {
                        let end = reference.line_end.unwrap_or(start);
                        chip.label = if end == start {
                            format!("{}:{start}", chip.label)
                        } else {
                            format!("{}:{start}-{end}", chip.label)
                        };
                    }
                    chip
                }
                "folder" => Chip::of(ChipKind::Folder, &reference.target),
                "diff" | "staged" => Chip::directive(slice),
                "git" | "url" => Chip {
                    kind: ChipKind::Other,
                    text: slice.to_string(),
                    label: slice.trim_start_matches('@').to_string(),
                },
                _ => return None,
            };
            // The token's text is the text it replaces, byte for byte.
            Some((
                range,
                Chip {
                    text: slice.to_string(),
                    ..chip
                },
            ))
        })
        .collect()
}

// --- the commands ---------------------------------------------------------------------------------

/// Where a `/` entry comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOrigin {
    /// One of the core's coding commands (`/plan`, `/init`, `/review`, `/learn`, ...).
    Builtin,
    /// A report on the thread, answered in the timeline (`/usage`, `/diff`, ...).
    Report,
    /// An action of the app (`/new`, `/settings`, ...).
    App,
    /// A skill or bundle: what it loads is sent as the prompt.
    Skill,
}

impl CommandOrigin {
    /// The badge on the entry (`None`: built in, no badge).
    pub fn badge(self) -> Option<&'static str> {
        match self {
            CommandOrigin::Skill => Some("Skill"),
            _ => None,
        }
    }
}

/// One entry of the `/` menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// Without the slash.
    pub name: String,
    pub description: String,
    /// What follows the name (`[focus]`), shown while the arguments are typed.
    pub hint: Option<String>,
    pub origin: CommandOrigin,
}

impl CommandSpec {
    /// A command of the core's catalog; the hint is ours.
    pub fn from_core(name: &str, description: &str, origin: CommandOrigin) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            hint: argument_hint(name, origin).map(str::to_string),
            origin,
        }
    }

    /// `/name [hint]`.
    pub fn usage(&self) -> String {
        match &self.hint {
            Some(hint) => format!("/{} {hint}", self.name),
            None => format!("/{}", self.name),
        }
    }

    /// Whether anything can follow the name.
    pub fn takes_arguments(&self) -> bool {
        self.hint.is_some()
    }
}

/// The hint for what follows a command's name (`None`: nothing does).
fn argument_hint(name: &str, origin: CommandOrigin) -> Option<&'static str> {
    if origin == CommandOrigin::Skill {
        return Some("[what to do with it]");
    }
    Some(match name {
        "plan" => "[what to plan]",
        "review" => "[what to look at]",
        "learn" => "<what to learn>",
        "sandbox" => "[remove-labels]",
        "insights" => "[days]",
        "diff" => "[git diff arguments]",
        "refine" => "[what to look for]",
        _ => return None,
    })
}

/// The reports the core answers through `Core::run_command` that its catalog does not list.
pub fn report_commands() -> Vec<CommandSpec> {
    let report = |name: &str, description: &str| {
        CommandSpec::from_core(name, description, CommandOrigin::Report)
    };
    vec![
        report("status", "The thread, its model and folder, tokens so far"),
        report("context", "What fills the context window now"),
        report("usage", "Tokens and cost of this thread"),
        report("insights", "Usage across your threads"),
        report("diff", "What this thread changed in the folder"),
        report("tools", "The tools the agent has"),
        report("memory", "What the agent remembers"),
        report(
            "refine",
            "Review this thread for lessons to save to memory and skills",
        ),
    ]
}

/// What an app command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCommand {
    /// A new thread in the project of this one.
    New,
    /// Opens the model picker.
    Model,
    /// Opens the settings.
    Settings,
    /// Shows or hides the terminal drawer.
    Terminal,
}

impl AppCommand {
    /// The command called `name`.
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "new" => Self::New,
            "model" => Self::Model,
            "settings" => Self::Settings,
            "terminal" => Self::Terminal,
            _ => return None,
        })
    }

    /// The workspace command it runs (`keymap::COMMANDS`); the model picker is the composer's.
    pub fn workspace_id(self) -> Option<&'static str> {
        match self {
            Self::New => Some("chat.new"),
            Self::Settings => Some("settings.open"),
            Self::Terminal => Some("terminal.toggle"),
            Self::Model => None,
        }
    }
}

/// The app's own commands.
pub fn app_commands() -> Vec<CommandSpec> {
    let app = |name: &str, description: &str| CommandSpec {
        name: name.to_string(),
        description: description.to_string(),
        hint: None,
        origin: CommandOrigin::App,
    };
    vec![
        app("new", "Start a new thread here"),
        app("model", "Pick the model and the reasoning effort"),
        app("settings", "Open the settings"),
        app("terminal", "Show or hide the terminal"),
    ]
}

/// The core's catalog with the reports and the app's commands. A name the core lists wins: a
/// skill called `status` cannot be run by name anyway (`entries` drops it the same way for the
/// coding commands).
pub fn merged(core: Vec<CommandSpec>) -> Vec<CommandSpec> {
    let mut all = core;
    for extra in report_commands().into_iter().chain(app_commands()) {
        match all.iter().position(|c| c.name == extra.name) {
            // A skill with a built-in's name: the built-in runs.
            Some(ix) if all[ix].origin == CommandOrigin::Skill => all[ix] = extra,
            Some(_) => {}
            None => all.push(extra),
        }
    }
    all
}

/// The entries whose name starts with (else contains) `query`, prefix matches first, then by name
/// (the order of the core's `commands::complete`).
pub fn filter_commands<'a>(all: &'a [CommandSpec], query: &str) -> Vec<&'a CommandSpec> {
    let query = query.to_lowercase();
    let mut matches: Vec<(u8, &CommandSpec)> = all
        .iter()
        .filter_map(|c| {
            let name = c.name.to_lowercase();
            let rank = if name.starts_with(&query) {
                0
            } else if name.contains(&query) {
                1
            } else {
                return None;
            };
            Some((rank, c))
        })
        .collect();
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    matches.into_iter().map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_that_starts_with_at_is_a_mention() {
        let t = trigger("look at @src/ma", 15, &[]).unwrap();
        assert_eq!(
            t,
            Trigger::Mention {
                range: 8..15,
                word: "@src/ma".into()
            }
        );
        // At the start, after a new line, and the bare `@`.
        assert!(matches!(
            trigger("@", 1, &[]),
            Some(Trigger::Mention { .. })
        ));
        assert!(matches!(
            trigger("a\n@x", 4, &[]),
            Some(Trigger::Mention { range, .. }) if range == (2..4)
        ));
    }

    #[test]
    fn an_email_or_a_finished_word_is_not_a_mention() {
        assert_eq!(trigger("mail me@example.com", 19, &[]), None);
        assert_eq!(trigger("@src/main.rs ", 13, &[]), None);
        assert_eq!(trigger("plain", 5, &[]), None);
        // The caret in the middle of the word: what is before it is asked.
        assert!(matches!(
            trigger("@abcdef", 4, &[]),
            Some(Trigger::Mention { word, .. }) if word == "@abc"
        ));
    }

    #[test]
    fn a_caret_in_or_after_a_chip_is_not_a_trigger() {
        let text = "@file:a.rs";
        let chip = [0..10];
        assert_eq!(trigger(text, 4, &chip), None);
        // Right after the chip, with no space: the chip is not a word being typed.
        assert_eq!(trigger(text, 10, &chip), None);
    }

    #[test]
    fn a_slash_at_the_start_is_a_command_and_then_its_arguments() {
        assert_eq!(
            trigger("/us", 3, &[]),
            Some(Trigger::Command { query: "us".into() })
        );
        assert_eq!(
            trigger("/", 1, &[]),
            Some(Trigger::Command { query: "".into() })
        );
        assert_eq!(
            trigger("/diff HEAD~1", 12, &[]),
            Some(Trigger::Arguments {
                name: "diff".into()
            })
        );
        // Not at the start: no command menu (but a mention still is one).
        assert_eq!(trigger("a /us", 5, &[]), None);
        assert!(matches!(
            trigger("/diff @sr", 9, &[]),
            Some(Trigger::Arguments { .. })
        ));
    }

    #[test]
    fn a_typed_text_is_split_into_the_command_and_its_argument() {
        assert_eq!(parse_command("/diff HEAD~1"), Some(("diff", "HEAD~1")));
        assert_eq!(parse_command("/usage"), Some(("usage", "")));
        assert_eq!(
            parse_command("  /plan  a thing "),
            Some(("plan", "a thing"))
        );
        assert_eq!(parse_command("hello /usage"), None);
        assert_eq!(parse_command("/"), None);
        // A path is no command.
        assert_eq!(parse_command("/home/me/file is broken"), None);
    }

    #[test]
    fn the_cores_completions_become_entries() {
        let file = Suggestion::from_item("@file:src/main.rs", "main.rs", "src");
        assert_eq!(file.kind, MentionKind::File);
        assert_eq!(file.label, "main.rs");
        assert_eq!(file.description, "src");
        let folder = Suggestion::from_item("@folder:src/ui/", "ui/", "dir");
        assert_eq!(folder.kind, MentionKind::Folder);
        assert_eq!(folder.description, "src");
        assert_eq!(folder.drill_text().as_deref(), Some("@src/ui/"));
        let prefix = Suggestion::from_item("@file:", "@file:", "attach file");
        assert_eq!(prefix.kind, MentionKind::Prefix);
        assert!(prefix.chip().is_none());
        let diff = Suggestion::from_item("@diff", "@diff", "git diff");
        assert_eq!(diff.kind, MentionKind::Directive);
        assert_eq!(diff.chip().unwrap().text, "@diff");
    }

    #[test]
    fn a_chip_is_the_reference_the_runtime_reads() {
        let chip = Suggestion::from_item("@file:src/main.rs", "main.rs", "src")
            .chip()
            .unwrap();
        assert_eq!(chip.text, "@file:src/main.rs");
        assert_eq!(chip.label, "main.rs");
        assert_eq!(chip.kind, ChipKind::File);
        // A path with a space is quoted so the reference reads back whole.
        let spaced = Chip::of(ChipKind::File, "my docs/a b.txt");
        assert_eq!(spaced.text, "@file:`my docs/a b.txt`");
        assert_eq!(spaced.label, "a b.txt");
        let folder = Chip::of(ChipKind::Folder, "src/ui/");
        assert_eq!(folder.text, "@folder:src/ui/");
        assert_eq!(folder.label, "ui/");
        assert_eq!(Chip::kind_of_id(&folder.id()), ChipKind::Folder);
    }

    #[test]
    fn references_in_a_text_are_found_with_their_ranges() {
        let text = "fix @file:src/a.rs:10-20 and see @folder:docs/ then @diff, ok";
        let chips = reference_chips(text);
        let labels: Vec<_> = chips.iter().map(|(_, c)| c.label.as_str()).collect();
        assert_eq!(labels, ["a.rs:10-20", "docs/", "diff"]);
        for (range, chip) in &chips {
            assert_eq!(
                &text[range.clone()],
                chip.text,
                "the token's text is its range's"
            );
        }
        // Plain text, an email and a url-less mention have none.
        assert!(reference_chips("no references here, me@x.com").is_empty());
        // A quoted path keeps its quotes in the text.
        let quoted = reference_chips("see @file:`a b/c d.txt` now");
        assert_eq!(quoted.len(), 1);
        assert_eq!(quoted[0].1.text, "@file:`a b/c d.txt`");
        assert_eq!(quoted[0].1.label, "c d.txt");
    }

    #[test]
    fn commands_filter_by_prefix_then_by_containing() {
        let all = merged(vec![
            CommandSpec::from_core("plan", "Write a plan", CommandOrigin::Builtin),
            CommandSpec::from_core("review", "Review", CommandOrigin::Builtin),
            CommandSpec::from_core("deploy-plan", "A skill", CommandOrigin::Skill),
        ]);
        let names = |q: &str| -> Vec<String> {
            filter_commands(&all, q)
                .iter()
                .map(|c| c.name.clone())
                .collect()
        };
        assert_eq!(names("pl"), ["plan", "deploy-plan"]);
        assert_eq!(names("us"), ["usage", "status"]);
        assert!(names("zzz").is_empty());
        // The bare slash lists everything, by name.
        assert_eq!(names("").len(), all.len());
    }

    #[test]
    fn the_merged_catalog_has_the_reports_and_the_apps_commands_once() {
        let all = merged(vec![
            CommandSpec::from_core(
                "status",
                "A skill named like a report",
                CommandOrigin::Skill,
            ),
            CommandSpec::from_core("plan", "Plan", CommandOrigin::Builtin),
        ]);
        let status: Vec<_> = all.iter().filter(|c| c.name == "status").collect();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].origin, CommandOrigin::Report, "the built-in runs");
        for name in ["usage", "diff", "new", "model", "settings"] {
            assert!(all.iter().any(|c| c.name == name), "{name}");
        }
        assert_eq!(AppCommand::named("new"), Some(AppCommand::New));
        assert_eq!(AppCommand::named("plan"), None);
        assert_eq!(AppCommand::Settings.workspace_id(), Some("settings.open"));
    }

    #[test]
    fn arguments_have_hints_and_a_usage_line() {
        let diff = CommandSpec::from_core("diff", "", CommandOrigin::Report);
        assert_eq!(diff.usage(), "/diff [git diff arguments]");
        assert!(diff.takes_arguments());
        let usage = CommandSpec::from_core("usage", "", CommandOrigin::Report);
        assert_eq!(usage.usage(), "/usage");
        assert!(!usage.takes_arguments());
        let skill = CommandSpec::from_core("deploy", "", CommandOrigin::Skill);
        assert!(skill.takes_arguments());
    }
}
