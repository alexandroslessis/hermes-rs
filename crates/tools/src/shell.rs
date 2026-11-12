//! The `terminal` tool: one persistent shell per session.
//!
//! With Git Bash, a single `bash.exe` process lives as long as the session, so `cd` and `export`
//! carry over between calls. Each command is wrapped so its output ends with a sentinel line
//! carrying the exit code and the new working directory. A timeout, or a dropped future (the
//! user stopped the turn), kills the whole process tree and the next call starts a fresh shell
//! in the same directory. Without Git Bash, each command runs in a fresh PowerShell and only the
//! working directory carries over.
//!
//! Hermes: `tools/terminal_tool.py`.

use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use std::sync::Arc;

use hermes_agent::operation::Emitter;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::sync::CancellationToken;

use crate::job::Job;
use crate::{SessionTools, ToolOutput, display_path, parse_args};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DEFAULT_TIMEOUT_SECS: u64 = 180;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_OUTPUT_CHARS: usize = 30_000;

/// One `tool.progress` event at most per this interval per call (Hermes's `tool.progress`
/// `_MIN_INTERVAL_S = 0.5`); the first update always passes.
const RELAY_INTERVAL: Duration = Duration::from_millis(500);
/// The relayed message is the accumulated output's tail, this many chars, so the clicked-open
/// row shows the output growing without flooding the socket on a chatty build.
const RELAY_TAIL_CHARS: usize = 4_096;

/// The relay's state across a command's captured chunks.
struct Relay {
    last: Option<Instant>,
    text: String,
    bytes: u64,
}
const HEAD_CHARS: usize = 10_000;
const TAIL_CHARS: usize = 18_000;
/// Output kept in memory per command; anything beyond is counted but dropped.
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellKind {
    GitBash(PathBuf),
    PowerShell,
}

/// Finds Git for Windows' bash (never WSL's `System32\bash.exe`), else PowerShell.
pub fn detect_shell() -> ShellKind {
    let mut candidates: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|var| std::env::var_os(var))
        .map(|dir| PathBuf::from(dir).join("Git").join("bin").join("bash.exe"))
        .collect();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        );
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("git.exe").is_file() {
                if let Some(root) = dir.parent() {
                    candidates.push(root.join("bin").join("bash.exe"));
                }
            }
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .map(ShellKind::GitBash)
        .unwrap_or(ShellKind::PowerShell)
}

/// Names the user keeps in the shell's environment although they look like secrets
/// (`terminal.env_passthrough` in Settings), compared case-insensitively.
static ENV_PASSTHROUGH: parking_lot::RwLock<Vec<String>> = parking_lot::RwLock::new(Vec::new());

/// Sets the passthrough list (the app calls it at startup and after Settings change).
pub fn set_env_passthrough(names: Vec<String>) {
    *ENV_PASSTHROUGH.write() = names.into_iter().map(|n| n.to_ascii_uppercase()).collect();
}

/// Whether an environment variable looks like it holds a secret: its name contains `KEY`,
/// `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL` or `AUTH` (so `SSH_AUTH_SOCK` goes too;
/// list it in `terminal.env_passthrough` to keep it). `AUTHOR` is not `AUTH`: git's
/// `GIT_AUTHOR_NAME` / `GIT_AUTHOR_EMAIL` stay, or the agent's commits would change author.
fn looks_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase().replace("AUTHOR", "");
    [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ]
    .iter()
    .any(|word| name.contains(word))
}

/// The parent's environment minus secret-looking names (except the user's passthrough list):
/// what the agent's shell starts with, so a command (or a prompt injection) can't read the
/// provider's API key with `env`.
pub(crate) fn scrubbed_env(
    parent: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    passthrough: &[String],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    parent
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            !looks_secret(&name)
                || passthrough
                    .iter()
                    .any(|kept| kept.eq_ignore_ascii_case(&name))
        })
        .collect()
}

pub(crate) fn shell_env(scratch_dir: &Path) -> Vec<(&'static str, String)> {
    let scratch = display_path(scratch_dir);
    vec![
        ("TMPDIR", scratch.clone()),
        ("TEMP", scratch.clone()),
        ("TMP", scratch),
        ("MSYS_NO_PATHCONV", "1".into()),
        ("MSYS2_ARG_CONV_EXCL", "*".into()),
        ("CHERE_INVOKING", "1".into()),
        ("PAGER", "cat".into()),
        ("GIT_PAGER", "cat".into()),
        ("TERM", "dumb".into()),
        ("PYTHONUTF8", "1".into()),
        ("PYTHONIOENCODING", "utf-8".into()),
    ]
}

/// A command builder that never opens a console window and starts from our environment without the secret-looking variables (`scrubbed_env`).
pub(crate) fn command(program: &Path, cwd: &Path, scratch_dir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .env_clear()
        .envs(scrubbed_env(std::env::vars_os(), &ENV_PASSTHROUGH.read()))
        .envs(shell_env(scratch_dir))
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    command
}

/// Fallback when a job object couldn't be set up: kills the process and the descendants Windows
/// can see (not MSYS-forked ones).
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// A spawned process together with everything it starts. Killing it kills the whole tree.
#[derive(Clone)]
pub(crate) struct ProcessTree {
    pid: Option<u32>,
    job: Option<Arc<Job>>,
}

impl ProcessTree {
    /// Puts the process into a fresh job object right after spawning it.
    pub(crate) fn contain(pid: Option<u32>) -> Self {
        let job = pid.and_then(|pid| {
            let job = Job::new().and_then(|job| job.assign(pid).map(|()| job));
            match job {
                Ok(job) => Some(Arc::new(job)),
                Err(error) => {
                    tracing::warn!("could not put process {pid} in a job object: {error}");
                    None
                }
            }
        });
        Self { pid, job }
    }

    pub(crate) fn kill(&self) {
        match (&self.job, self.pid) {
            (Some(job), _) => job.terminate(),
            (None, Some(pid)) => kill_tree(pid),
            (None, None) => {}
        }
    }
}

/// Kills the process tree unless disarmed, including when the owning future is dropped.
struct KillOnDrop {
    tree: Option<ProcessTree>,
}

impl KillOnDrop {
    fn disarm(&mut self) {
        self.tree = None;
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(tree) = &self.tree {
            tree.kill();
        }
    }
}

struct BashProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    tree: ProcessTree,
}

pub struct ShellSession {
    kind: ShellKind,
    cwd: parking_lot::Mutex<PathBuf>,
    scratch_dir: PathBuf,
    process: tokio::sync::Mutex<Option<BashProcess>>,
}

#[derive(Debug)]
pub struct RunResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The shell process ended (an `exit` in the command, or a kill) and will be restarted.
    pub shell_restarted: bool,
    /// The turn was stopped while the command ran; the output is what it had printed.
    pub interrupted: bool,
}

fn bash_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The turn's cancel, or a future that never resolves (no turn to observe: the review path's
/// and background spawns' runs are stopped by nothing but their timeout).
async fn stopped(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

fn powershell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

impl ShellSession {
    pub fn new(kind: ShellKind, cwd: PathBuf, scratch_dir: PathBuf) -> Self {
        Self {
            kind,
            cwd: parking_lot::Mutex::new(cwd),
            scratch_dir,
            process: tokio::sync::Mutex::new(None),
        }
    }

    pub fn kind(&self) -> &ShellKind {
        &self.kind
    }

    pub fn cwd(&self) -> PathBuf {
        self.cwd.lock().clone()
    }

    pub(crate) fn scratch_dir(&self) -> &Path {
        &self.scratch_dir
    }

    pub async fn shutdown(&self) {
        if let Some(mut process) = self.process.lock().await.take() {
            process.tree.kill();
            let _ = process.child.wait().await;
        }
    }

    pub async fn run(
        &self,
        command: &str,
        workdir: Option<&Path>,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
        on_chunk: &(dyn Fn(&str) + Send + Sync),
    ) -> RunResult {
        let _ = std::fs::create_dir_all(&self.scratch_dir);
        match &self.kind {
            ShellKind::GitBash(bash) => {
                self.run_bash(bash, command, workdir, timeout, cancel, on_chunk)
                    .await
            }
            ShellKind::PowerShell => {
                self.run_powershell(command, workdir, timeout, cancel, on_chunk)
                    .await
            }
        }
    }

    async fn spawn_bash(&self, bash: &Path) -> std::io::Result<BashProcess> {
        let mut child = command(bash, &self.cwd(), &self.scratch_dir)
            .args(["--noprofile", "--norc"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let tree = ProcessTree::contain(child.id());
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut stderr = child.stderr.take().expect("piped stderr");
        // The wrapper sends command output to stdout; anything bash itself prints is discarded.
        tokio::spawn(async move {
            let mut sink = Vec::new();
            let _ = stderr.read_to_end(&mut sink).await;
        });
        Ok(BashProcess {
            child,
            stdin,
            stdout,
            tree,
        })
    }

    async fn run_bash(
        &self,
        bash: &Path,
        command: &str,
        workdir: Option<&Path>,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
        on_chunk: &(dyn Fn(&str) + Send + Sync),
    ) -> RunResult {
        let mut slot = self.process.lock().await;
        if let Some(process) = slot.as_mut() {
            if !matches!(process.child.try_wait(), Ok(None)) {
                *slot = None;
            }
        }
        if slot.is_none() {
            match self.spawn_bash(bash).await {
                Ok(process) => *slot = Some(process),
                Err(error) => {
                    return RunResult {
                        output: format!("Could not start {}: {error}", bash.display()),
                        exit_code: None,
                        timed_out: false,
                        shell_restarted: false,
                        interrupted: false,
                    };
                }
            }
        }
        let process = slot.as_mut().expect("shell started");

        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let sentinel = format!("__HERMES_DONE_{nonce}__ ");
        let body = match workdir {
            Some(dir) => format!(
                "( cd -- {} || exit 1\n{command}\n)",
                bash_quote(&display_path(dir))
            ),
            None => format!("{{ {command}\n}}"),
        };
        let script = format!(
            "{body} </dev/null 2>&1\n__hermes_ec=$?\nprintf '\\n%s%d %s\\n' '{sentinel}' \"$__hermes_ec\" \"$(pwd -W 2>/dev/null || pwd)\"\n"
        );
        if process.stdin.write_all(script.as_bytes()).await.is_err()
            || process.stdin.flush().await.is_err()
        {
            process.tree.kill();
            *slot = None;
            return RunResult {
                output: "The shell had exited; it was restarted. Run the command again.".into(),
                exit_code: None,
                timed_out: false,
                shell_restarted: true,
                interrupted: false,
            };
        }

        let mut killer = KillOnDrop {
            tree: Some(process.tree.clone()),
        };
        let mut captured: Vec<u8> = Vec::new();
        let mut dropped_bytes = 0usize;
        let mut finished: Option<(i32, String)> = None;
        let read = async {
            let mut line = Vec::new();
            loop {
                line.clear();
                match process.stdout.read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                if line.starts_with(sentinel.as_bytes()) {
                    let rest = String::from_utf8_lossy(&line[sentinel.len()..]);
                    let rest = rest.trim_end();
                    let (code, cwd) = rest.split_once(' ').unwrap_or((rest, ""));
                    finished = Some((code.parse().unwrap_or(-1), cwd.to_string()));
                    return;
                }
                if captured.len() < MAX_CAPTURE_BYTES {
                    captured.extend_from_slice(&line);
                    // Live output: the line as captured, before the run's own masking of
                    // the full result (the tool masks each chunk it relays).
                    on_chunk(&String::from_utf8_lossy(&line));
                } else {
                    dropped_bytes += line.len();
                }
            }
        };
        let (timed_out, interrupted) = match tokio::time::timeout(timeout, async {
            tokio::select! {
                biased;
                _ = read => false,
                _ = stopped(cancel) => true,
            }
        })
        .await
        {
            Ok(interrupted) => (false, interrupted),
            Err(_) => (true, false),
        };
        // The sentinel is printed after a newline of its own, so trailing newlines are framing.
        let mut output = String::from_utf8_lossy(&captured)
            .trim_end_matches(['\n', '\r'])
            .to_string();
        if dropped_bytes > 0 {
            output.push_str(&format!(
                "\n[... {dropped_bytes} more bytes of output were discarded]"
            ));
        }
        match finished {
            Some((exit_code, cwd)) => {
                killer.disarm();
                if !cwd.is_empty() {
                    *self.cwd.lock() = PathBuf::from(cwd);
                }
                RunResult {
                    output,
                    exit_code: Some(exit_code),
                    timed_out: false,
                    shell_restarted: false,
                    interrupted: false,
                }
            }
            None => {
                drop(killer);
                *slot = None;
                RunResult {
                    output,
                    exit_code: None,
                    timed_out,
                    shell_restarted: true,
                    interrupted,
                }
            }
        }
    }

    async fn run_powershell(
        &self,
        command: &str,
        workdir: Option<&Path>,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
        on_chunk: &(dyn Fn(&str) + Send + Sync),
    ) -> RunResult {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let sentinel = format!("__HERMES_DONE_{nonce}__ ");
        let cwd = workdir.map(Path::to_path_buf).unwrap_or_else(|| self.cwd());
        let script = format!(
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8\n\
             Set-Location -LiteralPath {}\n\
             try {{ . {{\n{command}\n}} 2>&1 | Out-String -Stream -Width 4096 | ForEach-Object {{ $_.TrimEnd() }} }} catch {{ $_ | Out-String }}\n\
             $hermesExit = if ($null -ne $LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}\n\
             \"{sentinel}$hermesExit $((Get-Location).Path)\"\n",
            powershell_quote(&cwd.display().to_string())
        );
        let spawned = command_for_powershell(&cwd, &self.scratch_dir)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
            ])
            .arg(encode_powershell(&script))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                return RunResult {
                    output: format!("Could not start PowerShell: {error}"),
                    exit_code: None,
                    timed_out: false,
                    shell_restarted: false,
                    interrupted: false,
                };
            }
        };
        let mut killer = KillOnDrop {
            tree: Some(ProcessTree::contain(child.id())),
        };
        let mut stdout = child.stdout.take().expect("piped stdout");
        // PowerShell's own errors (a script that doesn't parse never prints the sentinel) come on
        // stderr, as CLIXML: kept, a little of it, to tell the model why nothing ran.
        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut kept = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    let room = (16 * 1024usize).saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                }
                kept
            })
        });
        // Kept up to MAX_CAPTURE_BYTES, as the bash path: the rest is read (so the command can
        // finish) and dropped. Unbounded, a command printing millions of lines held hundreds of
        // MB and made masking secrets in it take minutes.
        let mut captured = Vec::new();
        let mut dropped_bytes = 0usize;
        // The end of the dropped part, where the sentinel line (exit code, folder) is.
        let mut dropped_tail: Vec<u8> = Vec::new();
        let read = async {
            let mut chunk = vec![0u8; 64 * 1024];
            // The relay takes whole lines, as the bash path: a raw chunk can end mid-line
            // (the sentinel split across a boundary would flash in the relay), so the
            // trailing partial line is held back until its newline arrives.
            let mut line_buf = Vec::new();
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        let keep = n.min(MAX_CAPTURE_BYTES.saturating_sub(captured.len()));
                        captured.extend_from_slice(&chunk[..keep]);
                        if keep > 0 {
                            // Live output, as the bash path: complete lines, the sentinel
                            // line filtered, before the run's own masking of the full result.
                            line_buf.extend_from_slice(&chunk[..keep]);
                            while let Some(end) = line_buf.iter().position(|&byte| byte == b'\n') {
                                let line: Vec<u8> = line_buf.drain(..=end).collect();
                                if !line.starts_with(sentinel.as_bytes()) {
                                    on_chunk(&String::from_utf8_lossy(&line));
                                }
                            }
                        }
                        dropped_bytes += n - keep;
                        if keep < n {
                            dropped_tail.extend_from_slice(&chunk[keep..n]);
                            let excess = dropped_tail.len().saturating_sub(8 * 1024);
                            dropped_tail.drain(..excess);
                        }
                    }
                }
            }
        };
        let (timed_out, interrupted) = match tokio::time::timeout(timeout, async {
            tokio::select! {
                biased;
                _ = read => false,
                _ = stopped(cancel) => true,
            }
        })
        .await
        {
            Ok(interrupted) => (false, interrupted),
            Err(_) => (true, false),
        };
        let mut text = String::from_utf8_lossy(&captured).into_owned();
        if dropped_bytes > 0 {
            // The sentinel line is in the dropped part: keep only whole lines, and say so.
            if let Some(end) = text.rfind('\n') {
                text.truncate(end);
            }
            text.push_str(&format!(
                "\n[... {dropped_bytes} more bytes of output were not kept ...]\n"
            ));
            let tail = String::from_utf8_lossy(&dropped_tail);
            if let Some(line) = tail.lines().find(|l| l.starts_with(&sentinel)) {
                text.push_str(line);
                text.push('\n');
            }
        }
        let mut exit_code = None;
        let mut lines = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix(&sentinel) {
                let (code, cwd) = rest.split_once(' ').unwrap_or((rest, ""));
                exit_code = Some(code.parse().unwrap_or(-1));
                if workdir.is_none() && !cwd.is_empty() {
                    *self.cwd.lock() = PathBuf::from(cwd);
                }
            } else {
                lines.push(line);
            }
        }
        if timed_out || interrupted {
            drop(killer);
        } else {
            killer.disarm();
            let _ = child.wait().await;
        }
        let mut output = lines.join("\n").trim_end().to_string();
        let mut exit_code = exit_code;
        // No sentinel: PowerShell stopped before running the command (a syntax error such as `&&`,
        // which Windows PowerShell 5.1 doesn't have). Say why instead of returning nothing.
        if exit_code.is_none() && !timed_out && !interrupted {
            if let Some(task) = stderr_task {
                if let Ok(Ok(bytes)) = tokio::time::timeout(Duration::from_secs(2), task).await {
                    let errors = powershell_errors(&bytes);
                    if !errors.is_empty() {
                        output =
                            format!("{output}\nPowerShell could not run the command:\n{errors}")
                                .trim()
                                .to_string();
                        exit_code = Some(1);
                    }
                }
            }
        }
        RunResult {
            output,
            exit_code,
            timed_out,
            shell_restarted: false,
            interrupted,
        }
    }
}

/// The error text in what PowerShell wrote on stderr: plain text, or CLIXML (`#< CLIXML` then
/// `<S S="Error">…</S>` pieces with `_x000D__x000A_` for line ends) when stderr is redirected.
fn powershell_errors(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if !text.contains("#< CLIXML") {
        return text.trim().to_string();
    }
    let mut out = String::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("<S S=\"Error\">") {
        rest = &rest[start + "<S S=\"Error\">".len()..];
        let Some(end) = rest.find("</S>") else { break };
        out.push_str(&rest[..end]);
        rest = &rest[end..];
    }
    out.replace("_x000D__x000A_", "\n")
        .replace("_x000D_", "")
        .replace("_x000A_", "\n")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

pub(crate) fn command_for_powershell(cwd: &Path, scratch_dir: &Path) -> Command {
    command(Path::new("powershell.exe"), cwd, scratch_dir)
}

/// `-EncodedCommand` takes base64 of UTF-16LE; unlike `-Command -` it runs multi-line blocks.
pub(crate) fn encode_powershell(script: &str) -> String {
    use base64::Engine as _;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// Keeps the head and tail of long output and saves the whole text to the scratch dir.
fn limit_output(output: String, scratch_dir: &Path) -> (String, Option<PathBuf>) {
    let total = output.chars().count();
    if total <= MAX_OUTPUT_CHARS {
        return (output, None);
    }
    let dir = scratch_dir.join("terminal-output");
    let path = dir.join(format!("{}.txt", uuid::Uuid::new_v4().simple()));
    let saved = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&path, &output))
        .is_ok();
    let head: String = output.chars().take(HEAD_CHARS).collect();
    let tail: String = output.chars().skip(total - TAIL_CHARS).collect();
    let note = if saved {
        format!("full output saved to {}", display_path(&path))
    } else {
        "full output could not be saved".to_string()
    };
    (
        format!(
            "{head}\n\n[... {} characters omitted; {note} — read it with read_file ...]\n\n{tail}",
            total - HEAD_CHARS - TAIL_CHARS
        ),
        saved.then_some(path),
    )
}

#[derive(Deserialize)]
struct TerminalArgs {
    command: String,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    notify: bool,
}

pub async fn terminal(
    tools: &SessionTools,
    args: Value,
    live: Option<(&str, &Emitter)>,
) -> ToolOutput {
    let args: TerminalArgs = match parse_args("terminal", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    if args.command.trim().is_empty() {
        return ToolOutput::error("command is empty.");
    }
    let workdir = match args.workdir.as_deref() {
        Some(dir) => {
            let dir = tools.resolve(dir);
            if !dir.is_dir() {
                return ToolOutput::error(format!(
                    "workdir does not exist: {}",
                    display_path(&dir)
                ));
            }
            Some(dir)
        }
        None => None,
    };
    let approved = match tools.check_command(&args.command).await {
        Ok(approved) => approved,
        Err(blocked) => return blocked,
    };
    let approval_note = approved.map(|description| {
        format!("Command required approval ({description}) and was approved by the user.")
    });
    if args.background {
        let mut started = crate::process::spawn(tools, &args.command, workdir, args.notify).await;
        if let Some(note) = approval_note {
            started.value["approval"] = json!(note);
        }
        return started;
    }

    let timeout = Duration::from_secs(
        args.timeout
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS),
    );
    let started = Instant::now();
    // Live output: each captured chunk is masked (the same redaction the full result gets)
    // and relayed as a `tool.progress` event, the accumulated tail so the clicked-open row
    // shows the output growing. Throttled to one per 500 ms (Hermes's `tool.progress`
    // `_MIN_INTERVAL_S`); the finished result's masking still runs on the whole output, so
    // a secret split across chunk boundaries can only flash in the relay, never in the result.
    let relay = live.map(|(tool_call_id, emit)| {
        let state = std::sync::Mutex::new(Relay {
            last: None,
            text: String::new(),
            bytes: 0,
        });
        let command = args.command.clone();
        (tool_call_id, emit, state, command)
    });
    let on_chunk = |chunk: &str| {
        let Some((tool_call_id, emit, state, command)) = &relay else {
            return;
        };
        let masked = crate::redact::redact_terminal_output(chunk, command);
        let mut relay = state.lock().unwrap_or_else(|e| e.into_inner());
        relay.text.push_str(&masked);
        let chars = relay.text.chars().count();
        if chars > RELAY_TAIL_CHARS {
            relay.text = relay.text.chars().skip(chars - RELAY_TAIL_CHARS).collect();
        }
        relay.bytes += chunk.len() as u64;
        let now = Instant::now();
        if relay
            .last
            .is_some_and(|last| now.duration_since(last) < RELAY_INTERVAL)
        {
            return;
        }
        relay.last = Some(now);
        emit.emit(hermes_agent::events::AgentEvent::ToolProgress {
            tool_call_id: tool_call_id.to_string(),
            name: "terminal".to_string(),
            progress: relay.bytes as f64,
            message: relay.text.clone(),
        });
    };
    let result = tools
        .shell
        .run(
            &args.command,
            workdir.as_deref(),
            timeout,
            live.map(|(_, emit)| emit.cancel_token()),
            &on_chunk,
        )
        .await;
    // Secrets are masked before the output is trimmed or saved (Hermes redacts the spill too).
    let output = crate::redact::redact_terminal_output(&result.output, &args.command);
    let (output, saved) = limit_output(output, tools.shell.scratch_dir());
    let cwd = display_path(&tools.shell.cwd());
    let mut value = json!({
        "output": output,
        "exit_code": result.exit_code,
        "cwd": cwd,
        "duration_ms": started.elapsed().as_millis() as u64,
    });
    if let Some(note) = approval_note {
        value["approval"] = json!(note);
    }
    if let Some(path) = saved {
        value["output_file"] = json!(display_path(&path));
    }
    if result.timed_out {
        value["timed_out"] = json!(true);
        return ToolOutput::error_with(
            format!(
                "Command timed out after {}s and was killed. The shell restarts in {cwd} (exported variables were lost). \
                 For long-running work use background=true.",
                timeout.as_secs()
            ),
            value,
        );
    }
    if result.shell_restarted {
        value["note"] = json!(
            "The shell exited during this command and will restart in the same directory; exported variables were lost."
        );
    }
    // The turn was stopped while the command ran; the output is what it had printed. The tool
    // response is kept (the state machine saves it), so the row shows the partial output too.
    if result.interrupted {
        value["interrupted"] = json!(true);
        value["note"] = json!(
            "The command was stopped before it finished; the output above is what it had printed."
        );
    }
    ToolOutput::ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::tools_in;

    #[test]
    fn powershells_clixml_errors_read_as_text() {
        let clixml = "#< CLIXML\r\n<Objs Version=\"1.1.0.1\"><S S=\"Error\">At line:1 char:5_x000D__x000A_</S><S S=\"Error\">The token &apos;&amp;&amp;&apos; is not a valid statement separator.</S></Objs>";
        assert_eq!(
            powershell_errors(clixml.as_bytes()),
            "At line:1 char:5\nThe token '&&' is not a valid statement separator."
        );
        assert_eq!(powershell_errors(b"plain failure\r\n"), "plain failure");
        assert_eq!(powershell_errors(b""), "");
    }

    #[test]
    fn secret_looking_variables_are_dropped_unless_passed_through() {
        let parent = || {
            [
                "PATH",
                "SystemRoot",
                "USERPROFILE",
                "GIT_AUTHOR_NAME",
                "OPENAI_API_KEY",
                "gh_token",
                "DB_PASSWORD",
                "AWS_SECRET_ACCESS_KEY",
                "SSH_AUTH_SOCK",
                "hermes_rs_or_api_key",
                "MY_CREDENTIALS",
                "PASSWD_FILE",
                "GIT_AUTHOR_TOKEN",
            ]
            .into_iter()
            .map(|name| (name.into(), "v".into()))
        };
        let names = |vars: Vec<(std::ffi::OsString, std::ffi::OsString)>| -> Vec<String> {
            vars.into_iter()
                .map(|(n, _)| n.to_string_lossy().into_owned())
                .collect()
        };
        let basics = ["PATH", "SystemRoot", "USERPROFILE", "GIT_AUTHOR_NAME"];
        assert_eq!(names(scrubbed_env(parent(), &[])), basics);
        // The passthrough list is matched case-insensitively.
        let kept = names(scrubbed_env(
            parent(),
            &["OPENAI_API_KEY".into(), "ssh_auth_sock".into()],
        ));
        assert_eq!(
            kept,
            [&basics[..], &["OPENAI_API_KEY", "SSH_AUTH_SOCK"]].concat()
        );
    }

    #[tokio::test]
    async fn the_shell_does_not_see_secret_variables_but_keeps_the_basics() {
        // Process-global state: names unique to this test.
        unsafe {
            std::env::set_var("HERMES_TEST_SCRUB_TOKEN", "scrub-me-12345");
            std::env::set_var("HERMES_TEST_KEEP_TOKEN", "keep-me-67890");
        }
        set_env_passthrough(vec!["hermes_test_keep_token".into()]);
        let (_dir, tools) = tools_in();
        let list = if bash_available(&tools) {
            "echo keep=$HERMES_TEST_KEEP_TOKEN scrub=$HERMES_TEST_SCRUB_TOKEN root=$SYSTEMROOT utf=$PYTHONUTF8 path=${PATH:+set}"
        } else {
            "\"keep=$($env:HERMES_TEST_KEEP_TOKEN) scrub=$($env:HERMES_TEST_SCRUB_TOKEN) root=$($env:SystemRoot) utf=$($env:PYTHONUTF8) path=$(if ($env:Path) {'set'})\""
        };
        let out = terminal(&tools, json!({"command": list}), None).await;
        set_env_passthrough(Vec::new());
        let text = out.value["output"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        assert!(text.contains("keep=keep-me-67890"), "{text}");
        assert!(text.contains("scrub= "), "{text}");
        // SystemRoot survives the scrub (Windows programs break without it).
        assert!(text.contains("root=c:"), "{text}");
        assert!(text.contains("path=set"), "{text}");
        // The terminal's own settings still apply.
        assert!(text.contains("utf=1"), "{text}");
    }

    fn bash_available(tools: &SessionTools) -> bool {
        matches!(tools.shell.kind(), ShellKind::GitBash(_))
    }

    #[tokio::test]
    async fn state_persists_between_calls() {
        let (dir, tools) = tools_in();
        if !bash_available(&tools) {
            return;
        }
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let out = terminal(
            &tools,
            json!({"command": "export GREETING=hello && cd sub"}),
            None,
        )
        .await;
        assert_eq!(out.value["exit_code"], 0, "{:?}", out.value);
        let out = terminal(&tools, json!({"command": "echo $GREETING; pwd -W"}), None).await;
        let text = out.value["output"].as_str().unwrap();
        assert!(text.starts_with("hello\n"), "{text}");
        assert!(text.trim_end().ends_with("/sub"), "{text}");
        assert!(out.value["cwd"].as_str().unwrap().ends_with("/sub"));
        tools.shutdown().await;
    }

    #[tokio::test]
    async fn exit_codes_and_stderr_are_captured() {
        let (_dir, tools) = tools_in();
        if !bash_available(&tools) {
            return;
        }
        let out = terminal(
            &tools,
            json!({"command": "echo out; echo err >&2; false"}),
            None,
        )
        .await;
        assert_eq!(out.value["exit_code"], 1);
        let text = out.value["output"].as_str().unwrap();
        assert!(text.contains("out") && text.contains("err"), "{text}");
        tools.shutdown().await;
    }

    #[tokio::test]
    async fn workdir_does_not_change_the_session_cwd() {
        let (dir, tools) = tools_in();
        if !bash_available(&tools) {
            return;
        }
        std::fs::create_dir(dir.path().join("other")).unwrap();
        let before = tools.cwd();
        let out = terminal(
            &tools,
            json!({"command": "pwd -W", "workdir": "other"}),
            None,
        )
        .await;
        assert!(out.value["output"].as_str().unwrap().ends_with("/other"));
        assert_eq!(tools.cwd(), before);
        tools.shutdown().await;
    }

    #[tokio::test]
    async fn timeout_kills_and_the_next_command_works() {
        let (_dir, tools) = tools_in();
        if !bash_available(&tools) {
            return;
        }
        let out = terminal(&tools, json!({"command": "sleep 30", "timeout": 1}), None).await;
        assert!(out.is_error);
        assert_eq!(out.value["timed_out"], true);
        let out = terminal(&tools, json!({"command": "echo alive"}), None).await;
        assert_eq!(out.value["output"], "alive");
        tools.shutdown().await;
    }

    #[tokio::test]
    async fn exit_in_a_command_restarts_the_shell() {
        let (_dir, tools) = tools_in();
        if !bash_available(&tools) {
            return;
        }
        let out = terminal(&tools, json!({"command": "exit 3"}), None).await;
        assert!(out.value["note"].as_str().is_some(), "{:?}", out.value);
        let out = terminal(&tools, json!({"command": "echo back"}), None).await;
        assert_eq!(out.value["output"], "back");
        tools.shutdown().await;
    }

    #[test]
    fn long_output_keeps_head_and_tail_and_saves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let output = format!("START{}END", "x".repeat(50_000));
        let (limited, saved) = limit_output(output.clone(), dir.path());
        assert!(limited.starts_with("START") && limited.ends_with("END"));
        assert!(limited.len() < 30_000);
        assert_eq!(std::fs::read_to_string(saved.unwrap()).unwrap(), output);
    }

    /// A table is not padded out to the 4096-column width it is formatted at (a `ls` listing was
    /// thousands of spaces wide per row).
    #[tokio::test]
    async fn a_powershell_table_has_no_trailing_padding() {
        let dir = tempfile::tempdir().unwrap();
        let shell = ShellSession::new(
            ShellKind::PowerShell,
            dir.path().to_path_buf(),
            dir.path().join("scratch"),
        );
        let result = shell
            .run(
                "[pscustomobject]@{Name='one'; Size=1}, [pscustomobject]@{Name='two'; Size=2} | Format-Table",
                None,
                Duration::from_secs(30),
                None,
                &|_| {},
            )
            .await;
        assert_eq!(result.exit_code, Some(0), "{:?}", result.output);
        assert!(result.output.contains("one"), "{:?}", result.output);
        for line in result.output.lines() {
            assert_eq!(line, line.trim_end(), "padded: {line:?}");
        }
    }

    /// A command Windows PowerShell can't parse (`&&`) says why, instead of returning nothing.
    #[tokio::test]
    async fn a_powershell_syntax_error_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let shell = ShellSession::new(
            ShellKind::PowerShell,
            dir.path().to_path_buf(),
            dir.path().join("scratch"),
        );
        let result = shell
            .run(
                "Write-Output a && Write-Output b",
                None,
                Duration::from_secs(30),
                None,
                &|_| {},
            )
            .await;
        assert_eq!(result.exit_code, Some(1), "{:?}", result.output);
        assert!(
            result.output.contains("could not run the command"),
            "{:?}",
            result.output
        );
    }

    /// The PowerShell path (no Git Bash on the machine) relays its live output too: whole
    /// lines only, never the run's own sentinel line (the bash path filters it; the nonce
    /// would flash in the relay).
    #[tokio::test]
    async fn the_powershell_relay_takes_whole_lines_without_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let shell = ShellSession::new(
            ShellKind::PowerShell,
            dir.path().to_path_buf(),
            dir.path().join("scratch"),
        );
        let relayed = std::sync::Mutex::new(Vec::new());
        let result = shell
            .run(
                "Write-Output one; Start-Sleep -Milliseconds 700; Write-Output two",
                None,
                Duration::from_secs(30),
                None,
                &|line| relayed.lock().unwrap().push(line.to_string()),
            )
            .await;
        assert_eq!(result.exit_code, Some(0), "{:?}", result.output);
        let relayed = relayed.into_inner().unwrap();
        assert!(!relayed.is_empty(), "no live output was relayed");
        assert!(
            relayed.iter().any(|line| line.contains("one")),
            "{relayed:?}"
        );
        assert!(
            relayed.iter().any(|line| line.contains("two")),
            "{relayed:?}"
        );
        assert!(
            relayed.iter().all(|line| !line.contains("__HERMES_DONE_")),
            "{relayed:?}"
        );
    }
}
