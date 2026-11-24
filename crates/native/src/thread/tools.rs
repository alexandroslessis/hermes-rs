//! What a tool call looks like in the timeline, with no GPUI in it: [`ToolCard`], built from the
//! call's name, its arguments and its result text, exactly as the core sends and stores them
//! (`CoreEvent::ToolStarted { name, args }`, `ToolCompleted { text }`, the stored `ToolEntry`).
//! The same card comes out of a live call and out of the stored one ([`ToolCard::from_stored`]); a
//! test replays a real core's turn and compares them.
//!
//! A call failed when the core says so (`ToolCompleted::is_error`, the stored
//! `ToolEntry::is_error`: the harness's own error, or a result the tool flagged) or when its JSON
//! says `success: false` / a non-empty `error` (a command that timed out, a result from before the
//! flag). A command that exits non-zero ran: it shows its exit code, it is not a failed call.
//! The tools (`crates/tools`) answer with JSON text, possibly followed by the harness's notes
//! (read past: [`leading_json`]), per tool:
//! - `terminal`: `{output, exit_code, cwd, duration_ms, output_file?, note?, timed_out?}`;
//! - `patch`: `{success, diff (unified), files_modified, replacements}`; `write_file`:
//!   `{success, path, bytes_written}` (the content is in the arguments);
//! - `read_file`: `{content, total_lines, file_size}` or `{files: [..]}` for `paths`;
//! - `search_files`: `{matches | groups | files | counts, total_count}`;
//! - `web_search`: `{data: {web: [..]}}`, `web_extract`: `{results: [{url, title, content, error}]}`;
//! - `delegate_task`: `{results: [{task_index, status, summary, tool_trace, duration_seconds}]}`;
//! - `memory`, `skill_manage`: `{success, message}`.
//! Any other tool (todo, clarify, vision, process, MCP...) gets the generic card: its name, its
//! arguments and its result, as they are.
//!
//! Output is kept raw until the card is opened ([`Output::ensure`]): a thread with hundreds of
//! stored commands does not parse them all when it opens.
//!
//! t3code: `apps/web/src/components/chat/TimelineWorkEntryRow.tsx` (what a row says),
//! `packages/client-runtime/src/work-log/presentation.ts` (`toolGroupAction`, the labels:
//! `Read 3 files`, `Ran 2 commands`), `CommandBlock.tsx` (`$ command`).

use std::sync::Arc;

use serde_json::Value;

use super::{ansi, diff};

/// A card's family: what it looks like and which actions it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Command,
    Edit,
    Read,
    Search,
    Web,
    Delegate,
    Memory,
    Skill,
    Generic,
}

impl ToolKind {
    /// The family of tool `name`.
    pub fn of(name: &str) -> Self {
        match name {
            "terminal" => ToolKind::Command,
            "patch" | "write_file" => ToolKind::Edit,
            "read_file" => ToolKind::Read,
            "search_files" | "session_search" => ToolKind::Search,
            "web_search" | "web_extract" => ToolKind::Web,
            "delegate_task" => ToolKind::Delegate,
            "memory" => ToolKind::Memory,
            "skill_manage" => ToolKind::Skill,
            _ => ToolKind::Generic,
        }
    }
}

/// Where a call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Failed,
}

/// Text shown as lines: kept raw, parsed into styled lines when the card is opened.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub raw: Arc<str>,
    lines: Option<Arc<Vec<ansi::Line>>>,
}

/// The most text a card keeps (a result is already limited by its tool; this bounds a hostile
/// or runaway one).
const MAX_RAW: usize = 400_000;

impl Output {
    pub fn new(raw: &str) -> Self {
        let raw = if raw.len() > MAX_RAW {
            let mut end = MAX_RAW;
            while !raw.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "{}\n[... {} more bytes not shown ...]",
                &raw[..end],
                raw.len() - end
            )
        } else {
            raw.to_string()
        };
        Self {
            raw: raw.into(),
            lines: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.raw.trim().is_empty()
    }

    /// Parses the text if that was not done yet (when the card opens, or its live text changes
    /// while it is open).
    pub fn ensure(&mut self) {
        if self.lines.is_none() {
            self.lines = Some(Arc::new(ansi::parse(&self.raw)));
        }
    }

    /// The parsed lines, once [`Self::ensure`] ran.
    pub fn lines(&self) -> Option<&Arc<Vec<ansi::Line>>> {
        self.lines.as_ref()
    }

    /// A line count without parsing.
    pub fn line_count(&self) -> usize {
        self.lines
            .as_ref()
            .map_or_else(|| self.raw.lines().count(), |l| l.len())
    }
}

/// The screen a memory or skill write links to (the screens are Phase 7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// `target`: `memory` or `user`.
    Memory(String),
    /// The skill's name.
    Skill(String),
}

impl Screen {
    pub fn label(&self) -> String {
        match self {
            Screen::Memory(target) if target == "user" => "Open your profile".to_string(),
            Screen::Memory(_) => "Open memory".to_string(),
            Screen::Skill(name) => format!("Open skill {name}"),
        }
    }
}

/// One tool a subagent used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubTool {
    pub name: String,
    pub preview: String,
    pub failed: bool,
}

/// One task of a `delegate_task` call.
#[derive(Clone, Debug, PartialEq)]
pub struct Subtask {
    /// The subagent's id while it runs (live events name it), `None` from a result.
    pub id: Option<String>,
    pub goal: String,
    pub status: Status,
    pub summary: String,
    pub duration_s: Option<f64>,
    pub tools: Vec<SubTool>,
    pub open: bool,
}

/// What a card shows below its line when it is opened.
#[derive(Clone, Debug, PartialEq)]
pub enum Detail {
    None,
    Command {
        command: String,
        cwd: Option<String>,
        note: Option<String>,
        output: Output,
    },
    Edit {
        path: String,
        additions: u32,
        deletions: u32,
        diff: Arc<diff::Diff>,
        /// The diff is built from the arguments (the result carries none): no context lines.
        from_args: bool,
    },
    /// The result as plain text (read, search, web).
    Text(Output),
    Delegate(Vec<Subtask>),
    Write {
        message: String,
        link: Screen,
    },
    Generic {
        args: String,
        result: Output,
    },
}

/// One tool call as a card.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCard {
    pub kind: ToolKind,
    pub name: String,
    pub args: Value,
    /// The line: what was done (`cargo test`, `Edited src/a.rs`, `Read notes.md`).
    pub title: String,
    /// After the title, quieter: counts, a range, or the error of a failed call.
    pub subtitle: String,
    pub status: Status,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
    pub detail: Detail,
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    line.chars().take(300).collect()
}

/// `Read 3 files`, `1 file`.
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The host of a URL (`https://example.com/a?b` is `example.com`), or the text as it is.
pub fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

impl ToolCard {
    /// A call that has just started: what the arguments already say.
    pub fn started(name: &str, args: &Value) -> Self {
        let kind = ToolKind::of(name);
        let mut card = Self {
            kind,
            name: name.to_string(),
            args: args.clone(),
            title: String::new(),
            subtitle: String::new(),
            status: Status::Running,
            exit_code: None,
            duration_ms: None,
            detail: Detail::None,
        };
        card.describe_call();
        card
    }

    /// A stored call; `result` is `None` for one that never got its result (its turn was
    /// stopped); `is_error` is the stored error flag (`transcript::ToolEntry::is_error`).
    pub fn from_stored(name: &str, args: &Value, result: Option<&str>, is_error: bool) -> Self {
        let mut card = Self::started(name, args);
        match result {
            Some(result) => card.finish(result, is_error),
            None => {
                card.status = Status::Failed;
                card.subtitle = "No result was stored (the turn was stopped)".to_string();
            }
        }
        card
    }

    /// The turn ended with this call unfinished (it was stopped): it stops spinning.
    pub fn interrupt(&mut self) {
        if self.status == Status::Running {
            self.status = Status::Failed;
            self.subtitle = "Stopped before it finished".to_string();
        }
        if let Detail::Delegate(tasks) = &mut self.detail {
            for task in tasks.iter_mut().filter(|t| t.status == Status::Running) {
                task.status = Status::Failed;
            }
        }
    }

    /// Whether the card has something to open.
    pub fn expandable(&self) -> bool {
        match &self.detail {
            Detail::None => false,
            Detail::Command { output, note, .. } => {
                !output.is_empty() || note.is_some() || self.status == Status::Running
            }
            Detail::Edit { diff, .. } => !diff.lines.is_empty(),
            Detail::Text(output) => !output.is_empty(),
            Detail::Delegate(tasks) => !tasks.is_empty(),
            Detail::Write { message, .. } => !message.is_empty(),
            Detail::Generic { args, result } => !args.is_empty() || !result.is_empty(),
        }
    }

    /// Makes what the card shows when open ready (parses its text).
    pub fn prepare_open(&mut self) {
        match &mut self.detail {
            Detail::Command { output, .. } | Detail::Text(output) => output.ensure(),
            Detail::Generic { result, .. } => result.ensure(),
            _ => {}
        }
    }

    /// The live output of a running command: the tail so far (each progress event carries the
    /// whole tail). `open` parses it now.
    pub fn progress(&mut self, tail: &str, open: bool) {
        if let Detail::Command { output, .. } = &mut self.detail {
            *output = Output::new(tail);
            if open {
                output.ensure();
            }
        }
    }

    fn describe_call(&mut self) {
        let args = &self.args;
        match self.kind {
            ToolKind::Command => {
                let command = str_of(args, "command").unwrap_or_default();
                self.title = first_line(command);
                let background = args.get("background").and_then(Value::as_bool) == Some(true);
                self.detail = Detail::Command {
                    command: command.to_string(),
                    cwd: str_of(args, "workdir").map(str::to_string),
                    note: background.then(|| "Running in the background.".to_string()),
                    output: Output::default(),
                };
            }
            ToolKind::Edit => {
                let path = str_of(args, "path").unwrap_or("a file").to_string();
                let (diff, from_args) = if self.name == "patch" {
                    (
                        diff::from_strings(
                            str_of(args, "old_string").unwrap_or_default(),
                            str_of(args, "new_string").unwrap_or_default(),
                        ),
                        true,
                    )
                } else {
                    (
                        diff::all_added(str_of(args, "content").unwrap_or_default()),
                        false,
                    )
                };
                self.title = format!(
                    "{} {path}",
                    if self.name == "patch" {
                        "Editing"
                    } else {
                        "Writing"
                    }
                );
                self.detail = Detail::Edit {
                    path,
                    additions: diff.additions,
                    deletions: diff.deletions,
                    diff: Arc::new(diff),
                    from_args,
                };
            }
            ToolKind::Read => {
                let paths = args.get("paths").and_then(Value::as_array);
                if let Some(paths) = paths.filter(|p| !p.is_empty()) {
                    self.title = format!("Read {}", plural(paths.len(), "file", "files"));
                } else {
                    self.title = format!("Read {}", str_of(args, "path").unwrap_or("a file"));
                    let offset = args.get("offset").and_then(Value::as_u64);
                    let limit = args.get("limit").and_then(Value::as_u64);
                    self.subtitle = match (offset, limit) {
                        (Some(o), Some(l)) => format!("lines {o}\u{2013}{}", o + l.max(1) - 1),
                        (Some(o), None) => format!("from line {o}"),
                        (None, Some(l)) => format!("first {l} lines"),
                        _ => String::new(),
                    };
                }
                self.detail = Detail::Text(Output::default());
            }
            ToolKind::Search => {
                let pattern = str_of(args, "pattern")
                    .or_else(|| str_of(args, "query"))
                    .unwrap_or_default();
                let files = str_of(args, "target") == Some("files");
                self.title = format!(
                    "Searched {}\"{pattern}\"",
                    if files { "files for " } else { "" }
                );
                if let Some(path) = str_of(args, "path").filter(|p| !p.is_empty() && *p != ".") {
                    self.subtitle = format!("in {path}");
                }
                self.detail = Detail::Text(Output::default());
            }
            ToolKind::Web => {
                if self.name == "web_search" {
                    self.title = format!(
                        "Searched the web for \"{}\"",
                        str_of(args, "query").unwrap_or_default()
                    );
                } else {
                    let urls: Vec<&str> = args
                        .get("urls")
                        .and_then(Value::as_array)
                        .map(|u| u.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    self.title = match urls.as_slice() {
                        [] => "Fetched a page".to_string(),
                        [one] => format!("Fetched {}", host_of(one)),
                        [first, rest @ ..] => {
                            format!("Fetched {} and {} more", host_of(first), rest.len())
                        }
                    };
                    self.subtitle = urls.first().map(|u| (*u).to_string()).unwrap_or_default();
                }
                self.detail = Detail::Text(Output::default());
            }
            ToolKind::Delegate => {
                let tasks = args
                    .get("tasks")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let subtasks: Vec<Subtask> = tasks
                    .iter()
                    .map(|t| Subtask {
                        id: None,
                        goal: str_of(t, "goal").unwrap_or_default().to_string(),
                        status: Status::Running,
                        summary: String::new(),
                        duration_s: None,
                        tools: Vec::new(),
                        open: false,
                    })
                    .collect();
                self.title = match subtasks.as_slice() {
                    [one] => format!("Delegated: {}", first_line(&one.goal)),
                    many => format!("Delegated {}", plural(many.len(), "task", "tasks")),
                };
                self.detail = Detail::Delegate(subtasks);
            }
            ToolKind::Memory => {
                self.title = memory_title(args);
            }
            ToolKind::Skill => {
                self.title = skill_title(args);
            }
            ToolKind::Generic => {
                self.title = self.name.clone();
                self.subtitle = super::data::tool_summary(&self.name, args);
                let pretty = match args {
                    Value::Object(o) if o.is_empty() => String::new(),
                    Value::Null => String::new(),
                    other => serde_json::to_string_pretty(other).unwrap_or_default(),
                };
                self.detail = Detail::Generic {
                    args: pretty,
                    result: Output::default(),
                };
            }
        }
        if matches!(self.kind, ToolKind::Memory | ToolKind::Skill) {
            self.detail = Detail::Write {
                message: String::new(),
                link: self.link(),
            };
        }
    }

    fn link(&self) -> Screen {
        match self.kind {
            ToolKind::Skill => Screen::Skill(skill_name(&self.args).unwrap_or_default()),
            _ => Screen::Memory(str_of(&self.args, "target").unwrap_or("memory").to_string()),
        }
    }

    /// Fills the card from the call's result text and the core's error flag (`is_error`: the
    /// call failed in the harness, or the tool flagged its result).
    pub fn finish(&mut self, result: &str, is_error: bool) {
        let (json, trailing) = leading_json(result);
        let failed_json = json.as_ref().is_some_and(|v| {
            v.get("success") == Some(&Value::Bool(false))
                || v
                    .get("error")
                    .and_then(Value::as_str)
                    .is_some_and(|e| !e.is_empty())
        });
        self.status = if is_error || failed_json {
            Status::Failed
        } else {
            Status::Done
        };
        let error = if self.status == Status::Failed {
            let text = json
                .as_ref()
                .and_then(|v| str_of(v, "error"))
                .unwrap_or(result);
            Some(first_line(text))
        } else {
            None
        };
        match self.kind {
            ToolKind::Command => self.finish_command(json.as_ref(), result, trailing),
            ToolKind::Edit => self.finish_edit(json.as_ref()),
            ToolKind::Read => self.finish_read(json.as_ref()),
            ToolKind::Search => self.finish_search(json.as_ref()),
            ToolKind::Web => self.finish_web(json.as_ref()),
            ToolKind::Delegate => self.finish_delegate(json.as_ref()),
            ToolKind::Memory | ToolKind::Skill => {
                let message = json
                    .as_ref()
                    .and_then(|v| str_of(v, "message").or_else(|| str_of(v, "error")))
                    .unwrap_or(result)
                    .to_string();
                self.detail = Detail::Write {
                    message,
                    link: self.link(),
                };
            }
            ToolKind::Generic => {
                if let Detail::Generic { result: out, .. } = &mut self.detail {
                    *out = Output::new(&pretty_result(json.as_ref(), result));
                }
            }
        }
        if let Some(error) = error {
            self.subtitle = error;
        }
    }

    fn finish_command(&mut self, json: Option<&Value>, raw: &str, trailing: &str) {
        let Detail::Command {
            output, note, cwd, ..
        } = &mut self.detail
        else {
            return;
        };
        match json {
            Some(v) => {
                let text = str_of(v, "output").unwrap_or_default();
                *output = Output::new(text);
                self.exit_code = v.get("exit_code").and_then(Value::as_i64);
                self.duration_ms = v.get("duration_ms").and_then(Value::as_u64);
                if let Some(dir) = str_of(v, "cwd") {
                    *cwd = Some(dir.to_string());
                }
                let mut notes: Vec<String> = Vec::new();
                for key in ["approval", "note", "sandbox"] {
                    if let Some(n) = str_of(v, key).filter(|n| !n.is_empty()) {
                        notes.push(n.to_string());
                    }
                }
                if v.get("timed_out").and_then(Value::as_bool) == Some(true)
                    && let Some(e) = str_of(v, "error")
                {
                    notes.push(e.to_string());
                }
                if let Some(file) = str_of(v, "output_file") {
                    notes.push(format!("The full output is saved in {file}."));
                }
                // A background start: no output, a session to manage.
                if output.is_empty()
                    && let Some(id) = str_of(v, "session_id")
                {
                    notes.push(format!("Running in the background (session {id})."));
                }
                // The harness's notes after the result (a loop warning, a repeated call).
                if !trailing.trim().is_empty() {
                    notes.push(trailing.trim().to_string());
                }
                *note = (!notes.is_empty()).then(|| notes.join("\n"));
            }
            None => *output = Output::new(raw),
        }
    }

    fn finish_edit(&mut self, json: Option<&Value>) {
        let Detail::Edit {
            additions,
            deletions,
            diff: shown,
            from_args,
            path,
        } = &mut self.detail
        else {
            return;
        };
        if self.status != Status::Failed {
            let unified = json
                .and_then(|v| str_of(v, "diff"))
                .filter(|d| !d.is_empty());
            if let Some(unified) = unified {
                let parsed = diff::parse_unified(unified);
                *additions = parsed.additions;
                *deletions = parsed.deletions;
                *shown = Arc::new(parsed);
                *from_args = false;
            }
            if let Some(modified) = json
                .and_then(|v| v.get("files_modified"))
                .and_then(Value::as_array)
                && let Some(first) = modified.first().and_then(Value::as_str)
            {
                *path = first.to_string();
            }
            if let Some(shown_path) = json.and_then(|v| str_of(v, "path")) {
                *path = shown_path.to_string();
            }
            if json
                .and_then(|v| v.get("no_change"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                *additions = 0;
                *deletions = 0;
                *shown = Arc::new(diff::Diff::default());
            }
        } else {
            *additions = 0;
            *deletions = 0;
        }
        let verb = if self.name == "patch" {
            "Edited"
        } else {
            "Wrote"
        };
        self.title = format!(
            "{} {}",
            if self.status == Status::Failed {
                "Could not edit"
            } else {
                verb
            },
            path
        );
    }

    fn finish_read(&mut self, json: Option<&Value>) {
        let mut text = String::new();
        if let Some(v) = json {
            if let Some(files) = v.get("files").and_then(Value::as_array) {
                let ok = v.get("files_read").and_then(Value::as_u64).unwrap_or(0);
                let failed = v.get("files_failed").and_then(Value::as_u64).unwrap_or(0);
                self.subtitle = if failed > 0 {
                    format!("{ok} read, {failed} failed")
                } else {
                    String::new()
                };
                for file in files {
                    text.push_str(&format!(
                        "== {} ==\n{}\n",
                        str_of(file, "path").unwrap_or("?"),
                        str_of(file, "content")
                            .or_else(|| str_of(file, "error"))
                            .unwrap_or_default()
                    ));
                }
            } else {
                if let Some(content) = str_of(v, "content") {
                    text.push_str(content);
                }
                if self.subtitle.is_empty()
                    && let Some(total) = v.get("total_lines").and_then(Value::as_u64)
                {
                    self.subtitle = plural(total as usize, "line", "lines");
                }
                if let Some(hint) = str_of(v, "hint") {
                    text.push_str(hint);
                }
            }
        }
        self.detail = Detail::Text(Output::new(&text));
    }

    fn finish_search(&mut self, json: Option<&Value>) {
        let mut text = String::new();
        if let Some(v) = json {
            let total = v.get("total_count").and_then(Value::as_u64).unwrap_or(0);
            let (one, many) = if v.get("files").is_some() {
                ("file", "files")
            } else {
                ("match", "matches")
            };
            let count = match total {
                0 => format!("no {many}"),
                1 => format!("1 {one}"),
                n => format!("{n} {many}"),
            };
            let tail = std::mem::take(&mut self.subtitle);
            self.subtitle = if tail.is_empty() {
                count
            } else {
                format!("{count} {tail}")
            };
            if let Some(matches) = v.get("matches").and_then(Value::as_array) {
                for m in matches {
                    text.push_str(&format!(
                        "{}:{}: {}\n",
                        str_of(m, "path").unwrap_or("?"),
                        m.get("line").and_then(Value::as_u64).unwrap_or(0),
                        str_of(m, "content").unwrap_or_default()
                    ));
                }
            } else if let Some(groups) = v.get("groups").and_then(Value::as_array) {
                for g in groups {
                    text.push_str(&format!(
                        "{}\n{}\n",
                        str_of(g, "path").unwrap_or("?"),
                        str_of(g, "text").unwrap_or_default()
                    ));
                }
            } else if let Some(files) = v.get("files").and_then(Value::as_array) {
                for f in files.iter().filter_map(Value::as_str) {
                    text.push_str(f);
                    text.push('\n');
                }
            } else if let Some(counts) = v.get("counts").and_then(Value::as_object) {
                for (path, n) in counts {
                    text.push_str(&format!("{path}: {n}\n"));
                }
            }
            // `session_search`: rows of its own.
            if text.is_empty() {
                text = pretty_result(json, "");
            }
        }
        self.detail = Detail::Text(Output::new(&text));
    }

    fn finish_web(&mut self, json: Option<&Value>) {
        let mut text = String::new();
        if let Some(v) = json {
            if let Some(rows) = v
                .get("data")
                .and_then(|d| d.get("web"))
                .and_then(Value::as_array)
            {
                self.subtitle = plural(rows.len(), "result", "results");
                for row in rows {
                    text.push_str(&format!(
                        "{}\n{}\n{}\n\n",
                        str_of(row, "title").unwrap_or_default(),
                        str_of(row, "url").unwrap_or_default(),
                        str_of(row, "description").unwrap_or_default()
                    ));
                }
            } else if let Some(results) = v.get("results").and_then(Value::as_array) {
                let failed = results
                    .iter()
                    .filter(|r| r.get("error").is_some_and(Value::is_string))
                    .count();
                if failed > 0 && self.status == Status::Done {
                    self.status = Status::Failed;
                    self.subtitle = results
                        .iter()
                        .find_map(|r| str_of(r, "error"))
                        .map(first_line)
                        .unwrap_or_default();
                } else if self.status == Status::Done {
                    self.subtitle = plural(results.len(), "page", "pages");
                }
                for r in results {
                    text.push_str(&format!(
                        "{}\n{}\n\n",
                        str_of(r, "url").unwrap_or_default(),
                        str_of(r, "error")
                            .or_else(|| str_of(r, "content"))
                            .unwrap_or_default()
                    ));
                }
            }
        }
        self.detail = Detail::Text(Output::new(&text));
    }

    fn finish_delegate(&mut self, json: Option<&Value>) {
        let Detail::Delegate(tasks) = &mut self.detail else {
            return;
        };
        let results = json
            .and_then(|v| v.get("results"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (i, result) in results.iter().enumerate() {
            let index = result
                .get("task_index")
                .and_then(Value::as_u64)
                .map_or(i, |n| n as usize);
            if tasks.len() <= index {
                tasks.resize_with(index + 1, || Subtask {
                    id: None,
                    goal: String::new(),
                    status: Status::Running,
                    summary: String::new(),
                    duration_s: None,
                    tools: Vec::new(),
                    open: false,
                });
            }
            let task = &mut tasks[index];
            task.status = match str_of(result, "status") {
                Some("completed") | Some("complete") | Some("ok") | Some("success") => Status::Done,
                Some("running") => Status::Running,
                _ => Status::Failed,
            };
            task.summary = str_of(result, "summary")
                .filter(|s| !s.is_empty())
                .or_else(|| str_of(result, "error"))
                .unwrap_or_default()
                .to_string();
            task.duration_s = result.get("duration_seconds").and_then(Value::as_f64);
            // The trace is the subagent's tool use: `{tool, status}`.
            if let Some(trace) = result.get("tool_trace").and_then(Value::as_array) {
                task.tools = trace
                    .iter()
                    .map(|t| SubTool {
                        name: str_of(t, "tool").unwrap_or("?").to_string(),
                        preview: String::new(),
                        failed: str_of(t, "status") == Some("error"),
                    })
                    .collect();
            }
        }
        let done = tasks.iter().filter(|t| t.status == Status::Done).count();
        let failed = tasks.iter().filter(|t| t.status == Status::Failed).count();
        self.subtitle = match (done, failed) {
            (d, 0) if d == tasks.len() => String::new(),
            (d, f) => format!("{d} done, {f} failed"),
        };
        if failed > 0 && done == 0 && !tasks.is_empty() {
            self.status = Status::Failed;
        }
    }
}

/// The JSON value a result starts with, and the text after it. The core joins a result's text
/// blocks with newlines, so a JSON result can be followed by the harness's notes (a loop warning,
/// "identical call"): the value is still read. `None` for a result that is not JSON.
fn leading_json(result: &str) -> (Option<Value>, &str) {
    let mut values = serde_json::Deserializer::from_str(result).into_iter::<Value>();
    match values.next() {
        // Objects and arrays only: plain text that starts with a number or `true` is text.
        Some(Ok(value)) if value.is_object() || value.is_array() => {
            let rest = &result[values.byte_offset()..];
            (Some(value), rest)
        }
        _ => (None, ""),
    }
}

fn pretty_result(json: Option<&Value>, raw: &str) -> String {
    match json {
        Some(Value::String(s)) => s.clone(),
        Some(v) => serde_json::to_string_pretty(v).unwrap_or_else(|_| raw.to_string()),
        None => raw.to_string(),
    }
}

/// `Remembered "..."`, `Updated memory`, `Removed from your profile`.
fn memory_title(args: &Value) -> String {
    let target = if str_of(args, "target") == Some("user") {
        "your profile"
    } else {
        "memory"
    };
    let op = args
        .get("operations")
        .and_then(Value::as_array)
        .and_then(|o| o.first());
    let many = args
        .get("operations")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let snippet = |text: Option<&str>| {
        let text = first_line(text.unwrap_or_default());
        let cut: String = text.chars().take(80).collect();
        if cut.len() < text.len() {
            format!("{cut}...")
        } else {
            cut
        }
    };
    let head = match op.and_then(|o| str_of(o, "action")) {
        Some("add") => format!(
            "Remembered: {}",
            snippet(op.and_then(|o| str_of(o, "content")))
        ),
        Some("patch") => format!(
            "Updated {target}: {}",
            snippet(op.and_then(|o| str_of(o, "content")))
        ),
        Some("remove") => format!(
            "Removed from {target}: {}",
            snippet(op.and_then(|o| str_of(o, "old_text")))
        ),
        Some("note_read") => format!("Read a note in {target}"),
        _ => format!("Changed {target}"),
    };
    if many > 1 {
        format!("{head} (+{} more)", many - 1)
    } else {
        head
    }
}

fn skill_name(args: &Value) -> Option<String> {
    args.get("operations")
        .and_then(Value::as_array)
        .and_then(|o| o.iter().find_map(|o| str_of(o, "name")))
        .map(str::to_string)
}

/// `Created skill deploy`, `Updated skill deploy`.
fn skill_title(args: &Value) -> String {
    let op = args
        .get("operations")
        .and_then(Value::as_array)
        .and_then(|o| o.first());
    let name = skill_name(args).unwrap_or_else(|| "a skill".to_string());
    let many = args
        .get("operations")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let head = match op.and_then(|o| str_of(o, "action")) {
        Some("create") => format!("Created skill {name}"),
        Some("delete") => format!("Deleted skill {name}"),
        Some("patch") | Some("edit") | Some("write_file") | Some("remove_file") => {
            format!("Updated skill {name}")
        }
        _ => format!("Changed skill {name}"),
    };
    if many > 1 {
        format!("{head} (+{} more)", many - 1)
    } else {
        head
    }
}

// --- changed files ----------------------------------------------------------------------------

/// A file a turn changed, with the lines it added and removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

/// The files the edit cards of a turn changed, in the order they were first touched, a file edited
/// twice once with its counts added up. A failed edit changed nothing. Paths use `/`.
pub fn changed_files<'a>(cards: impl IntoIterator<Item = &'a ToolCard>) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    for card in cards {
        if card.status != Status::Done {
            continue;
        }
        let Detail::Edit {
            path,
            additions,
            deletions,
            ..
        } = &card.detail
        else {
            continue;
        };
        let path = path.replace('\\', "/");
        match files.iter_mut().find(|f| f.path == path) {
            Some(file) => {
                file.additions += additions;
                file.deletions += deletions;
            }
            None => files.push(ChangedFile {
                path,
                additions: *additions,
                deletions: *deletions,
            }),
        }
    }
    files
}

/// A node of the changed files' tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Dir {
        name: String,
        path: String,
        additions: u32,
        deletions: u32,
        children: Vec<Node>,
    },
    File {
        name: String,
        path: String,
        additions: u32,
        deletions: u32,
    },
}

impl Node {
    pub fn path(&self) -> &str {
        match self {
            Node::Dir { path, .. } | Node::File { path, .. } => path,
        }
    }
}

/// The tree of `files`: folders first, then files, each sorted by name (digits as numbers, case
/// ignored); a folder with one folder in it is joined with it (`src/thread`), with the counts of
/// the folder's files added up.
pub fn build_tree(files: &[ChangedFile]) -> Vec<Node> {
    #[derive(Default)]
    struct Dir {
        dirs: Vec<(String, Dir)>,
        files: Vec<ChangedFile>,
    }
    let mut root = Dir::default();
    for file in files {
        let parts: Vec<&str> = file.path.split('/').filter(|p| !p.is_empty()).collect();
        let Some((_, dirs)) = parts.split_last() else {
            continue;
        };
        let mut at = &mut root;
        for part in dirs {
            let ix = match at.dirs.iter().position(|(n, _)| n == part) {
                Some(ix) => ix,
                None => {
                    at.dirs.push(((*part).to_string(), Dir::default()));
                    at.dirs.len() - 1
                }
            };
            at = &mut at.dirs[ix].1;
        }
        at.files.push(ChangedFile {
            path: parts.join("/"),
            ..file.clone()
        });
    }

    fn natural(a: &str, b: &str) -> std::cmp::Ordering {
        // Digits as numbers, the rest case-insensitive.
        let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
        loop {
            match (x.peek().copied(), y.peek().copied()) {
                (None, None) => return std::cmp::Ordering::Equal,
                (None, _) => return std::cmp::Ordering::Less,
                (_, None) => return std::cmp::Ordering::Greater,
                (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                    let take = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                        let mut n = String::new();
                        while let Some(d) = it.peek().copied().filter(char::is_ascii_digit) {
                            n.push(d);
                            it.next();
                        }
                        n.parse::<u128>().unwrap_or(0)
                    };
                    let (m, n) = (take(&mut x), take(&mut y));
                    if m != n {
                        return m.cmp(&n);
                    }
                }
                (Some(p), Some(q)) => {
                    let (p, q) = (p.to_lowercase().next(), q.to_lowercase().next());
                    if p != q {
                        return p.cmp(&q);
                    }
                    x.next();
                    y.next();
                }
            }
        }
    }

    fn convert(dir: Dir, prefix: &str) -> Vec<Node> {
        let mut dirs = dir.dirs;
        dirs.sort_by(|a, b| natural(&a.0, &b.0));
        let mut files = dir.files;
        files.sort_by(|a, b| {
            natural(
                a.path.rsplit('/').next().unwrap_or(""),
                b.path.rsplit('/').next().unwrap_or(""),
            )
        });
        let mut nodes: Vec<Node> = dirs
            .into_iter()
            .map(|(name, dir)| {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                let children = convert(dir, &path);
                let (additions, deletions) = sum(&children);
                compact(name, path, additions, deletions, children)
            })
            .collect();
        nodes.extend(files.into_iter().map(|f| Node::File {
            name: f.path.rsplit('/').next().unwrap_or(&f.path).to_string(),
            path: f.path,
            additions: f.additions,
            deletions: f.deletions,
        }));
        nodes
    }

    /// `a` holding only `b` is `a/b`.
    fn compact(
        mut name: String,
        mut path: String,
        mut additions: u32,
        mut deletions: u32,
        mut children: Vec<Node>,
    ) -> Node {
        while children.len() == 1 && matches!(children[0], Node::Dir { .. }) {
            let Node::Dir {
                name: inner,
                path: inner_path,
                additions: a,
                deletions: d,
                children: inner_children,
            } = children.remove(0)
            else {
                unreachable!("checked above");
            };
            name = format!("{name}/{inner}");
            path = inner_path;
            additions = a;
            deletions = d;
            children = inner_children;
        }
        Node::Dir {
            name,
            path,
            additions,
            deletions,
            children,
        }
    }

    fn sum(nodes: &[Node]) -> (u32, u32) {
        nodes.iter().fold((0, 0), |(a, d), n| match n {
            Node::Dir {
                additions,
                deletions,
                ..
            }
            | Node::File {
                additions,
                deletions,
                ..
            } => (a + additions, d + deletions),
        })
    }

    convert(root, "")
}

/// One row of the tree as drawn: the node and how deep it sits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub node: Node,
    pub depth: usize,
    /// A folder that is open (its children follow).
    pub open: bool,
}

/// The rows of `nodes`, a folder's children after it unless its path is in `closed`.
pub fn tree_rows(nodes: &[Node], closed: &std::collections::HashSet<String>) -> Vec<TreeRow> {
    fn walk(
        nodes: &[Node],
        closed: &std::collections::HashSet<String>,
        depth: usize,
        out: &mut Vec<TreeRow>,
    ) {
        for node in nodes {
            match node {
                Node::Dir { path, children, .. } => {
                    let open = !closed.contains(path);
                    out.push(TreeRow {
                        node: node.clone(),
                        depth,
                        open,
                    });
                    if open {
                        walk(children, closed, depth + 1, out);
                    }
                }
                Node::File { .. } => out.push(TreeRow {
                    node: node.clone(),
                    depth,
                    open: false,
                }),
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, closed, 0, &mut out);
    out
}

/// The paths of every folder in `nodes`.
pub fn dir_paths(nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    for node in nodes {
        if let Node::Dir { path, children, .. } = node {
            out.push(path.clone());
            out.extend(dir_paths(children));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn done(name: &str, args: Value, result: Value) -> ToolCard {
        // As our tools flag it: `success: false` is an error result.
        let is_error = result.get("success") == Some(&json!(false));
        ToolCard::from_stored(name, &args, Some(&result.to_string()), is_error)
    }

    #[test]
    fn a_command_has_its_first_line_its_exit_code_and_its_output() {
        let card = done(
            "terminal",
            json!({"command": "cargo test\n--all"}),
            json!({"output": "ok\n\u{1b}[31mFAILED\u{1b}[0m\n", "exit_code": 101, "cwd": "C:/p", "duration_ms": 1200}),
        );
        assert_eq!(card.kind, ToolKind::Command);
        assert_eq!(
            (card.title.as_str(), card.exit_code, card.duration_ms),
            ("cargo test", Some(101), Some(1200))
        );
        assert_eq!(
            card.status,
            Status::Done,
            "a non-zero exit is not a failed call"
        );
        let Detail::Command { output, cwd, .. } = &card.detail else {
            panic!()
        };
        assert_eq!(cwd.as_deref(), Some("C:/p"));
        assert!(output.lines().is_none(), "kept raw until the card opens");
        let mut card = card;
        card.prepare_open();
        let Detail::Command { output, .. } = &card.detail else {
            panic!()
        };
        let lines = output.lines().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "FAILED");
        assert_eq!(lines[1].spans.len(), 1);
    }

    #[test]
    fn a_timed_out_command_is_failed_and_keeps_its_output_and_the_reason() {
        let card = done(
            "terminal",
            json!({"command": "sleep 99"}),
            json!({"success": false, "error": "Command timed out after 1s and was killed.", "output": "partial", "exit_code": -1, "timed_out": true}),
        );
        assert_eq!(card.status, Status::Failed);
        assert_eq!(card.subtitle, "Command timed out after 1s and was killed.");
        let Detail::Command { output, note, .. } = &card.detail else {
            panic!()
        };
        assert_eq!(&*output.raw, "partial");
        assert!(note.as_deref().unwrap().contains("timed out"));
    }

    #[test]
    fn a_background_command_says_so_while_it_runs_and_after() {
        let started = ToolCard::started(
            "terminal",
            &json!({"command": "npm run dev", "background": true}),
        );
        assert!(
            matches!(&started.detail, Detail::Command { note: Some(n), .. } if n.contains("background"))
        );
        let card = done(
            "terminal",
            json!({"command": "npm run dev", "background": true}),
            json!({"session_id": "proc_1", "pid": 4}),
        );
        let Detail::Command { note, .. } = &card.detail else {
            panic!()
        };
        assert!(note.as_deref().unwrap().contains("proc_1"));
    }

    #[test]
    fn progress_replaces_the_live_output_and_parses_it_only_when_open() {
        let mut card = ToolCard::started("terminal", &json!({"command": "make"}));
        card.progress("step 1\n", false);
        let Detail::Command { output, .. } = &card.detail else {
            panic!()
        };
        assert!(output.lines().is_none());
        card.progress("step 1\nstep 2\n", true);
        let Detail::Command { output, .. } = &card.detail else {
            panic!()
        };
        assert_eq!(output.lines().unwrap().len(), 2);
        assert_eq!(card.status, Status::Running);
    }

    #[test]
    fn a_patch_reads_its_counts_from_the_unified_diff_and_a_running_one_from_its_arguments() {
        let args = json!({"path": "src/a.rs", "old_string": "fn b() {}", "new_string": "fn b() { todo() }\n// note"});
        let running = ToolCard::started("patch", &args);
        let Detail::Edit {
            additions,
            deletions,
            from_args,
            ..
        } = &running.detail
        else {
            panic!()
        };
        assert_eq!((*additions, *deletions, *from_args), (2, 1, true));
        let diff = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,4 @@\n fn a() {}\n-fn b() {}\n+fn b() { todo() }\n+// note\n fn c() {}\n";
        let card = done(
            "patch",
            args,
            json!({"success": true, "diff": diff, "files_modified": ["src/a.rs"], "replacements": 1}),
        );
        assert_eq!(card.title, "Edited src/a.rs");
        let Detail::Edit {
            additions,
            deletions,
            from_args,
            diff,
            ..
        } = &card.detail
        else {
            panic!()
        };
        assert_eq!((*additions, *deletions, *from_args), (2, 1, false));
        assert_eq!(diff.lines.len(), 6);
    }

    #[test]
    fn a_written_file_counts_its_lines_as_added() {
        let card = done(
            "write_file",
            json!({"path": "new.txt", "content": "a\nb\nc\n"}),
            json!({"success": true, "path": "new.txt", "bytes_written": 6, "verified": true}),
        );
        assert_eq!(card.title, "Wrote new.txt");
        let Detail::Edit {
            additions,
            deletions,
            ..
        } = &card.detail
        else {
            panic!()
        };
        assert_eq!((*additions, *deletions), (3, 0));
    }

    #[test]
    fn a_failed_edit_changes_no_file_and_says_why() {
        let card = done(
            "patch",
            json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            json!({"success": false, "error": "old_string was not found in a.rs"}),
        );
        assert_eq!(card.status, Status::Failed);
        assert_eq!(card.subtitle, "old_string was not found in a.rs");
        assert!(changed_files([&card]).is_empty());
        let Detail::Edit {
            additions,
            deletions,
            ..
        } = &card.detail
        else {
            panic!()
        };
        assert_eq!((*additions, *deletions), (0, 0));
    }

    #[test]
    fn reads_and_searches_are_one_line_with_their_counts() {
        let read = done(
            "read_file",
            json!({"path": "notes.md", "offset": 10, "limit": 5}),
            json!({"content": "10|a\n11|b", "total_lines": 40, "file_size": 90}),
        );
        assert_eq!(
            (read.title.as_str(), read.subtitle.as_str()),
            ("Read notes.md", "lines 10\u{2013}14")
        );
        let whole = done(
            "read_file",
            json!({"path": "n.md"}),
            json!({"content": "1|a", "total_lines": 1, "file_size": 3}),
        );
        assert_eq!(whole.subtitle, "1 line");
        let many = done(
            "read_file",
            json!({"paths": ["a", "b", "c"]}),
            json!({"files": [{"path": "a", "content": "x"}, {"path": "b", "success": false, "error": "no"}], "files_read": 2, "files_failed": 1}),
        );
        assert_eq!(
            (many.title.as_str(), many.subtitle.as_str()),
            ("Read 3 files", "2 read, 1 failed")
        );
        let search = done(
            "search_files",
            json!({"pattern": "Streaming", "path": "docs"}),
            json!({"matches": [{"path": "docs/a.md", "line": 3, "content": "Streaming x"}], "total_count": 1}),
        );
        assert_eq!(
            (search.title.as_str(), search.subtitle.as_str()),
            ("Searched \"Streaming\"", "1 match in docs")
        );
        let Detail::Text(text) = &search.detail else {
            panic!()
        };
        assert_eq!(&*text.raw, "docs/a.md:3: Streaming x\n");
        let none = done(
            "search_files",
            json!({"pattern": "zzz", "target": "files"}),
            json!({"files": [], "total_count": 0}),
        );
        assert_eq!(
            (none.title.as_str(), none.subtitle.as_str()),
            ("Searched files for \"zzz\"", "no files")
        );
    }

    #[test]
    fn a_web_fetch_with_a_blocked_page_is_failed_with_the_reason() {
        let ok = done(
            "web_extract",
            json!({"urls": ["https://example.com/a?b=1", "https://other.org"]}),
            json!({"results": [{"url": "https://example.com/a?b=1", "title": "T", "content": "body", "error": null}]}),
        );
        assert_eq!(
            (ok.title.as_str(), ok.status),
            ("Fetched example.com and 1 more", Status::Done)
        );
        assert_eq!(ok.subtitle, "1 page");
        let blocked = done(
            "web_extract",
            json!({"urls": ["http://127.0.0.1:9/x"]}),
            json!({"results": [{"url": "http://127.0.0.1:9/x", "title": "", "content": "", "error": "Blocked: URL targets a private or internal network address"}]}),
        );
        assert_eq!(blocked.status, Status::Failed);
        assert!(blocked.subtitle.starts_with("Blocked"));
        let search = done(
            "web_search",
            json!({"query": "rust gpui"}),
            json!({"success": true, "data": {"web": [{"title": "GPUI", "url": "https://gpui.rs", "description": "d", "position": 1}]}}),
        );
        assert_eq!(
            (search.title.as_str(), search.subtitle.as_str()),
            ("Searched the web for \"rust gpui\"", "1 result")
        );
    }

    #[test]
    fn memory_and_skill_writes_link_to_their_screens() {
        let memory = done(
            "memory",
            json!({"target": "user", "operations": [{"action": "add", "content": "Prefers tabs"}]}),
            json!({"success": true, "message": "Entry added."}),
        );
        assert_eq!(memory.title, "Remembered: Prefers tabs");
        let Detail::Write { message, link } = &memory.detail else {
            panic!()
        };
        assert_eq!(
            (message.as_str(), link),
            ("Entry added.", &Screen::Memory("user".into()))
        );
        assert_eq!(link.label(), "Open your profile");
        let skill = done(
            "skill_manage",
            json!({"operations": [{"action": "create", "name": "deploy", "content": "..."}]}),
            json!({"success": true, "message": "Skill 'deploy' created."}),
        );
        assert_eq!(skill.title, "Created skill deploy");
        let Detail::Write { link, .. } = &skill.detail else {
            panic!()
        };
        assert_eq!(link, &Screen::Skill("deploy".into()));
        let failed = done(
            "memory",
            json!({"target": "memory", "operations": [{"action": "add", "content": "x"}]}),
            json!({"success": false, "error": "Memory is full"}),
        );
        assert_eq!(
            (failed.status, failed.subtitle.as_str()),
            (Status::Failed, "Memory is full")
        );
    }

    #[test]
    fn a_delegation_lists_its_tasks_with_their_trace() {
        let args = json!({"tasks": [{"goal": "Find the parser"}, {"goal": "Find the lexer"}]});
        let running = ToolCard::started("delegate_task", &args);
        assert_eq!(running.title, "Delegated 2 tasks");
        let card = done(
            "delegate_task",
            args,
            json!({"results": [
                {"task_index": 0, "status": "completed", "summary": "It is in parse.rs", "duration_seconds": 3.5, "tool_trace": [{"tool": "read_file", "status": "ok"}, {"tool": "terminal", "status": "error"}]},
                {"task_index": 1, "status": "failed", "summary": "", "error": "model gone"}
            ], "total_duration_seconds": 4.0}),
        );
        let Detail::Delegate(tasks) = &card.detail else {
            panic!()
        };
        assert_eq!(tasks[0].status, Status::Done);
        assert_eq!(tasks[0].tools.len(), 2);
        assert!(tasks[0].tools[1].failed);
        assert_eq!(
            (tasks[1].status, tasks[1].summary.as_str()),
            (Status::Failed, "model gone")
        );
        assert_eq!(card.subtitle, "1 done, 1 failed");
        let one = ToolCard::started(
            "delegate_task",
            &json!({"tasks": [{"goal": "One thing\nmore"}]}),
        );
        assert_eq!(one.title, "Delegated: One thing");
    }

    #[test]
    fn an_unknown_tool_shows_its_name_arguments_and_result_as_they_are() {
        let card = done(
            "todo_list",
            json!({"todos": [{"id": "1"}]}),
            json!({"todos": []}),
        );
        assert_eq!(card.kind, ToolKind::Generic);
        let Detail::Generic { args, result } = &card.detail else {
            panic!()
        };
        assert!(args.contains("todos"));
        assert_eq!(result.raw.trim(), "{\n  \"todos\": []\n}");
        let mcp = ToolCard::from_stored(
            "github__list_issues",
            &json!({"repo": "x"}),
            Some("plain text from a server"),
            false,
        );
        assert_eq!(
            mcp.status,
            Status::Done,
            "an unknown tool may answer in plain text"
        );
        let Detail::Generic { result, .. } = &mcp.detail else {
            panic!()
        };
        assert_eq!(&*result.raw, "plain text from a server");
    }

    #[test]
    fn a_call_fails_when_the_core_says_so_not_because_its_text_is_plain() {
        // The harness's own error (an unknown tool, a cancelled call): the core's flag.
        let card = ToolCard::from_stored(
            "read_file",
            &json!({"path": "a"}),
            Some("Tool call was cancelled"),
            true,
        );
        assert_eq!(
            (card.status, card.subtitle.as_str()),
            (Status::Failed, "Tool call was cancelled")
        );
        // Plain text that is not an error (a result too big for the model, replaced by a note
        // saying where it was saved): done.
        let spilled = ToolCard::from_stored(
            "read_file",
            &json!({"path": "big.log"}),
            Some("The result was 2 MB; it is saved in scratch/r1.txt."),
            false,
        );
        assert_eq!(spilled.status, Status::Done);
    }

    #[test]
    fn a_json_result_followed_by_the_harness_notes_is_still_read() {
        // `tool_result_text` joins a result's text blocks: the guardrails' notes come after.
        let text = format!(
            "{}\nYou ran this exact command 3 times; it returned the same output.",
            json!({"output": "ok", "exit_code": 0, "duration_ms": 5})
        );
        let card = ToolCard::from_stored("terminal", &json!({"command": "ls"}), Some(&text), false);
        assert_eq!((card.status, card.duration_ms), (Status::Done, Some(5)));
        let Detail::Command { output, note, .. } = &card.detail else {
            panic!()
        };
        assert_eq!(&*output.raw, "ok");
        assert!(note.as_deref().unwrap().contains("3 times"));
        let read = ToolCard::from_stored(
            "read_file",
            &json!({"path": "a"}),
            Some(&format!("{}\nnote", json!({"content": "x", "total_lines": 1}))),
            false,
        );
        assert_eq!((read.status, read.subtitle.as_str()), (Status::Done, "1 line"));
    }

    #[test]
    fn a_call_without_a_result_is_failed_not_spinning() {
        let card = ToolCard::from_stored("terminal", &json!({"command": "x"}), None, false);
        assert_eq!(card.status, Status::Failed);
        assert!(card.subtitle.contains("No result"));
    }

    #[test]
    fn changed_files_add_up_per_file_and_leave_failed_edits_out() {
        let edit = |path: &str, a: &str, b: &str| {
            done(
                "patch",
                json!({"path": path, "old_string": a, "new_string": b}),
                json!({"success": true, "diff": format!("--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,1 @@\n-{a}\n+{b}\n"), "files_modified": [path]}),
            )
        };
        let failed = done(
            "patch",
            json!({"path": "z.rs"}),
            json!({"success": false, "error": "no"}),
        );
        let cards = [
            edit("src/a.rs", "x", "y"),
            edit("src/b.rs", "x", "y"),
            edit("src/a.rs", "y", "z"),
            failed,
        ];
        let files = changed_files(&cards);
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: "src/a.rs".into(),
                    additions: 2,
                    deletions: 2
                },
                ChangedFile {
                    path: "src/b.rs".into(),
                    additions: 1,
                    deletions: 1
                },
            ]
        );
    }

    fn file(path: &str, a: u32, d: u32) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            additions: a,
            deletions: d,
        }
    }

    #[test]
    fn the_tree_puts_folders_first_joins_single_folders_and_sorts_naturally() {
        let tree = build_tree(&[
            file("README.md", 1, 0),
            file("crates/native/src/thread/b10.rs", 3, 1),
            file("crates/native/src/thread/b2.rs", 4, 0),
            file("crates/native/src/lib.rs", 1, 1),
        ]);
        let names: Vec<&str> = tree
            .iter()
            .map(|n| match n {
                Node::Dir { name, .. } | Node::File { name, .. } => name.as_str(),
            })
            .collect();
        assert_eq!(names, ["crates/native/src", "README.md"]);
        let Node::Dir {
            path,
            additions,
            deletions,
            children,
            ..
        } = &tree[0]
        else {
            panic!()
        };
        assert_eq!(path, "crates/native/src");
        assert_eq!((*additions, *deletions), (8, 2));
        let inner: Vec<&str> = children
            .iter()
            .map(|n| match n {
                Node::Dir { name, .. } | Node::File { name, .. } => name.as_str(),
            })
            .collect();
        assert_eq!(inner, ["thread", "lib.rs"]);
        let Node::Dir {
            children: thread, ..
        } = &children[0]
        else {
            panic!()
        };
        let order: Vec<&str> = thread.iter().map(|n| n.path()).collect();
        assert_eq!(
            order,
            [
                "crates/native/src/thread/b2.rs",
                "crates/native/src/thread/b10.rs"
            ]
        );
    }

    #[test]
    fn closed_folders_hide_their_children_in_the_rows() {
        let tree = build_tree(&[
            file("a/b.rs", 1, 0),
            file("a/c/d.rs", 1, 0),
            file("e.rs", 1, 0),
        ]);
        let all = tree_rows(&tree, &Default::default());
        let shown: Vec<(&str, usize)> = all.iter().map(|r| (r.node.path(), r.depth)).collect();
        assert_eq!(
            shown,
            [
                ("a", 0),
                ("a/c", 1),
                ("a/c/d.rs", 2),
                ("a/b.rs", 1),
                ("e.rs", 0)
            ]
        );
        let closed: std::collections::HashSet<String> = ["a".to_string()].into();
        let rows = tree_rows(&tree, &closed);
        assert_eq!(
            rows.iter().map(|r| r.node.path()).collect::<Vec<_>>(),
            ["a", "e.rs"]
        );
        assert!(!rows[0].open);
        assert_eq!(dir_paths(&tree), ["a", "a/c"]);
    }
}
