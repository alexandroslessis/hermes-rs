//! Background processes: `terminal(background=true)` and the `process_manage` tool.
//!
//! Hermes: `tools/process_registry.py` (and its `process_registry_*.py` siblings).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::watch;

use crate::shell::{ProcessTree, ShellKind, command, command_for_powershell, encode_powershell};
use crate::{SessionTools, ToolOutput, display_path, parse_args};

const MAX_BUFFERED_LINES: usize = 5000;
/// Lines of output a process status list carries (its viewer's tail).
const STATUS_TAIL_LINES: usize = 200;
const DEFAULT_LOG_LINES: usize = 200;
const DEFAULT_WAIT_SECS: u64 = 180;
const MAX_WAIT_SECS: u64 = 600;
const MAX_LINE_CHARS: usize = 4000;
const TAIL_LINES_IN_NOTICE: usize = 20;

/// Sent through `UserBridge::process_finished` for processes started with `notify=true`.
#[derive(Debug, Clone)]
pub struct ProcessNotice {
    pub process_id: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub tail: String,
}

/// The last `MAX_BUFFERED_LINES` lines, numbered from the start of the process.
#[derive(Default)]
struct OutputBuffer {
    lines: VecDeque<String>,
    total: usize,
}

impl OutputBuffer {
    fn push(&mut self, line: String) {
        self.lines.push_back(line);
        self.total += 1;
        if self.lines.len() > MAX_BUFFERED_LINES {
            self.lines.pop_front();
        }
    }

    fn first_kept(&self) -> usize {
        self.total - self.lines.len()
    }

    /// Lines with absolute index >= `from`, and the index after the last one.
    fn since(&self, from: usize) -> (Vec<String>, usize, bool) {
        let first = self.first_kept();
        let skipped = from < first;
        let start = from.max(first) - first;
        (
            self.lines.iter().skip(start).cloned().collect(),
            self.total,
            skipped,
        )
    }

    fn range(&self, offset: usize, limit: usize) -> Vec<String> {
        let first = self.first_kept();
        let start = offset.max(first) - first;
        self.lines.iter().skip(start).take(limit).cloned().collect()
    }
}

struct BackgroundProcess {
    id: String,
    command: String,
    cwd: PathBuf,
    tree: ProcessTree,
    started: Instant,
    output: Mutex<OutputBuffer>,
    poll_cursor: Mutex<usize>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    exit: watch::Receiver<Option<Option<i32>>>,
}

impl BackgroundProcess {
    fn exit_status(&self) -> Option<Option<i32>> {
        *self.exit.borrow()
    }

    fn status_json(&self) -> Value {
        let mut value = json!({
            "session_id": self.id,
            "command": self.command,
            "cwd": display_path(&self.cwd),
            "running_seconds": self.started.elapsed().as_secs(),
            "output_lines": self.output.lock().total,
        });
        match self.exit_status() {
            None => value["status"] = json!("running"),
            Some(code) => {
                value["status"] = json!("exited");
                value["exit_code"] = json!(code);
            }
        }
        value
    }
}

#[derive(Default)]
pub struct ProcessRegistry {
    processes: Mutex<HashMap<String, Arc<BackgroundProcess>>>,
}

impl ProcessRegistry {
    fn find(&self, prefix: &str) -> Result<Arc<BackgroundProcess>, String> {
        let processes = self.processes.lock();
        let matches: Vec<_> = processes
            .values()
            .filter(|process| {
                process.id.starts_with(prefix)
                    || process.id.trim_start_matches("proc_").starts_with(prefix)
            })
            .cloned()
            .collect();
        match matches.len() {
            1 => Ok(matches.into_iter().next().expect("one match")),
            0 => Err(format!(
                "No background process matches '{prefix}'. Use action='list' to see them."
            )),
            n => Err(format!(
                "'{prefix}' matches {n} processes; use a longer prefix."
            )),
        }
    }

    /// Every process's status (the `process` tool's `list`), oldest first.
    pub(crate) fn status_list(&self) -> Vec<Value> {
        let mut list: Vec<_> = self.processes.lock().values().cloned().collect();
        list.sort_by_key(|process| process.started);
        list.iter()
            .map(|process| {
                let mut status = process.status_json();
                // The last lines it printed, for a viewer of the process.
                let output = process.output.lock();
                let skip = output.lines.len().saturating_sub(STATUS_TAIL_LINES);
                status["output_tail"] = json!(
                    output
                        .lines
                        .iter()
                        .skip(skip)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                status
            })
            .collect()
    }

    /// How many of the processes are still running.
    pub(crate) fn running(&self) -> usize {
        self.processes
            .lock()
            .values()
            .filter(|process| process.exit_status().is_none())
            .count()
    }

    pub async fn kill_all(&self) {
        let processes: Vec<_> = self.processes.lock().values().cloned().collect();
        for process in processes {
            if process.exit_status().is_none() {
                process.tree.kill();
            }
        }
    }
}

fn read_lines<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    process: Arc<BackgroundProcess>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line);
                    let mut text = text.trim_end_matches(['\n', '\r']).to_string();
                    if text.chars().count() > MAX_LINE_CHARS {
                        text =
                            format!("{}…", text.chars().take(MAX_LINE_CHARS).collect::<String>());
                    }
                    process.output.lock().push(text);
                }
            }
        }
    })
}

pub async fn spawn(
    tools: &SessionTools,
    command_line: &str,
    workdir: Option<PathBuf>,
    notify: bool,
) -> ToolOutput {
    let cwd = workdir.unwrap_or_else(|| tools.cwd());
    let scratch = tools.scratch_dir.clone();
    let _ = std::fs::create_dir_all(&scratch);
    let mut builder = match tools.shell.kind() {
        ShellKind::GitBash(bash) => {
            let mut builder = command(bash, &cwd, &scratch);
            builder.args(["--noprofile", "--norc", "-c", command_line]);
            builder
        }
        ShellKind::PowerShell => {
            let mut builder = command_for_powershell(&cwd, &scratch);
            builder
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-EncodedCommand",
                ])
                .arg(encode_powershell(command_line));
            builder
        }
    };
    // Background processes outlive the tool call; the registry kills them at session end.
    builder.kill_on_drop(false);
    let mut child = match builder
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return ToolOutput::error(format!("Could not start the process: {error}")),
    };

    let id = format!("proc_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let (exit_tx, exit_rx) = watch::channel(None);
    let process = Arc::new(BackgroundProcess {
        id: id.clone(),
        command: command_line.to_string(),
        cwd: cwd.clone(),
        tree: ProcessTree::contain(child.id()),
        started: Instant::now(),
        output: Mutex::new(OutputBuffer::default()),
        poll_cursor: Mutex::new(0),
        stdin: tokio::sync::Mutex::new(child.stdin.take()),
        exit: exit_rx,
    });
    let stdout = read_lines(child.stdout.take().expect("piped stdout"), process.clone());
    let stderr = read_lines(child.stderr.take().expect("piped stderr"), process.clone());
    tools
        .processes
        .processes
        .lock()
        .insert(id.clone(), process.clone());

    let bridge = tools.bridge.clone();
    let session_id = tools.session_id.clone();
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|status| status.code());
        let _ = stdout.await;
        let _ = stderr.await;
        let _ = exit_tx.send(Some(code));
        if notify {
            let tail = {
                let output = process.output.lock();
                let skip = output.lines.len().saturating_sub(TAIL_LINES_IN_NOTICE);
                output
                    .lines
                    .iter()
                    .skip(skip)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let tail = crate::redact::redact_terminal_output(&tail, &process.command);
            bridge.process_finished(
                &session_id,
                ProcessNotice {
                    process_id: process.id.clone(),
                    command: process.command.clone(),
                    exit_code: code,
                    tail,
                },
            );
        }
    });

    ToolOutput::ok(json!({
        "session_id": id,
        "status": "running",
        "cwd": display_path(&cwd),
        "notify": notify,
        "hint": "Check on it with process_manage (poll, log, wait, kill).",
    }))
}

#[derive(Deserialize)]
struct ProcessArgs {
    action: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn process_manage(tools: &SessionTools, args: Value) -> ToolOutput {
    let args: ProcessArgs = match parse_args("process_manage", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    if args.action == "list" {
        let mut list: Vec<_> = tools.processes.processes.lock().values().cloned().collect();
        list.sort_by_key(|process| process.started);
        return ToolOutput::ok(
            json!({"processes": list.iter().map(|p| p.status_json()).collect::<Vec<_>>()}),
        );
    }
    let Some(prefix) = args.session_id.as_deref() else {
        return ToolOutput::error(format!(
            "session_id is required for action '{}'.",
            args.action
        ));
    };
    let process = match tools.processes.find(prefix) {
        Ok(process) => process,
        Err(error) => return ToolOutput::error(error),
    };

    match args.action.as_str() {
        "poll" => {
            let mut cursor = process.poll_cursor.lock();
            let (lines, next, skipped) = process.output.lock().since(*cursor);
            *cursor = next;
            let mut value = process.status_json();
            value["new_output"] = json!(lines.join("\n"));
            if skipped {
                value["note"] =
                    json!("Some output since the last poll was dropped from the rolling buffer.");
            }
            ToolOutput::ok(value)
        }
        "log" => {
            let limit = args.limit.unwrap_or(DEFAULT_LOG_LINES);
            let (lines, offset) = {
                let output = process.output.lock();
                let offset = args
                    .offset
                    .unwrap_or_else(|| output.total.saturating_sub(limit));
                (output.range(offset, limit), offset.max(output.first_kept()))
            };
            let mut value = process.status_json();
            value["offset"] = json!(offset);
            value["output"] = json!(lines.join("\n"));
            ToolOutput::ok(value)
        }
        "wait" => {
            let timeout = Duration::from_secs(
                args.timeout
                    .unwrap_or(DEFAULT_WAIT_SECS)
                    .clamp(1, MAX_WAIT_SECS),
            );
            let mut exit = process.exit.clone();
            let finished = tokio::time::timeout(timeout, exit.wait_for(Option::is_some))
                .await
                .is_ok();
            let mut value = process.status_json();
            let (lines, next, _) = process.output.lock().since(*process.poll_cursor.lock());
            *process.poll_cursor.lock() = next;
            value["new_output"] = json!(lines.join("\n"));
            if !finished {
                value["note"] = json!(format!("Still running after {}s.", timeout.as_secs()));
            }
            ToolOutput::ok(value)
        }
        "kill" => {
            if process.exit_status().is_none() {
                process.tree.kill();
                let mut exit = process.exit.clone();
                let _ =
                    tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some))
                        .await;
            }
            ToolOutput::ok(process.status_json())
        }
        "write" | "submit" => {
            let mut data = args.data.unwrap_or_default();
            if args.action == "submit" {
                data.push('\n');
            }
            let mut stdin = process.stdin.lock().await;
            let Some(pipe) = stdin.as_mut() else {
                return ToolOutput::error("stdin is closed for this process.");
            };
            if let Err(error) = async {
                pipe.write_all(data.as_bytes()).await?;
                pipe.flush().await
            }
            .await
            {
                return ToolOutput::error(format!("Could not write to stdin: {error}"));
            }
            ToolOutput::ok(json!({"session_id": process.id, "bytes_written": data.len()}))
        }
        "close" => {
            process.stdin.lock().await.take();
            ToolOutput::ok(json!({"session_id": process.id, "stdin": "closed"}))
        }
        other => ToolOutput::error(format!("Unknown action '{other}'.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{TestBridge, tools_in, tools_with_bridge};

    #[tokio::test]
    async fn background_process_lifecycle() {
        let (_dir, tools) = tools_in();
        if !matches!(tools.shell.kind(), ShellKind::GitBash(_)) {
            return;
        }
        let started = crate::shell::terminal(
            &tools,
            json!({"command": "echo one; sleep 1; echo two", "background": true}),
            None,
        )
        .await;
        let id = started.value["session_id"].as_str().unwrap().to_string();

        let waited = process_manage(
            &tools,
            json!({"action": "wait", "session_id": &id[5..9], "timeout": 20}),
        )
        .await;
        assert_eq!(waited.value["status"], "exited", "{:?}", waited.value);
        assert_eq!(waited.value["exit_code"], 0);
        assert_eq!(waited.value["new_output"], "one\ntwo");

        let log = process_manage(&tools, json!({"action": "log", "session_id": id})).await;
        assert_eq!(log.value["output"], "one\ntwo");
    }

    #[tokio::test]
    async fn stdin_submit_and_kill() {
        let (_dir, tools) = tools_in();
        if !matches!(tools.shell.kind(), ShellKind::GitBash(_)) {
            return;
        }
        let started = crate::shell::terminal(
            &tools,
            json!({"command": "read line; echo got:$line; sleep 60", "background": true}),
            None,
        )
        .await;
        let id = started.value["session_id"].as_str().unwrap().to_string();
        process_manage(
            &tools,
            json!({"action": "submit", "session_id": id, "data": "hi"}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let polled = process_manage(&tools, json!({"action": "poll", "session_id": id})).await;
        assert_eq!(polled.value["new_output"], "got:hi");
        let killed = process_manage(&tools, json!({"action": "kill", "session_id": id})).await;
        assert_eq!(killed.value["status"], "exited");
    }

    #[tokio::test]
    async fn notify_reports_completion_through_the_bridge() {
        let bridge = Arc::new(TestBridge::default());
        let (_dir, tools) = tools_with_bridge(bridge.clone());
        if !matches!(tools.shell.kind(), ShellKind::GitBash(_)) {
            return;
        }
        crate::shell::terminal(
            &tools,
            json!({"command": "echo done; exit 4", "background": true, "notify": true}),
            None,
        )
        .await;
        for _ in 0..100 {
            if !bridge.notices.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let notices = bridge.notices.lock();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].exit_code, Some(4));
        assert_eq!(notices[0].tail, "done");
    }

    #[test]
    fn rolling_buffer_drops_the_oldest_lines() {
        let mut buffer = OutputBuffer::default();
        for n in 0..MAX_BUFFERED_LINES + 10 {
            buffer.push(n.to_string());
        }
        let (lines, next, skipped) = buffer.since(0);
        assert!(skipped);
        assert_eq!(lines.first().unwrap(), "10");
        assert_eq!(next, MAX_BUFFERED_LINES + 10);
    }
}
