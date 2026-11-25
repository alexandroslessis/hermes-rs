//! The questions that wait for the user, as data with no GPUI in it: an approval of a flagged
//! command or file edit (four choices: once, for this session, always, deny) and the agent's
//! clarify questions (choices to pick, free text), the queue they wait in, and what each key does
//! on them. The core's events ([`CoreEvent::ApprovalRequested`], [`CoreEvent::ClarifyRequested`],
//! [`CoreEvent::Settled`]) build the queue ([`Asks::on_event`]); the composer's panel
//! ([`super::panel`]) draws the first request of the open thread; the app model keeps the queue
//! of every thread, so a request for a thread that is not open is waiting when it is opened, and
//! the sidebar knows which thread waits.
//!
//! What a request is, by the core's wording (`hermes_core::questions`): an approval has the
//! `command` text, a `description` (why it was flagged: the runtime's reason, also the key a
//! "session" or "always" answer is remembered by) and the `choices` offered (Manual and Plan
//! offer only `once` and `deny`); an edit is asked as the command `write_file <path>` or
//! `patch <path>` with a description `file edit` or `edit a file ...`
//! (`hermes_tools::SessionTools::check_edit`), which [`Approval::subject`] tells from a terminal
//! command that happens to start with `patch`. A clarify has the question, up to four choices
//! (the core adds "Other": free text is always possible) and `multi_select`; its answer is the
//! text, or the picked choices joined with commas; an empty answer skips it.
//!
//! The keys follow the Tauri page (`apps/desktop/src/components/assistant-ui/tool/approval.tsx`,
//! `lib/keybinds/approval-keys.ts`: Enter allows once, Escape denies; a held key does not repeat
//! onto the next card) and t3code's question panel (`ComposerPendingUserInputPanel.tsx`: digits
//! pick the option with that number, only when no text field has the keyboard).
//!
//! t3code: `components/chat/ComposerPendingApprovalActions.tsx`,
//! `components/chat/TimelinePendingApprovalRow.tsx`, `ComposerPendingUserInputPanel.tsx`,
//! `session-logic.ts` (`PendingApproval`, `PendingUserInput`). Hermes: none (the core's `Ask`).

use hermes_core::CoreEvent;

/// One of Hermes's four answers to a flagged command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Choice {
    Once,
    Session,
    Always,
    Deny,
}

impl Choice {
    /// The core's word for it (`ApprovalRequested::choices`).
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "once" => Some(Choice::Once),
            "session" => Some(Choice::Session),
            "always" => Some(Choice::Always),
            "deny" => Some(Choice::Deny),
            _ => None,
        }
    }

    /// The core's word for it.
    pub fn id(self) -> &'static str {
        match self {
            Choice::Once => "once",
            Choice::Session => "session",
            Choice::Always => "always",
            Choice::Deny => "deny",
        }
    }

    /// The button's text.
    pub fn label(self) -> &'static str {
        match self {
            Choice::Once => "Allow once",
            Choice::Session => "Allow for this session",
            Choice::Always => "Always allow",
            Choice::Deny => "Deny",
        }
    }

    /// What it does, for the tooltip.
    pub fn hint(self) -> &'static str {
        match self {
            Choice::Once => "Run it this once; ask again next time",
            Choice::Session => "Run it, and do not ask again for this kind of command in this thread",
            Choice::Always => {
                "Run it, and add this kind of command to the permanent allowlist (Settings)"
            }
            Choice::Deny => "Do not run it; the agent is told it was denied",
        }
    }
}

/// What an approval is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    /// A shell command.
    Command(String),
    /// A file edit by the `write_file` or `patch` tool.
    Edit { tool: String, path: String },
}

/// A flagged command or edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub command: String,
    /// Why it was flagged.
    pub reason: String,
    /// The answers offered, in the core's order.
    pub choices: Vec<Choice>,
}

impl Approval {
    /// A command or an edit: the core asks an edit as `<tool> <path>` with a reason that starts
    /// `file edit` or `edit a file`; a command whose text starts the same is told apart by it.
    pub fn subject(&self) -> Subject {
        let edit_reason = self.reason == "file edit" || self.reason.starts_with("edit a file");
        if edit_reason
            && let Some((tool, path)) = self.command.split_once(' ')
            && matches!(tool, "write_file" | "patch")
        {
            return Subject::Edit {
                tool: tool.to_string(),
                path: path.trim().to_string(),
            };
        }
        Subject::Command(self.command.clone())
    }

    /// The choice Enter takes: once, else the first offered.
    pub fn default_choice(&self) -> Option<Choice> {
        if self.choices.contains(&Choice::Once) {
            Some(Choice::Once)
        } else {
            self.choices.first().copied()
        }
    }
}

/// The agent's question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    pub choices: Vec<String>,
    pub multi_select: bool,
}

/// What a request asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    Approval(Approval),
    Clarify(Question),
}

/// One question waiting, from the core's event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub prompt_id: String,
    pub session: String,
    pub ask: Ask,
}

impl Request {
    /// The request an event opens, if it is one.
    pub fn from_event(event: &CoreEvent) -> Option<Self> {
        match event {
            CoreEvent::ApprovalRequested {
                session_id,
                prompt_id,
                command,
                description,
                choices,
            } => {
                let mut offered: Vec<Choice> =
                    choices.iter().filter_map(|c| Choice::parse(c)).collect();
                // A request that offers nothing the panel knows must still be answerable.
                if offered.is_empty() {
                    offered = vec![Choice::Once, Choice::Deny];
                }
                if !offered.contains(&Choice::Deny) {
                    offered.push(Choice::Deny);
                }
                Some(Self {
                    prompt_id: prompt_id.clone(),
                    session: session_id.clone(),
                    ask: Ask::Approval(Approval {
                        command: command.clone(),
                        reason: description.clone(),
                        choices: offered,
                    }),
                })
            }
            CoreEvent::ClarifyRequested {
                session_id,
                prompt_id,
                question,
                choices,
                multi_select,
            } => Some(Self {
                prompt_id: prompt_id.clone(),
                session: session_id.clone(),
                ask: Ask::Clarify(Question {
                    text: question.clone(),
                    choices: choices.clone(),
                    multi_select: *multi_select,
                }),
            }),
            _ => None,
        }
    }

    /// What the thread is waiting for, in a few words (the notification's body). It never holds
    /// the command, the path or the question: a toast shows on the lock screen and in the
    /// notification centre, and a command line may carry a token (review, CP5.3).
    pub fn summary(&self) -> String {
        match &self.ask {
            Ask::Approval(a) => match a.subject() {
                Subject::Command(_) => "A command needs your approval".to_string(),
                Subject::Edit { .. } => "A file change needs your approval".to_string(),
            },
            Ask::Clarify(_) => "The agent has a question for you".to_string(),
        }
    }
}

/// `text` as the approval panel shows it: what the runtime will run, with every character that a
/// screen would hide or interpret (control characters, ANSI escapes, bidirectional overrides,
/// zero-width and other format characters) written as a visible escape (`\x1b`, `\u{202e}`), so
/// the text on screen is the text that runs. Newlines stay (a multi-line command shows as lines);
/// a tab is `\t`. The flag says whether anything was escaped.
pub fn visible(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut escaped = false;
    for c in text.chars() {
        if c == '\n' {
            out.push(c);
        } else if c == '\t' {
            out.push_str("\\t");
            escaped = true;
        } else if c == '\r' {
            out.push_str("\\r");
            escaped = true;
        } else if c.is_control() {
            out.push_str(&format!("\\x{:02x}", c as u32));
            escaped = true;
        } else if hidden(c) {
            out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            escaped = true;
        } else {
            out.push(c);
        }
    }
    (out, escaped)
}

/// Characters that draw as nothing or reorder what is around them (Unicode format characters and
/// the like): bidirectional marks and overrides, zero-width spaces and joiners, the BOM, soft
/// hyphen, invisible fillers, tag characters, variation selectors.
fn hidden(c: char) -> bool {
    matches!(c as u32,
        0x00ad | 0x034f | 0x061c | 0x115f | 0x1160 | 0x17b4 | 0x17b5 | 0x180b..=0x180f
        | 0x200b..=0x200f | 0x2028 | 0x2029 | 0x202a..=0x202e | 0x2060..=0x206f | 0x3164
        | 0xfe00..=0xfe0f | 0xfeff | 0xffa0 | 0xfff0..=0xfffb | 0x1d173..=0x1d17a
        | 0xe0000..=0xe0fff)
}

/// Whether `text` has characters outside ASCII (a lookalike such as a Cyrillic `а` in `cаt`):
/// the panel says so under the command.
pub fn non_ascii(text: &str) -> bool {
    text.chars().any(|c| !c.is_ascii())
}

/// `text` on one line, cut at `max` characters with an ellipsis.
pub fn one_line(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// What the user answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Approval(Choice),
    /// A clarify answer: the text, or the picked choices joined with commas (empty skips).
    Text(String),
}

/// The requests waiting, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asks {
    list: Vec<Request>,
}

impl Asks {
    /// Takes in one of the core's events. True when the queue changed.
    ///
    /// A request opens; `Settled` closes it (answered here or elsewhere, the turn was stopped, the
    /// runtime's time ran out); a turn that ended or went idle leaves no question open (the
    /// runtime cannot be waiting for one).
    pub fn on_event(&mut self, event: &CoreEvent) -> bool {
        if let Some(request) = Request::from_event(event) {
            return self.push(request);
        }
        match event {
            CoreEvent::Settled { prompt_id, .. } => self.remove(prompt_id),
            CoreEvent::TurnEnded { session_id, .. } | CoreEvent::Idle { session_id } => {
                self.clear_session(session_id)
            }
            _ => false,
        }
    }

    /// Adds a request (one with the same id is already there: nothing changes).
    pub fn push(&mut self, request: Request) -> bool {
        if self.list.iter().any(|r| r.prompt_id == request.prompt_id) {
            return false;
        }
        self.list.push(request);
        true
    }

    /// Removes a request; true when it was there.
    pub fn remove(&mut self, prompt_id: &str) -> bool {
        let before = self.list.len();
        self.list.retain(|r| r.prompt_id != prompt_id);
        self.list.len() != before
    }

    /// Removes every request of a session; true when there were any.
    pub fn clear_session(&mut self, session: &str) -> bool {
        let before = self.list.len();
        self.list.retain(|r| r.session != session);
        self.list.len() != before
    }

    /// The session's requests, oldest first.
    pub fn of<'a>(&'a self, session: &'a str) -> impl Iterator<Item = &'a Request> {
        self.list.iter().filter(move |r| r.session == session)
    }

    /// The request the session shows now: its oldest.
    pub fn first(&self, session: &str) -> Option<&Request> {
        self.list.iter().find(|r| r.session == session)
    }

    /// How many requests the session has waiting.
    pub fn count(&self, session: &str) -> usize {
        self.of(session).count()
    }

    /// Whether the session waits for the user.
    pub fn waiting(&self, session: &str) -> bool {
        self.first(session).is_some()
    }

    /// Whether only questions (no approval) wait in the session.
    pub fn only_questions(&self, session: &str) -> bool {
        self.waiting(session) && self.of(session).all(|r| matches!(r.ask, Ask::Clarify(_)))
    }

    /// Every request.
    pub fn all(&self) -> &[Request] {
        &self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// The key a digit stands for: `1` is the first option (index 0), up to `9`; `None` for any
/// other text.
pub fn digit_index(key: &str) -> Option<usize> {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    match c.to_digit(10) {
        Some(d @ 1..=9) => Some(d as usize - 1),
        _ => None,
    }
}

/// The answer text of a clarify: the picked choices joined with commas (a choice that holds a
/// comma would be split by the core, so the picks are what the core re-splits), or the free text
/// when it is not empty.
pub fn clarify_answer(free_text: &str, picked: &[String]) -> String {
    let text = free_text.trim();
    if !text.is_empty() {
        return text.to_string();
    }
    picked.join(", ")
}

/// What a key did to the request on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// The key is not the panel's.
    Ignored,
    /// The highlight or the picks moved.
    Moved,
    /// The user answered.
    Answer(Answer),
    /// The free-text field takes the keyboard.
    FocusField,
}

/// Where the keyboard is in the request on screen: the highlighted row and, for a multi-select,
/// the picked ones. A clarify's rows are its choices and then "Other" (the free-text field).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Nav {
    pub highlight: Option<usize>,
    pub picked: Vec<usize>,
    /// "Always allow" was chosen and waits for its confirmation.
    pub confirm_always: bool,
}

impl Nav {
    /// The start of a request: an approval highlights what Enter does.
    pub fn start(ask: &Ask) -> Self {
        match ask {
            Ask::Approval(a) => Self {
                highlight: a
                    .default_choice()
                    .and_then(|c| a.choices.iter().position(|x| *x == c)),
                picked: Vec::new(),
                confirm_always: false,
            },
            Ask::Clarify(_) => Self::default(),
        }
    }

    /// How many rows the keys move over.
    pub fn rows(ask: &Ask) -> usize {
        match ask {
            Ask::Approval(a) => a.choices.len(),
            Ask::Clarify(q) => q.choices.len() + 1,
        }
    }

    fn step(&mut self, rows: usize, delta: isize) {
        if rows == 0 {
            return;
        }
        self.highlight = Some(match (self.highlight, delta > 0) {
            (None, true) => 0,
            (None, false) => rows - 1,
            (Some(i), true) => (i + 1) % rows,
            (Some(i), false) => (i + rows - 1) % rows,
        });
    }

    /// An approval's choice taken by a key or a click: "Always allow" first asks for its
    /// confirmation ([`Nav::confirm_always`]); the others answer.
    pub fn pick(&mut self, choice: Choice) -> Reaction {
        if choice == Choice::Always {
            self.confirm_always = true;
            Reaction::Moved
        } else {
            Reaction::Answer(Answer::Approval(choice))
        }
    }

    fn toggle(&mut self, ix: usize) {
        match self.picked.iter().position(|p| *p == ix) {
            Some(at) => {
                self.picked.remove(at);
            }
            None => self.picked.push(ix),
        }
    }

    /// The picked choices' texts, in the order of the choices.
    pub fn picked_texts(&self, q: &Question) -> Vec<String> {
        let mut ixs = self.picked.clone();
        ixs.sort_unstable();
        ixs.into_iter()
            .filter_map(|i| q.choices.get(i).cloned())
            .collect()
    }

    /// One key (`enter`, `escape`, `up`, `down`, `tab`, `space`, a digit) pressed with the panel
    /// focused. `typed` is what the free-text field holds. Keys that mean more than one thing
    /// are the Tauri page's (Enter allows once, Escape denies) and t3code's (digits pick).
    pub fn key(&mut self, ask: &Ask, key: &str, typed: &str) -> Reaction {
        let rows = Self::rows(ask);
        // "Always allow" persists the flagged kind of command to the permanent allowlist: it
        // asks first (the Tauri page's confirm dialog). While it asks, Enter confirms and Escape
        // goes back; nothing else answers.
        if self.confirm_always && matches!(ask, Ask::Approval(_)) {
            return match key {
                "enter" => Reaction::Answer(Answer::Approval(Choice::Always)),
                "escape" => {
                    self.confirm_always = false;
                    Reaction::Moved
                }
                _ => Reaction::Ignored,
            };
        }
        match ask {
            Ask::Approval(a) => match key {
                "enter" => match self.highlight.and_then(|i| a.choices.get(i)).copied() {
                    Some(choice) => self.pick(choice),
                    None => match a.default_choice() {
                        Some(choice) => self.pick(choice),
                        None => Reaction::Ignored,
                    },
                },
                "escape" => Reaction::Answer(Answer::Approval(Choice::Deny)),
                "down" | "tab" => {
                    self.step(rows, 1);
                    Reaction::Moved
                }
                "up" => {
                    self.step(rows, -1);
                    Reaction::Moved
                }
                other => match digit_index(other).and_then(|i| a.choices.get(i)) {
                    Some(choice) => self.pick(*choice),
                    None => Reaction::Ignored,
                },
            },
            Ask::Clarify(q) => {
                let choices = q.choices.len();
                // Selecting row `i`: a single-select answers, a multi-select toggles.
                let select = |nav: &mut Nav, i: usize| -> Reaction {
                    if i >= choices {
                        return Reaction::FocusField;
                    }
                    if q.multi_select {
                        nav.toggle(i);
                        Reaction::Moved
                    } else {
                        Reaction::Answer(Answer::Text(q.choices[i].clone()))
                    }
                };
                match key {
                    "escape" => Reaction::Answer(Answer::Text(String::new())),
                    "tab" => Reaction::FocusField,
                    "down" => {
                        self.step(rows, 1);
                        Reaction::Moved
                    }
                    "up" => {
                        self.step(rows, -1);
                        Reaction::Moved
                    }
                    "space" => match self.highlight {
                        Some(i) => select(self, i),
                        None => Reaction::Ignored,
                    },
                    "enter" => {
                        if !typed.trim().is_empty() {
                            return Reaction::Answer(Answer::Text(typed.trim().to_string()));
                        }
                        if q.multi_select && !self.picked.is_empty() {
                            let picked = self.picked_texts(q);
                            return Reaction::Answer(Answer::Text(clarify_answer("", &picked)));
                        }
                        match self.highlight {
                            Some(i) => select(self, i),
                            None => Reaction::Ignored,
                        }
                    }
                    other => match digit_index(other) {
                        Some(i) if i <= choices => select(self, i),
                        _ => Reaction::Ignored,
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(id: &str, session: &str, command: &str, reason: &str, choices: &[&'static str]) -> CoreEvent {
        CoreEvent::ApprovalRequested {
            session_id: session.into(),
            prompt_id: id.into(),
            command: command.into(),
            description: reason.into(),
            choices: choices.to_vec(),
        }
    }

    fn clarify(id: &str, session: &str) -> CoreEvent {
        CoreEvent::ClarifyRequested {
            session_id: session.into(),
            prompt_id: id.into(),
            question: "Which one?".into(),
            choices: vec!["a".into(), "b".into()],
            multi_select: false,
        }
    }

    fn settled(id: &str, session: &str) -> CoreEvent {
        CoreEvent::Settled {
            session_id: session.into(),
            prompt_id: id.into(),
            kind: hermes_core::QuestionKind::Approval,
        }
    }

    #[test]
    fn requests_queue_in_order_per_session_and_settle_by_id() {
        let mut asks = Asks::default();
        assert!(asks.on_event(&approval("p1", "s1", "rm -r x", "recursive delete", &["once", "deny"])));
        assert!(asks.on_event(&clarify("p2", "s1")));
        assert!(asks.on_event(&clarify("p3", "s2")));
        assert_eq!(asks.count("s1"), 2);
        assert_eq!(asks.first("s1").unwrap().prompt_id, "p1");
        assert!(asks.waiting("s2") && !asks.waiting("s3"));
        // The same request twice is one.
        assert!(!asks.on_event(&clarify("p2", "s1")));
        assert_eq!(asks.count("s1"), 2);
        // Settling the first shows the second; settling again is nothing.
        assert!(asks.on_event(&settled("p1", "s1")));
        assert_eq!(asks.first("s1").unwrap().prompt_id, "p2");
        assert!(!asks.on_event(&settled("p1", "s1")));
        assert!(asks.waiting("s2"), "another thread's request stays");
    }

    #[test]
    fn a_session_that_idles_has_no_open_question() {
        let mut asks = Asks::default();
        asks.on_event(&clarify("p1", "s1"));
        asks.on_event(&clarify("p2", "s2"));
        assert!(asks.on_event(&CoreEvent::Idle { session_id: "s1".into() }));
        assert!(!asks.waiting("s1") && asks.waiting("s2"));
    }

    #[test]
    fn the_offered_choices_always_include_deny_and_are_never_empty() {
        let request = |choices: &[&'static str]| {
            let Some(Request { ask: Ask::Approval(a), .. }) =
                Request::from_event(&approval("p", "s", "c", "r", choices))
            else {
                panic!("an approval");
            };
            a
        };
        assert_eq!(
            request(&["once", "session", "always", "deny"]).choices,
            [Choice::Once, Choice::Session, Choice::Always, Choice::Deny]
        );
        assert_eq!(request(&["once"]).choices, [Choice::Once, Choice::Deny]);
        assert_eq!(request(&[]).choices, [Choice::Once, Choice::Deny]);
        assert_eq!(request(&["session", "deny"]).default_choice(), Some(Choice::Session));
        assert_eq!(request(&["once", "deny"]).default_choice(), Some(Choice::Once));
    }

    #[test]
    fn an_edit_is_told_from_a_command_by_the_reason() {
        let ask = |command: &str, reason: &str| Approval {
            command: command.into(),
            reason: reason.into(),
            choices: vec![Choice::Once, Choice::Deny],
        };
        assert_eq!(
            ask("write_file src/a b.rs", "file edit").subject(),
            Subject::Edit { tool: "write_file".into(), path: "src/a b.rs".into() }
        );
        assert_eq!(
            ask("patch /etc/x", "edit a file outside the working folder").subject(),
            Subject::Edit { tool: "patch".into(), path: "/etc/x".into() }
        );
        // The unix `patch` command is a command.
        assert_eq!(
            ask("patch -p1 < fix.diff", "run a command").subject(),
            Subject::Command("patch -p1 < fix.diff".into())
        );
        assert_eq!(
            ask("rm -rf build", "recursive delete").subject(),
            Subject::Command("rm -rf build".into())
        );
    }

    #[test]
    fn hidden_and_control_characters_are_shown_escaped() {
        assert_eq!(visible("ls -la"), ("ls -la".into(), false));
        assert_eq!(visible("a\nb"), ("a\nb".into(), false), "lines stay lines");
        assert_eq!(visible("echo \x1b[2Jhi"), ("echo \\x1b[2Jhi".into(), true));
        assert_eq!(visible("a\tb\rc"), ("a\\tb\\rc".into(), true));
        // A right-to-left override would show the end of the line reversed.
        assert_eq!(visible("cat \u{202e}exe.txt"), ("cat \\u{202e}exe.txt".into(), true));
        assert_eq!(visible("r\u{200b}m"), ("r\\u{200b}m".into(), true));
        assert_eq!(visible("\u{feff}x").0, "\\u{feff}x");
        assert!(non_ascii("c\u{0430}t file") && !non_ascii("cat file"));
    }

    #[test]
    fn a_notice_never_holds_the_command() {
        let request = Request::from_event(&approval(
            "p",
            "s",
            "curl -H 'Authorization: Bearer sk-secret' x",
            "network",
            &["once", "deny"],
        ))
        .unwrap();
        assert!(!request.summary().contains("sk-secret"));
        assert!(request.summary().contains("command"));
    }

    #[test]
    fn digits_and_answers() {
        assert_eq!(digit_index("1"), Some(0));
        assert_eq!(digit_index("9"), Some(8));
        assert_eq!(digit_index("0"), None);
        assert_eq!(digit_index("10"), None);
        assert_eq!(digit_index("a"), None);
        assert_eq!(clarify_answer("  my text ", &["a".into()]), "my text");
        assert_eq!(clarify_answer("", &["a".into(), "b".into()]), "a, b");
        assert_eq!(clarify_answer(" ", &[]), "");
        assert_eq!(one_line("a\n  b\tc", 20), "a b c");
        assert_eq!(one_line("abcdefghij", 5), "abcd…");
    }
    fn approval_ask(choices: &[Choice]) -> Ask {
        Ask::Approval(Approval {
            command: "rm -rf x".into(),
            reason: "recursive delete".into(),
            choices: choices.to_vec(),
        })
    }

    fn question(choices: &[&str], multi: bool) -> Ask {
        Ask::Clarify(Question {
            text: "Which?".into(),
            choices: choices.iter().map(|c| c.to_string()).collect(),
            multi_select: multi,
        })
    }

    #[test]
    fn approval_keys_enter_allows_once_escape_denies_digits_pick() {
        let all = approval_ask(&[Choice::Once, Choice::Session, Choice::Always, Choice::Deny]);
        let mut nav = Nav::start(&all);
        assert_eq!(nav.highlight, Some(0), "Enter's choice is highlighted");
        assert_eq!(nav.key(&all, "enter", ""), Reaction::Answer(Answer::Approval(Choice::Once)));
        assert_eq!(nav.key(&all, "escape", ""), Reaction::Answer(Answer::Approval(Choice::Deny)));
        assert_eq!(nav.key(&all, "2", ""), Reaction::Answer(Answer::Approval(Choice::Session)));
        // "Always" asks first: Escape goes back, Enter confirms, other keys do nothing.
        assert_eq!(nav.key(&all, "3", ""), Reaction::Moved);
        assert!(nav.confirm_always);
        assert_eq!(nav.key(&all, "1", ""), Reaction::Ignored, "no other answer while it asks");
        assert_eq!(nav.key(&all, "escape", ""), Reaction::Moved, "back, not a denial");
        assert!(!nav.confirm_always);
        assert_eq!(nav.key(&all, "3", ""), Reaction::Moved);
        assert_eq!(nav.key(&all, "enter", ""), Reaction::Answer(Answer::Approval(Choice::Always)));
        nav.confirm_always = false;
        assert_eq!(nav.key(&all, "4", ""), Reaction::Answer(Answer::Approval(Choice::Deny)));
        assert_eq!(nav.key(&all, "5", ""), Reaction::Ignored);
        assert_eq!(nav.key(&all, "x", ""), Reaction::Ignored);
        // Arrows move the highlight, wrapping; Enter takes it.
        assert_eq!(nav.key(&all, "down", ""), Reaction::Moved);
        assert_eq!(nav.highlight, Some(1));
        assert_eq!(nav.key(&all, "up", ""), Reaction::Moved);
        assert_eq!(nav.key(&all, "up", ""), Reaction::Moved);
        assert_eq!(nav.highlight, Some(3), "wraps to the last");
        assert_eq!(nav.key(&all, "enter", ""), Reaction::Answer(Answer::Approval(Choice::Deny)));
    }

    #[test]
    fn a_mode_that_asks_every_time_offers_two_choices_and_the_other_digits_do_nothing() {
        let two = approval_ask(&[Choice::Once, Choice::Deny]);
        let mut nav = Nav::start(&two);
        assert_eq!(nav.key(&two, "2", ""), Reaction::Answer(Answer::Approval(Choice::Deny)));
        assert_eq!(nav.key(&two, "3", ""), Reaction::Ignored);
        assert_eq!(nav.key(&two, "down", ""), Reaction::Moved);
        assert_eq!(nav.key(&two, "down", ""), Reaction::Moved);
        assert_eq!(nav.highlight, Some(0));
    }

    #[test]
    fn a_single_select_question_answers_with_the_choice() {
        let ask = question(&["blue", "green"], false);
        let mut nav = Nav::start(&ask);
        assert_eq!(nav.highlight, None);
        assert_eq!(nav.key(&ask, "enter", ""), Reaction::Ignored, "nothing chosen");
        assert_eq!(nav.key(&ask, "2", ""), Reaction::Answer(Answer::Text("green".into())));
        assert_eq!(nav.key(&ask, "down", ""), Reaction::Moved);
        assert_eq!(nav.key(&ask, "enter", ""), Reaction::Answer(Answer::Text("blue".into())));
        // The row after the choices is the free-text field.
        assert_eq!(nav.key(&ask, "3", ""), Reaction::FocusField);
        assert_eq!(nav.key(&ask, "tab", ""), Reaction::FocusField);
        assert_eq!(nav.key(&ask, "4", ""), Reaction::Ignored);
        // What is typed wins over a highlighted row.
        assert_eq!(nav.key(&ask, "enter", " mine "), Reaction::Answer(Answer::Text("mine".into())));
        // Escape skips: an empty answer.
        assert_eq!(nav.key(&ask, "escape", ""), Reaction::Answer(Answer::Text(String::new())));
    }

    #[test]
    fn a_multi_select_toggles_and_sends_the_picks_in_choice_order() {
        let ask = question(&["a", "b", "c"], true);
        let mut nav = Nav::start(&ask);
        assert_eq!(nav.key(&ask, "3", ""), Reaction::Moved);
        assert_eq!(nav.key(&ask, "1", ""), Reaction::Moved);
        assert_eq!(nav.picked, [2, 0]);
        assert_eq!(nav.key(&ask, "enter", ""), Reaction::Answer(Answer::Text("a, c".into())));
        // Toggling again takes a pick back; with none picked Enter toggles the highlighted row.
        assert_eq!(nav.key(&ask, "3", ""), Reaction::Moved);
        assert_eq!(nav.key(&ask, "1", ""), Reaction::Moved);
        assert!(nav.picked.is_empty());
        assert_eq!(nav.key(&ask, "down", ""), Reaction::Moved);
        assert_eq!(nav.key(&ask, "space", ""), Reaction::Moved);
        assert_eq!(nav.picked, [0]);
    }

    #[test]
    fn a_question_with_no_choices_has_only_the_field() {
        let ask = question(&[], false);
        let mut nav = Nav::start(&ask);
        assert_eq!(Nav::rows(&ask), 1);
        assert_eq!(nav.key(&ask, "1", ""), Reaction::FocusField);
        assert_eq!(nav.key(&ask, "enter", ""), Reaction::Ignored);
        assert_eq!(nav.key(&ask, "enter", "yes"), Reaction::Answer(Answer::Text("yes".into())));
    }
}
