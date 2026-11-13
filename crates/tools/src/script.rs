//! The `run_script` tool: several commands, reads or searches in one model call (CP10.1).
//!
//! Hermes: `tools/code_execution_tool.py` (`execute_code`: one script instead of a chain of tool
//! calls, only its output returns to the model; output caps and a timeout). Only that idea is
//! ported. Hermes's script calls Hermes's tools over an RPC channel and keeps a persistent
//! kernel; ours is a plain script in PowerShell, Python or bash that uses the machine's own
//! commands and libraries, runs once, and leaves nothing behind. The PORTING.md row is
//! `diverged`.
//!
//! **How it runs.** The code is checked ([`crate::script_guard`] and
//! [`SessionTools::check_script`]), written to `<scratch>/scripts/<random>/script.<ext>` and run
//! from there with the session's working folder as its cwd, the scrubbed environment of the
//! terminal tool (no secret-looking variables), stdin closed, no console window, and in its own
//! job object so the whole process tree dies on timeout, on the turn being stopped, and when the
//! script ends. The script is passed as a file, never as a command-line string: nothing the
//! model wrote is ever parsed by a second shell. The scratch folder is removed afterwards, also
//! after a kill or a stopped turn.
//!
//! **Languages.** `powershell` is Windows PowerShell (by its full path under `%SystemRoot%`);
//! `python` is the interpreter `uv python find --system` names (never the `python` on PATH, which
//! on Windows is often the Microsoft Store stub, never a repository's `.venv`, never a download);
//! `bash` needs Git for Windows' `bash.exe` (run without `BASH_ENV` and exported functions). A
//! missing interpreter is a plain error, before anyone is asked.
//!
//! **Limits.** `code` at most 64 KB; `timeout` defaults to 60 s, at most 600 s; stdout and stderr
//! are read up to a fixed cap each and the result is trimmed like the terminal's (head and tail,
//! the whole text saved to a file whose path is in the result). No live output relay, no
//! background mode: a script that starts something long-running is killed when it ends.
//!
//! **Approval is an aid, not a sandbox.** See [`crate::script_guard`] and `docs/PITFALLS.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use hermes_agent::operation::Emitter;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinHandle;

use crate::approval::{self, ApprovalChoice, CommandApproval};
use crate::path_policy;
use crate::script_guard::{self, Analysis, Language};
use crate::shell::{self, KillOnDrop, ProcessTree, ShellKind};
use crate::{ApprovalMode, SessionTools, ToolOutput, display_path, parse_args};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
/// The largest script accepted, in bytes.
pub(crate) const MAX_CODE_BYTES: usize = 64 * 1024;
/// The longest script an approval card shows. A script that needs a decision and is longer than
/// this is refused: the user must not approve text they can't see.
const MAX_PROMPT_CHARS: usize = 16_000;
/// Output kept in memory per stream; the rest is counted and dropped.
const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;
/// What of stderr the result carries (head and tail).
const STDERR_HEAD_CHARS: usize = 3_000;
const STDERR_TAIL_CHARS: usize = 6_000;

#[derive(Deserialize)]
struct ScriptArgs {
    language: String,
    code: String,
    /// Seconds: an integer, a float or a numeric string (models send all three); anything else,
    /// zero or negative is the default.
    #[serde(default)]
    timeout: Option<Value>,
}

/// The timeout a call asked for, within 1..=600 s.
fn timeout_secs(value: Option<&Value>) -> u64 {
    let seconds = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    match seconds {
        Some(s) if s.is_finite() && s > 0.0 => (s.ceil() as u64).clamp(1, MAX_TIMEOUT_SECS),
        _ => DEFAULT_TIMEOUT_SECS,
    }
}

// --- Interpreters ------------------------------------------------------------------------------

/// `uv.exe`: on PATH, else where its installer puts it. Only absolute PATH entries count: a
/// relative one (`.`) would be read against the app's own folder of the moment.
fn find_uv(path_dirs: &[PathBuf], extra: &[PathBuf]) -> Option<PathBuf> {
    path_dirs
        .iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("uv.exe"))
        .chain(extra.iter().cloned())
        .find(|candidate| candidate.is_file())
}

/// Windows PowerShell by its full path (`%SystemRoot%\System32\WindowsPowerShell\v1.0`), so no
/// `powershell.exe` elsewhere on PATH (or in a folder the search order reaches) stands in for it.
fn powershell_exe() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| {
            PathBuf::from(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

/// The Python uv would use *outside* any virtual environment (`uv python find --system`): a
/// repository's `.venv` (whose `python.exe` and `.pth` files are the repository's code) is never
/// picked, nor is a `.python-version` or `uv.toml` in it read. Run from the script's scratch
/// folder, with the scrubbed environment; nothing is downloaded.
async fn python_via_uv(uv: &Path, scratch: &Path) -> Result<PathBuf, String> {
    let mut command = shell::command(uv, scratch, scratch);
    command
        .args([
            "python",
            "find",
            "--system",
            "--no-project",
            "--no-config",
            "--no-python-downloads",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| "uv did not answer which Python to use within 30 s.".to_string())?
        .map_err(|e| format!("Could not start uv: {e}"))?;
    let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let path = PathBuf::from(&found);
    if output.status.success() && path.is_absolute() && path.is_file() {
        Ok(path)
    } else {
        let why = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!(
            "No Python is installed for run_script to use (uv python find: {}). Install one with \
             `uv python install` or use language=powershell.",
            if why.is_empty() { found } else { why }
        ))
    }
}

fn uv_for(home: &Path) -> Option<PathBuf> {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let mut extra = vec![home.join(".local").join("bin").join("uv.exe")];
    if let Some(real_home) = dirs::home_dir() {
        extra.push(real_home.join(".local").join("bin").join("uv.exe"));
    }
    if let Some(cargo) = std::env::var_os("CARGO_HOME") {
        extra.push(PathBuf::from(cargo).join("bin").join("uv.exe"));
    }
    find_uv(&path_dirs, &extra)
}

/// Git for Windows' bash: the session's own shell when that is Git Bash, else the one on this
/// machine. Never WSL's `bash.exe` (`detect_shell` skips it).
fn bash_for(session_shell: &ShellKind, detected: ShellKind) -> Option<PathBuf> {
    match (session_shell, detected) {
        (ShellKind::GitBash(path), _) => Some(path.clone()),
        (_, ShellKind::GitBash(path)) => Some(path),
        _ => None,
    }
}

const NO_UV: &str = "Python is not available to run_script: uv was not found (looked on PATH and in \
~/.local/bin). Install uv (https://docs.astral.sh/uv/) or use language=powershell. run_script never \
uses the `python` on PATH, which on Windows is often the Microsoft Store stub.";

const NO_BASH: &str = "bash is not available to run_script: Git for Windows' bash.exe was not \
found. Use language=powershell.";

/// What to start, besides the script file.
struct Launch {
    /// For Python, `uv.exe` until [`python_via_uv`] has named the interpreter.
    program: PathBuf,
    /// Arguments before the script's path (for PowerShell, the whole command line instead).
    prefix: Vec<OsString>,
}

fn launch_for(tools: &SessionTools, language: Language) -> Result<Launch, &'static str> {
    match language {
        Language::PowerShell => Ok(Launch {
            program: powershell_exe(),
            prefix: Vec::new(),
        }),
        Language::Python => {
            let uv = uv_for(&tools.home).ok_or(NO_UV)?;
            Ok(Launch {
                program: uv,
                prefix: ["-X", "utf8"].map(OsString::from).to_vec(),
            })
        }
        Language::Bash => {
            let bash = bash_for(tools.shell.kind(), shell::detect_shell()).ok_or(NO_BASH)?;
            Ok(Launch {
                program: bash,
                prefix: ["--noprofile", "--norc"].map(OsString::from).to_vec(),
            })
        }
    }
}

fn extension(language: Language) -> &'static str {
    match language {
        Language::PowerShell => "ps1",
        Language::Python => "py",
        Language::Bash => "sh",
    }
}

// --- The scratch folder ------------------------------------------------------------------------

/// A non-interactive bash sources `$BASH_ENV` (and `sh` mode `$ENV`) before the script, and
/// imports exported functions (`BASH_FUNC_name%%`) that can stand in for any command: none of
/// them reach a script.
fn strip_bash_startup(command: &mut tokio::process::Command, names: impl Iterator<Item = OsString>) {
    command.env_remove("BASH_ENV").env_remove("ENV");
    for name in names {
        if name.to_string_lossy().starts_with("BASH_FUNC_") {
            command.env_remove(name);
        }
    }
}

/// Run folders older than this are leftovers of a crash (a run lasts at most 600 s).
const STALE_RUN_DIR: Duration = Duration::from_secs(3600);

/// `<scratch>/scripts/<random>/` with the script file in it, removed when it goes out of scope
/// (also when the turn is stopped and the future dropped) and explicitly, with retries, after a
/// normal run.
///
/// The file is written, then held open **for reading only, sharing only reads** until the
/// script has ended: no other process (a script running in parallel, a background command) can
/// change, replace, rename or delete it, or rename the folder it sits in, between the check and
/// the run or while the interpreter reads it (bash reads a script as it goes, so a script could
/// otherwise append to itself). What runs is read back through that handle and compared with
/// what was checked; a folder that is a junction or symlink is refused.
struct RunDir {
    path: PathBuf,
    file: PathBuf,
    lock: Option<std::fs::File>,
    removed: bool,
}

impl RunDir {
    /// File-system work: call it from `spawn_blocking`.
    fn create(scratch: &Path, extension: &str, body: &[u8]) -> std::io::Result<Self> {
        use std::io::{Read, Write};
        let scripts = scratch.join("scripts");
        sweep_stale(&scripts);
        let path = scripts.join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&path)?;
        let mut dir = Self {
            file: path.join(format!("script.{extension}")),
            path,
            lock: None,
            removed: false,
        };
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dir.file)?
            .write_all(body)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 0x1;
            options.share_mode(FILE_SHARE_READ);
        }
        let mut lock = options.open(&dir.file)?;
        let mut written = Vec::with_capacity(body.len());
        lock.read_to_end(&mut written)?;
        dir.lock = Some(lock);
        if written != body {
            return Err(std::io::Error::other("the script file changed before it ran"));
        }
        if is_link(&dir.path) || is_link(&scripts) {
            return Err(std::io::Error::other(
                "the script's scratch folder is a junction or symbolic link",
            ));
        }
        Ok(dir)
    }

    /// Lets go of the file (the script has ended).
    fn unlock(&mut self) {
        self.lock = None;
    }

    /// Removes the folder, retrying while Windows still holds a handle of a killed process.
    async fn remove(&mut self) {
        self.unlock();
        let path = self.path.clone();
        let removed = tokio::task::spawn_blocking(move || remove_dir_retrying(&path, 40))
            .await
            .unwrap_or(false);
        self.removed = removed;
    }
}

fn remove_dir_retrying(path: &Path, attempts: u32) -> bool {
    for attempt in 0..attempts {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) if attempt + 1 == attempts => break,
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    false
}

impl Drop for RunDir {
    fn drop(&mut self) {
        self.unlock();
        if self.removed {
            return;
        }
        let path = self.path.clone();
        // The process may still be dying (the guard that kills it is dropped before this one),
        // and this may run on an async worker: removed on a thread of its own, with retries.
        std::thread::spawn(move || {
            remove_dir_retrying(&path, 100);
        });
    }
}

/// Whether a path is a reparse point (junction, symbolic link).
fn is_link(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        std::fs::symlink_metadata(path)
            .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .unwrap_or(true)
    }
    #[cfg(not(windows))]
    {
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
    }
}

/// Removes run folders a crash left behind (older than [`STALE_RUN_DIR`]); never follows a link.
fn sweep_stale(scripts: &Path) {
    let Ok(entries) = std::fs::read_dir(scripts) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_RUN_DIR);
        if stale {
            let path = entry.path();
            if is_link(&path) {
                let _ = std::fs::remove_dir(&path).or_else(|_| std::fs::remove_file(&path));
            } else {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

// --- Capturing output --------------------------------------------------------------------------

/// Reads a pipe to its end, keeping the first `cap` bytes; returns them and how many were dropped.
fn read_capped(mut reader: impl AsyncRead + Unpin + Send + 'static, cap: usize) -> JoinHandle<(Vec<u8>, usize)> {
    tokio::spawn(async move {
        let mut kept = Vec::new();
        let mut dropped = 0usize;
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let keep = n.min(cap.saturating_sub(kept.len()));
                    kept.extend_from_slice(&chunk[..keep]);
                    dropped += n - keep;
                }
            }
        }
        (kept, dropped)
    })
}

async fn finished(handle: JoinHandle<(Vec<u8>, usize)>) -> (String, usize) {
    // The tree is dead, so the pipe ends at once; the wait is for a process that escaped the job.
    match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok((bytes, dropped))) => {
            let text = String::from_utf8_lossy(&bytes);
            let text = text.trim_start_matches('\u{feff}');
            // PowerShell wraps some of what it writes to stderr in CLIXML.
            let text = if text.starts_with("#< CLIXML") {
                shell::powershell_errors(&bytes)
            } else {
                text.replace("\r\n", "\n")
            };
            (text, dropped)
        }
        _ => (String::new(), 0),
    }
}

/// Head and tail of a long text with a note between them.
fn clip_middle(text: &str, head: usize, tail: usize) -> String {
    let total = text.chars().count();
    if total <= head + tail {
        return text.to_string();
    }
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(total - tail).collect();
    format!("{start}\n[... {} characters omitted ...]\n{end}", total - head - tail)
}

/// Passes that mask secrets in the output: a script that dumps the environment or reads a secret
/// file gets the stronger one (the terminal's rule, by the words in the script).
fn redact_output(output: &str, code: &str) -> String {
    let lower = code.to_lowercase();
    let dumps = [
        "env:", "environ", "getenv", "printenv", ".env", "id_rsa", ".netrc", "credentials",
        ".npmrc", ".pypirc",
    ]
    .iter()
    .any(|word| lower.contains(word));
    let source = if dumps {
        crate::redact::Source::Assignments
    } else {
        crate::redact::Source::Output
    };
    crate::redact::redact(output, source)
}

fn powershell_driver(script: &Path) -> String {
    // Our own text, with only the random path in it. `$LASTEXITCODE` is the script's `exit n`
    // (or the last native command's code), else success is `$?`: the terminal's rule.
    format!(
        "$ProgressPreference = 'SilentlyContinue'\n\
         [Console]::OutputEncoding = [Text.Encoding]::UTF8\n\
         & {}\n\
         $hermesExit = if ($null -ne $LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}\n\
         exit $hermesExit\n",
        shell::powershell_quote(&script.display().to_string())
    )
}

enum Outcome {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Interrupted,
}

// --- The tool ----------------------------------------------------------------------------------

pub(crate) async fn run_script(
    tools: &SessionTools,
    args: Value,
    live: Option<(&str, &Emitter)>,
) -> ToolOutput {
    let args: ScriptArgs = match parse_args("run_script", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    let Some(language) = Language::parse(&args.language) else {
        return ToolOutput::error(format!(
            "language must be one of powershell, python or bash (got '{}').",
            args.language.chars().take(40).collect::<String>()
        ));
    };
    if args.code.trim().is_empty() {
        return ToolOutput::error("code is empty.");
    }
    if args.code.len() > MAX_CODE_BYTES {
        return ToolOutput::error(format!(
            "code is {} bytes; the limit is {MAX_CODE_BYTES}. Split the work into several scripts \
             or write a file with write_file and run it with terminal.",
            args.code.len()
        ));
    }
    if args.code.contains('\0') {
        return ToolOutput::error("code contains a NUL character.");
    }
    // What is checked is what runs: one spelling of line ends for both.
    let code = args.code.replace("\r\n", "\n").replace('\r', "\n");
    // No interpreter, no question for the user.
    let launch = match launch_for(tools, language) {
        Ok(launch) => launch,
        Err(message) => return ToolOutput::error(message),
    };
    let analysis = {
        let code = code.clone();
        match tokio::task::spawn_blocking(move || script_guard::analyze(language, &code)).await {
            Ok(analysis) => analysis,
            Err(error) => {
                return ToolOutput::error(format!("The script could not be checked: {error}"));
            }
        }
    };
    let approved = match tools.check_script(language, &code, &analysis).await {
        Ok(approved) => approved,
        Err(blocked) => return blocked,
    };
    let approval_note = approved.map(|description| {
        format!("Script required approval ({description}) and was approved by the user.")
    });
    let timeout = Duration::from_secs(timeout_secs(args.timeout.as_ref()));
    let cwd = tools.cwd();
    let started = Instant::now();

    let body = match language {
        // Windows PowerShell reads a script without a byte-order mark as the ANSI code page.
        Language::PowerShell => format!("\u{feff}{code}"),
        _ => code.clone(),
    };
    let created = {
        let scratch = tools.scratch_dir().to_path_buf();
        let extension = extension(language);
        tokio::task::spawn_blocking(move || RunDir::create(&scratch, extension, body.as_bytes()))
            .await
    };
    let mut dir = match created {
        Ok(Ok(dir)) => dir,
        Ok(Err(error)) => {
            return ToolOutput::error(format!("Could not write the script to its scratch folder: {error}"));
        }
        Err(error) => return ToolOutput::error(format!("Could not write the script: {error}")),
    };
    let file = dir.file.clone();
    // The change snapshot (checkpoints) is taken by `SessionTools::call` before this runs.

    let program = match language {
        Language::Python => match python_via_uv(&launch.program, &dir.path).await {
            Ok(python) => python,
            Err(message) => {
                dir.remove().await;
                return ToolOutput::error(message);
            }
        },
        _ => launch.program.clone(),
    };
    let mut command = shell::command(&program, &cwd, tools.scratch_dir());
    if language == Language::Bash {
        strip_bash_startup(&mut command, std::env::vars_os().map(|(name, _)| name));
    }
    match language {
        Language::PowerShell => {
            command
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-OutputFormat",
                    "Text",
                    "-EncodedCommand",
                ])
                .arg(shell::encode_powershell(&powershell_driver(&file)));
        }
        Language::Python => {
            command.args(&launch.prefix).arg(&file);
        }
        Language::Bash => {
            command.args(&launch.prefix).arg(display_path(&file));
        }
    }
    let spawned = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            dir.remove().await;
            return ToolOutput::error(format!("Could not start {}: {error}", program.display()));
        }
    };
    let tree = ProcessTree::contain(child.id());
    let mut killer = KillOnDrop::new(tree.clone());
    let stdout = read_capped(child.stdout.take().expect("piped stdout"), MAX_STDOUT_BYTES);
    let stderr = read_capped(child.stderr.take().expect("piped stderr"), MAX_STDERR_BYTES);
    let cancel = live.map(|(_, emit)| emit.cancel_token());
    let outcome = tokio::select! {
        biased;
        status = child.wait() => Outcome::Exited(status),
        _ = shell::stopped(cancel) => Outcome::Interrupted,
        _ = tokio::time::sleep(timeout) => Outcome::TimedOut,
    };
    // Whatever the script started goes with it; then the pipes end and the output is complete.
    tree.kill();
    if !matches!(outcome, Outcome::Exited(_)) {
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    }
    killer.disarm();
    let (out_text, out_dropped) = finished(stdout).await;
    let (err_text, err_dropped) = finished(stderr).await;
    dir.remove().await;

    let exit_code = match &outcome {
        Outcome::Exited(Ok(status)) => status.code(),
        _ => None,
    };
    let mut out_text = out_text;
    if out_dropped > 0 {
        out_text.push_str(&format!(
            "\n[... {out_dropped} more bytes of output were not kept ...]"
        ));
    }
    let output = redact_output(&out_text, &code);
    let (output, saved) = shell::limit_output(output, tools.scratch_dir());
    let mut stderr_text = redact_output(&err_text, &code);
    if err_dropped > 0 {
        stderr_text.push_str(&format!(
            "\n[... {err_dropped} more bytes of stderr were not kept ...]"
        ));
    }
    let stderr_text = clip_middle(stderr_text.trim_end(), STDERR_HEAD_CHARS, STDERR_TAIL_CHARS);

    let mut value = json!({
        "output": output.trim_end(),
        "exit_code": exit_code,
        "language": language.name(),
        "cwd": display_path(&cwd),
        "duration_ms": started.elapsed().as_millis() as u64,
    });
    if !stderr_text.is_empty() {
        value["stderr"] = json!(stderr_text);
    }
    if let Some(note) = approval_note {
        value["approval"] = json!(note);
    }
    if let Some(path) = saved {
        value["output_file"] = json!(display_path(&path));
    }
    match outcome {
        Outcome::TimedOut => {
            value["timed_out"] = json!(true);
            ToolOutput::error_with(
                format!(
                    "Script timed out after {}s and was killed (with everything it started). The \
                     output above is what it had printed.",
                    timeout.as_secs()
                ),
                value,
            )
        }
        Outcome::Interrupted => {
            value["interrupted"] = json!(true);
            value["note"] = json!(
                "The script was stopped before it finished; the output above is what it had printed."
            );
            ToolOutput::ok(value)
        }
        Outcome::Exited(Err(error)) => ToolOutput::error_with(
            format!("Waiting for the script failed: {error}"),
            value,
        ),
        Outcome::Exited(Ok(_)) => ToolOutput::ok(value),
    }
}

// --- Approval ----------------------------------------------------------------------------------

impl SessionTools {
    /// Whether a script may run: `Ok` carries the reasons when the user just approved it, `Err`
    /// is the result for the model.
    ///
    /// The same machinery as [`SessionTools::check_command`], with these differences (all on the
    /// strict side, because a script can't be proven safe by reading it):
    /// * a hardline line anywhere blocks the script, in every mode;
    /// * a script with a *dynamic* construct ([`Analysis::dynamic`]) asks in every mode but Yolo
    ///   and is never judged by the smart reviewer;
    /// * a script that names a persistence place asks even in Yolo;
    /// * flagged scripts on a tainted session always ask (Yolo warns and runs, decision D2);
    /// * the answers "for this session" and "always" are treated as "once", and remembered
    ///   approvals of the terminal (session or allowlist) are not consulted: a script is asked
    ///   about every time;
    /// * an obvious path literal outside the working folder flags the script (reads too).
    pub(crate) async fn check_script(
        &self,
        language: Language,
        code: &str,
        analysis: &Analysis,
    ) -> Result<Option<String>, ToolOutput> {
        let blocked = |message: String| {
            ToolOutput::error_with(
                message,
                json!({"output": "", "exit_code": -1, "status": "blocked"}),
            )
        };
        if let Some(description) = analysis.hardline {
            tracing::warn!("hardline block in a script: {description}");
            return Err(blocked(approval::hardline_message(description)));
        }
        let Some(store) = &self.approvals else {
            return Ok(None);
        };
        let mode = self.mode();
        let path_flag = self.command_flag(code).await;
        let outside = self.script_outside_path(code).await;
        let persistence = path_flag.as_ref().is_some_and(|flag| flag.persistence)
            || !analysis.persistence.is_empty();

        let mut reasons: Vec<String> = Vec::new();
        reasons.extend(analysis.persistence.iter().map(|d| d.to_string()));
        reasons.extend(analysis.dynamic.iter().map(|d| d.to_string()));
        reasons.extend(analysis.dangerous.iter().map(|d| d.to_string()));
        if let Some(flag) = &path_flag {
            reasons.push(flag.description.to_string());
        }
        if outside.is_some() {
            reasons.push(
                "names a path outside the working folder (a script's reads and writes can't be told apart)"
                    .to_string(),
            );
        }
        let flagged = !reasons.is_empty();
        let dynamic = !analysis.dynamic.is_empty();
        let taint_reason = self.taint_reason().filter(|_| flagged);
        let tainted = taint_reason.is_some();
        let always_ask = matches!(mode, ApprovalMode::Manual | ApprovalMode::Plan)
            || persistence
            || (mode != ApprovalMode::Yolo && (dynamic || tainted));
        let must_ask = match mode {
            ApprovalMode::Yolo => persistence,
            ApprovalMode::Manual | ApprovalMode::Plan => true,
            ApprovalMode::AcceptEdits | ApprovalMode::Auto => flagged,
        };
        if !must_ask {
            if tainted && mode == ApprovalMode::Yolo {
                self.warn_tainted(&format!("run_script ({})", language.name()));
            }
            return Ok(None);
        }

        let mut description = if reasons.is_empty() {
            "run a script".to_string()
        } else {
            let shown: Vec<&str> = reasons.iter().take(3).map(String::as_str).collect();
            match reasons.len() - shown.len() {
                0 => shown.join("; "),
                more => format!("{}; and {more} more", shown.join("; ")),
            }
        };
        if let Some(reason) = &taint_reason {
            description = format!("{description} ({reason})");
        }
        // The user approves what they can see: the whole script, within a bound, with every
        // control, bidirectional and invisible character shown as an escape instead of acting
        // on the card (an RLO or ESC sequence could otherwise hide or reorder the script's tail).
        let shown = visible(code);
        let chars = shown.chars().count();
        if chars > MAX_PROMPT_CHARS {
            return Err(blocked(format!(
                "This script needs approval ({description}) but is {chars} characters long, more \
                 than an approval card shows ({MAX_PROMPT_CHARS}); the user must see everything \
                 they approve. Split it into smaller scripts."
            )));
        }
        let request = CommandApproval {
            command: format!("run_script ({})\n{shown}", language.name()),
            description: description.clone(),
            once_only: true,
        };
        let guard = self.guard.lock().clone();
        let smart = !always_ask && (store.smart() || mode == ApprovalMode::Auto);
        if let Some(guard) = guard.filter(|_| smart) {
            match guard.assess(&request.command, &request.description).await {
                approval::Verdict::Approve => return Ok(Some(description)),
                approval::Verdict::Deny => {
                    return Err(blocked(approval::smart_denied_message(&description)));
                }
                approval::Verdict::Escalate => {}
            }
        }
        let answer = tokio::time::timeout(
            crate::APPROVAL_TIMEOUT,
            self.bridge.approve_command(&self.session_id, request),
        )
        .await;
        match answer {
            // "For this session" and "always" are not offered (`once_only`) and not honoured.
            Ok(ApprovalChoice::Once | ApprovalChoice::Session | ApprovalChoice::Always) => {
                if taint_reason.is_some() && mode == ApprovalMode::Yolo {
                    self.warn_tainted(&format!("run_script ({})", language.name()));
                }
                Ok(Some(description))
            }
            Ok(ApprovalChoice::Deny) => Err(blocked(approval::denied_message())),
            Err(_) => Err(blocked(approval::timeout_message())),
        }
    }

    /// The first obvious path literal of the script that points outside the working folder.
    pub(crate) async fn script_outside_path(&self, code: &str) -> Option<PathBuf> {
        let ctx = path_policy::PolicyContext::new(
            self.cwd(),
            self.scratch_dir.clone(),
            self.data_dir.clone(),
            self.home.clone(),
        )
        .with_project(self.project_root());
        let code = code.to_string();
        tokio::task::spawn_blocking(move || path_policy::script_outside_path(&code, &ctx))
            .await
            .unwrap_or(None)
    }
}

/// The script as the approval card shows it: tabs and line ends kept, every other control
/// character, and every invisible or bidirectional formatting character, written as `\u{XXXX}`.
pub(crate) fn visible(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    for c in code.chars() {
        let hidden = (c.is_control() && c != '\n' && c != '\t')
            || matches!(c,
                '\u{ad}' | '\u{34f}' | '\u{61c}' | '\u{115f}' | '\u{1160}' | '\u{17b4}' | '\u{17b5}'
                | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                | '\u{205f}'..='\u{206f}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}'
                | '\u{ffa0}' | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e0fff}');
        if hidden {
            out.push_str(&format!("\\u{{{:04x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Fetches from the network in a script (Python's libraries, `Invoke-WebRequest`, `curl`): the
/// session is tainted afterwards, as after a terminal command that fetches.
pub(crate) fn fetch_source(code: &str) -> Option<String> {
    static PYTHON_FETCH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(?:urllib|urlopen|requests\s*\.\s*(?:get|post|request|Session)|httpx|aiohttp|http\.client|pycurl|ftplib|socket\s*\.\s*create_connection)\b",
        )
        .expect("python fetch pattern")
    });
    crate::taint::command_source(code).or_else(|| {
        PYTHON_FETCH
            .is_match(code)
            .then(|| "the output of a script that fetched from the network".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::approval::ApprovalStore;
    use crate::{SessionToolsConfig, UserBridge};

    // -- Fixtures

    /// Answers every approval with `answer` and remembers what was asked.
    struct AskBridge {
        answer: ApprovalChoice,
        asked: parking_lot::Mutex<Vec<(String, String, bool)>>,
    }

    impl AskBridge {
        fn new(answer: ApprovalChoice) -> Arc<Self> {
            Arc::new(Self {
                answer,
                asked: Default::default(),
            })
        }
    }

    #[async_trait::async_trait]
    impl UserBridge for AskBridge {
        async fn clarify(
            &self,
            _session_id: &str,
            _request: crate::ClarifyRequest,
        ) -> crate::ClarifyOutcome {
            crate::ClarifyOutcome::Undelivered
        }
        fn process_finished(&self, _session_id: &str, _notice: crate::ProcessNotice) {}
        async fn approve_command(
            &self,
            _session_id: &str,
            request: CommandApproval,
        ) -> ApprovalChoice {
            self.asked
                .lock()
                .push((request.command, request.description, request.once_only));
            self.answer
        }
    }

    struct ApproveAll;

    #[async_trait::async_trait]
    impl approval::ApprovalGuard for ApproveAll {
        async fn assess(&self, _command: &str, _description: &str) -> approval::Verdict {
            approval::Verdict::Approve
        }
    }

    fn guarded(bridge: Arc<dyn UserBridge>) -> (tempfile::TempDir, SessionTools) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let tools = SessionTools::new(SessionToolsConfig {
            session_id: "ses_test".into(),
            cwd: work,
            home: dir.path().join("home"),
            data_dir: dir.path().join("data"),
            scratch_dir: dir.path().join("data").join("scratch"),
            shell: shell::detect_shell(),
            bridge,
            todos: Vec::new(),
            memory: None,
            history: None,
            skills: None,
            approvals: Some(Arc::new(ApprovalStore::new(&dir.path().join("data")))),
            checkpoints: None,
            web: None,
        });
        (dir, tools)
    }

    async fn check(tools: &SessionTools, language: Language, code: &str) -> Result<Option<String>, ToolOutput> {
        let analysis = script_guard::analyze(language, code);
        tools.check_script(language, code, &analysis).await
    }

    const DYNAMIC: &str = "iex $x";
    const DANGEROUS: &str = "Remove-Item -Recurse .\\build";
    const BENIGN: &str = "Get-ChildItem | Select-Object -First 3";

    fn has_powershell() -> bool {
        which("powershell.exe")
    }

    fn which(name: &str) -> bool {
        std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
            .unwrap_or(false)
    }

    // -- Approval

    #[tokio::test]
    async fn a_benign_script_runs_without_asking_in_the_asking_modes() {
        for mode in [ApprovalMode::AcceptEdits, ApprovalMode::Auto, ApprovalMode::Yolo] {
            let bridge = AskBridge::new(ApprovalChoice::Deny);
            let (_dir, tools) = guarded(bridge.clone());
            tools.set_mode(mode);
            assert!(check(&tools, Language::PowerShell, BENIGN).await.unwrap().is_none(), "{mode:?}");
            assert!(bridge.asked.lock().is_empty(), "{mode:?}");
        }
    }

    #[tokio::test]
    async fn manual_and_plan_ask_about_every_script() {
        for mode in [ApprovalMode::Manual, ApprovalMode::Plan] {
            let bridge = AskBridge::new(ApprovalChoice::Once);
            let (_dir, tools) = guarded(bridge.clone());
            tools.set_mode(mode);
            assert!(check(&tools, Language::PowerShell, BENIGN).await.unwrap().is_some());
            let asked = bridge.asked.lock().clone();
            assert_eq!(asked.len(), 1, "{mode:?}");
            // The whole script is on the card, once only.
            assert!(asked[0].0.contains(BENIGN) && asked[0].2, "{mode:?}");
            assert_eq!(asked[0].1, "run a script");
        }
    }

    #[tokio::test]
    async fn a_destructive_script_asks_and_a_denial_blocks_it() {
        for code in [
            "Remove-Item -Recurse C:\\",
            "Write-Output hi\nRemove-Item -Recurse -Force .\\build",
        ] {
            for mode in [ApprovalMode::AcceptEdits, ApprovalMode::Auto] {
                let bridge = AskBridge::new(ApprovalChoice::Deny);
                let (_dir, tools) = guarded(bridge.clone());
                tools.set_mode(mode);
                let blocked = check(&tools, Language::PowerShell, code).await.unwrap_err();
                assert!(blocked.is_error && blocked.value["status"] == "blocked", "{code}");
                assert!(blocked.value["error"].as_str().unwrap().starts_with("BLOCKED: User denied"));
                let asked = bridge.asked.lock().clone();
                assert_eq!(asked.len(), 1, "{mode:?} {code}");
                assert!(asked[0].1.contains("destructive delete"), "{:?}", asked[0].1);
                assert!(asked[0].0.contains(code));
            }
        }
    }

    #[tokio::test]
    async fn a_hardline_line_blocks_in_every_mode_without_asking() {
        for mode in [
            ApprovalMode::Manual,
            ApprovalMode::AcceptEdits,
            ApprovalMode::Auto,
            ApprovalMode::Yolo,
        ] {
            let bridge = AskBridge::new(ApprovalChoice::Once);
            let (_dir, tools) = guarded(bridge.clone());
            tools.set_mode(mode);
            let blocked = check(&tools, Language::Bash, "echo ok\nrm -rf /").await.unwrap_err();
            assert!(blocked.value["error"].as_str().unwrap().starts_with("BLOCKED (hardline)"), "{mode:?}");
            assert!(bridge.asked.lock().is_empty(), "{mode:?}");
        }
    }

    #[tokio::test]
    async fn a_dynamic_script_always_asks_and_is_never_smart_approved_or_remembered() {
        for mode in [ApprovalMode::AcceptEdits, ApprovalMode::Auto] {
            let bridge = AskBridge::new(ApprovalChoice::Always);
            let (_dir, tools) = guarded(bridge.clone());
            tools.set_mode(mode);
            tools.set_guard(Some(Arc::new(ApproveAll)));
            // "Always" and "for this session" are answers to a card that offers neither: asked
            // again every time, and the allowlist stays empty.
            for _ in 0..3 {
                assert!(check(&tools, Language::PowerShell, DYNAMIC).await.unwrap().is_some());
            }
            let asked = bridge.asked.lock().clone();
            assert_eq!(asked.len(), 3, "{mode:?}");
            assert!(asked.iter().all(|a| a.2 && a.1.contains("Invoke-Expression")), "{asked:?}");
            assert!(tools.approvals.as_ref().unwrap().always_allowed().is_empty());
            assert!(tools.session_approved.lock().is_empty());
        }
    }

    #[tokio::test]
    async fn a_dangerous_static_script_is_judged_by_the_reviewer_in_auto_mode_only() {
        let bridge = AskBridge::new(ApprovalChoice::Deny);
        let (_dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::Auto);
        tools.set_guard(Some(Arc::new(ApproveAll)));
        assert!(check(&tools, Language::PowerShell, DANGEROUS).await.unwrap().is_some());
        assert!(bridge.asked.lock().is_empty());
        tools.set_mode(ApprovalMode::AcceptEdits);
        assert!(check(&tools, Language::PowerShell, DANGEROUS).await.is_err());
        assert_eq!(bridge.asked.lock().len(), 1);
    }

    #[tokio::test]
    async fn remembered_terminal_approvals_do_not_cover_scripts() {
        let bridge = AskBridge::new(ApprovalChoice::Deny);
        let (_dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::AcceptEdits);
        let store = tools.approvals.as_ref().unwrap();
        for command in [DANGEROUS, "rm -rf build"] {
            store.allow_always(approval::dangerous(command).unwrap());
            tools.session_approved.lock().insert(approval::dangerous(command).unwrap().to_string());
        }
        assert!(check(&tools, Language::PowerShell, DANGEROUS).await.is_err());
        assert_eq!(bridge.asked.lock().len(), 1);
    }

    #[tokio::test]
    async fn yolo_runs_flagged_scripts_and_warns_when_tainted_but_persistence_still_asks() {
        let bridge = AskBridge::new(ApprovalChoice::Deny);
        let (_dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::Yolo);
        assert!(check(&tools, Language::PowerShell, DYNAMIC).await.unwrap().is_none());
        assert!(check(&tools, Language::PowerShell, DANGEROUS).await.unwrap().is_none());
        assert!(tools.take_taint_warnings().is_empty());
        assert!(bridge.asked.lock().is_empty());
        tools.mark_tainted("a web page".into()).await;
        assert!(check(&tools, Language::PowerShell, DYNAMIC).await.unwrap().is_none());
        assert_eq!(
            tools.take_taint_warnings(),
            ["ran after reading untrusted content: a web page"]
        );
        // A benign script on a tainted session: no warning (nothing was flagged).
        assert!(check(&tools, Language::PowerShell, BENIGN).await.unwrap().is_none());
        assert!(tools.take_taint_warnings().is_empty());
        assert!(bridge.asked.lock().is_empty());
        // A persistence place asks even in Yolo.
        let startup = "Set-Content \"$env:APPDATA\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\x.bat\" 'calc'";
        assert!(check(&tools, Language::PowerShell, startup).await.is_err());
        assert_eq!(bridge.asked.lock().len(), 1);
    }

    #[tokio::test]
    async fn a_tainted_session_always_asks_for_flagged_scripts_and_says_why() {
        for mode in [ApprovalMode::AcceptEdits, ApprovalMode::Auto] {
            let bridge = AskBridge::new(ApprovalChoice::Session);
            let (_dir, tools) = guarded(bridge.clone());
            tools.set_mode(mode);
            tools.set_guard(Some(Arc::new(ApproveAll)));
            tools.mark_tainted("a web page".into()).await;
            for _ in 0..2 {
                assert!(check(&tools, Language::PowerShell, DANGEROUS).await.unwrap().is_some());
            }
            let asked = bridge.asked.lock().clone();
            assert_eq!(asked.len(), 2, "{mode:?}");
            assert!(asked[1].1.ends_with("(this turn read a web page)"), "{:?}", asked[1].1);
            // Benign scripts are not asked about, tainted or not.
            assert!(check(&tools, Language::PowerShell, BENIGN).await.unwrap().is_none());
            assert_eq!(bridge.asked.lock().len(), 2);
        }
    }

    #[tokio::test]
    async fn an_outside_path_literal_asks_and_one_in_the_working_folder_does_not() {
        let bridge = AskBridge::new(ApprovalChoice::Deny);
        let (dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::AcceptEdits);
        let inside = format!("Get-Content '{}'", dir.path().join("work").join("a.txt").display());
        assert!(check(&tools, Language::PowerShell, &inside).await.unwrap().is_none());
        assert!(check(&tools, Language::PowerShell, "Get-Content ..\\..\\secret.txt").await.is_err());
        assert!(check(&tools, Language::Python, "print(open(r'C:\\Windows\\win.ini').read())").await.is_err());
        assert!(check(&tools, Language::Bash, "cat /etc/hosts").await.is_err());
        let asked = bridge.asked.lock().clone();
        assert_eq!(asked.len(), 3);
        assert!(asked.iter().all(|a| a.1.contains("outside the working folder")), "{asked:?}");
        // Yolo lets it through.
        tools.set_mode(ApprovalMode::Yolo);
        assert!(check(&tools, Language::Bash, "cat /etc/hosts").await.unwrap().is_none());
        // Regular expressions and URLs are not paths.
        tools.set_mode(ApprovalMode::AcceptEdits);
        assert!(check(&tools, Language::Python, "import re\nre.findall(r'/\\d+/', 'x')\nprint('https://a.test/x/y')").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn persistence_and_app_data_paths_in_a_script_are_flagged() {
        let bridge = AskBridge::new(ApprovalChoice::Deny);
        let (dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::AcceptEdits);
        let data = dir.path().join("data").join("desktop-config.json");
        let code = format!("Get-Content '{}'", data.display());
        assert!(check(&tools, Language::PowerShell, &code).await.is_err());
        let hooks = "Set-Content .git/hooks/pre-commit 'calc'";
        assert!(check(&tools, Language::PowerShell, hooks).await.is_err());
        assert_eq!(bridge.asked.lock().len(), 2);
    }

    #[tokio::test]
    async fn a_script_too_long_for_the_card_is_refused_when_it_needs_a_decision() {
        let bridge = AskBridge::new(ApprovalChoice::Once);
        let (_dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::AcceptEdits);
        let code = format!("iex $x\n# {}", "x".repeat(MAX_PROMPT_CHARS));
        let blocked = check(&tools, Language::PowerShell, &code).await.unwrap_err();
        assert!(blocked.value["error"].as_str().unwrap().contains("must see everything"));
        assert!(bridge.asked.lock().is_empty());
        // Long but benign: nothing to decide.
        let long = format!("# {}", "x".repeat(MAX_PROMPT_CHARS));
        assert!(check(&tools, Language::PowerShell, &long).await.unwrap().is_none());
    }

    // -- Execution (real interpreters; skipped when absent)

    async fn run(tools: &SessionTools, language: &str, code: &str, timeout: Option<u64>) -> ToolOutput {
        let mut args = json!({"language": language, "code": code});
        if let Some(timeout) = timeout {
            args["timeout"] = json!(timeout);
        }
        run_script(tools, args, None).await
    }

    fn scripts_left(tools: &SessionTools) -> usize {
        std::fs::read_dir(tools.scratch_dir().join("scripts"))
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn powershell_output_exit_code_stderr_and_cleanup() {
        if !has_powershell() {
            return;
        }
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        let ok = run(&tools, "powershell", "Write-Output 'hello \u{e9}'\n1 + 2", None).await;
        assert!(!ok.is_error, "{:?}", ok.value);
        assert_eq!(ok.value["exit_code"], 0);
        assert_eq!(ok.value["output"], "hello \u{e9}\n3", "{:?}", ok.value);
        assert_eq!(ok.value["language"], "powershell");
        assert!(ok.value.get("stderr").is_none());
        // The script ran in the session's working folder, from a scratch file.
        let cwd = run(&tools, "powershell", "(Get-Location).Path\n$PSCommandPath", None).await;
        let text = cwd.value["output"].as_str().unwrap().to_string();
        let mut lines = text.lines();
        assert!(lines.next().unwrap().to_lowercase().ends_with("work"), "{text}");
        assert!(lines.next().unwrap().replace('\\', "/").contains("/scratch/scripts/"), "{text}");
        // A nonzero exit and stderr.
        let failed = run(&tools, "powershell", "[Console]::Error.WriteLine('oops')\nexit 7", None).await;
        assert!(!failed.is_error);
        assert_eq!(failed.value["exit_code"], 7);
        assert!(failed.value["stderr"].as_str().unwrap().contains("oops"), "{:?}", failed.value);
        // A terminating error is a failure with the message.
        let thrown = run(&tools, "powershell", "throw 'bad thing'", None).await;
        assert_ne!(thrown.value["exit_code"], 0);
        assert!(thrown.value["stderr"].as_str().unwrap().contains("bad thing"), "{:?}", thrown.value);
        assert_eq!(scripts_left(&tools), 0);
    }

    #[tokio::test]
    async fn a_powershell_timeout_kills_the_process_tree_and_removes_the_script() {
        if !has_powershell() {
            return;
        }
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        let started = Instant::now();
        let timed_out = run(
            &tools,
            "powershell",
            "Write-Output 'before'\nStart-Sleep -Seconds 120\nWrite-Output 'after'",
            Some(2),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
        assert!(timed_out.is_error);
        assert_eq!(timed_out.value["timed_out"], true);
        assert!(timed_out.value["error"].as_str().unwrap().contains("timed out after 2s"));
        assert!(timed_out.value["output"].as_str().unwrap().contains("before"));
        assert!(!timed_out.value["output"].as_str().unwrap().contains("after"));
        assert_eq!(scripts_left(&tools), 0);
    }

    #[tokio::test]
    async fn large_output_is_trimmed_to_head_and_tail_with_the_rest_saved() {
        if !has_powershell() {
            return;
        }
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        let big = run(&tools, "powershell", "1..20000 | ForEach-Object { \"line $_ of the output\" }", Some(120)).await;
        assert!(!big.is_error, "{:?}", big.value);
        let text = big.value["output"].as_str().unwrap();
        assert!(text.chars().count() < 31_000, "{}", text.chars().count());
        assert!(text.contains("line 1 of") && text.contains("line 20000 of"));
        assert!(text.contains("characters omitted"));
        let saved = big.value["output_file"].as_str().unwrap();
        assert!(std::fs::read_to_string(saved).unwrap().contains("line 10000 of"));
    }

    #[tokio::test]
    async fn the_script_does_not_see_secret_variables() {
        if !has_powershell() {
            return;
        }
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        // SAFETY: tests in this crate share the process environment; the name is unique to this one.
        unsafe { std::env::set_var("RUN_SCRIPT_TEST_TOKEN", "abc123secret") };
        let seen = run(&tools, "powershell", "[Environment]::GetEnvironmentVariable('RUN_SCRIPT_TEST_TOKEN')\n$env:SystemRoot", None).await;
        unsafe { std::env::remove_var("RUN_SCRIPT_TEST_TOKEN") };
        let text = seen.value["output"].as_str().unwrap();
        assert!(!text.contains("abc123secret"), "{text}");
        assert!(text.to_lowercase().contains("windows"), "{text}");
    }

    #[tokio::test]
    async fn python_runs_through_uv() {
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        if uv_for(&tools.home).is_none() {
            // No uv: a plain error, nothing asked, nothing written.
            let error = run(&tools, "python", "print(1)", None).await;
            assert!(error.is_error && error.value["error"].as_str().unwrap().contains("uv was not found"));
            return;
        }
        let ok = run(&tools, "python", "import sys\nprint('h\u{e9}llo')\nprint('err', file=sys.stderr)\nsys.exit(3)", Some(120)).await;
        if ok.is_error && ok.value["error"].as_str().unwrap_or("").contains("No Python is installed") {
            return;
        }
        assert_eq!(ok.value["exit_code"], 3, "{:?}", ok.value);
        assert_eq!(ok.value["output"], "h\u{e9}llo");
        assert!(ok.value["stderr"].as_str().unwrap().contains("err"), "{:?}", ok.value);
        assert_eq!(scripts_left(&tools), 0);
    }

    #[tokio::test]
    async fn bash_runs_when_git_bash_exists() {
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        if bash_for(tools.shell.kind(), shell::detect_shell()).is_none() {
            let error = run(&tools, "bash", "echo hi", None).await;
            assert!(error.is_error && error.value["error"].as_str().unwrap().contains("bash.exe was not found"));
            return;
        }
        let ok = run(&tools, "bash", "echo hi\r\necho there >&2\r\nexit 4", None).await;
        assert_eq!(ok.value["exit_code"], 4, "{:?}", ok.value);
        assert_eq!(ok.value["output"], "hi");
        assert!(ok.value["stderr"].as_str().unwrap().contains("there"));
        assert_eq!(scripts_left(&tools), 0);
    }

    #[tokio::test]
    async fn the_whole_tool_asks_then_runs_and_a_denied_script_leaves_no_trace() {
        if !has_powershell() {
            return;
        }
        let code = "Set-Content -Path made.txt -Value hi\nRemove-Item -Recurse -Force nothing-here -ErrorAction SilentlyContinue";
        for (answer, runs) in [(ApprovalChoice::Once, true), (ApprovalChoice::Deny, false)] {
            let bridge = AskBridge::new(answer);
            let (dir, tools) = guarded(bridge.clone());
            tools.set_mode(ApprovalMode::AcceptEdits);
            let result = tools
                .dispatch("run_script", json!({"language": "powershell", "code": code}), None)
                .await;
            let made = dir.path().join("work").join("made.txt");
            assert_eq!(made.exists(), runs, "{:?}", result.value);
            assert_eq!(result.is_error, !runs, "{:?}", result.value);
            assert_eq!(bridge.asked.lock().len(), 1);
            assert!(bridge.asked.lock()[0].0.contains("Set-Content -Path made.txt"));
            assert_eq!(scripts_left(&tools), 0);
            if runs {
                assert!(result.value["approval"].as_str().unwrap().contains("approved by the user"));
            }
        }
    }

    #[tokio::test]
    async fn stopping_the_turn_kills_the_script_and_removes_its_scratch_folder() {
        if !has_powershell() {
            return;
        }
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        let emit = Emitter::new(tx, cancel.clone());
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            stopper.cancel();
        });
        let started = Instant::now();
        let result = run_script(
            &tools,
            json!({"language": "powershell", "code": "Write-Output begun\nStart-Sleep -Seconds 120", "timeout": 300}),
            Some(("call_1", &emit)),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
        assert_eq!(result.value["interrupted"], true, "{:?}", result.value);
        assert!(result.value["output"].as_str().unwrap().contains("begun"));
        assert_eq!(scripts_left(&tools), 0);
    }

    #[tokio::test]
    async fn dropping_the_call_kills_the_script_too() {
        if !has_powershell() {
            return;
        }
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        let marker = tools.cwd().join("still-running.txt");
        let code = "Start-Sleep -Seconds 4\nSet-Content still-running.txt 'x'";
        let call = run_script(&tools, json!({"language": "powershell", "code": code}), None);
        // The future is dropped (the turn's task aborted) while the script runs.
        let _ = tokio::time::timeout(Duration::from_millis(1500), call).await;
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(!marker.exists(), "the script outlived its call");
        assert_eq!(scripts_left(&tools), 0);
    }

    #[tokio::test]
    async fn a_script_naming_a_skill_script_the_agent_wrote_asks() {
        let bridge = AskBridge::new(ApprovalChoice::Deny);
        let (dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::AcceptEdits);
        let script = dir.path().join("data").join("skills").join("x").join("scripts").join("run.ps1");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "Write-Output hi").unwrap();
        let code = format!("& '{}'", script.display());
        assert!(check(&tools, Language::PowerShell, &code).await.is_err());
        assert!(bridge.asked.lock()[0].1.contains("runs a script the agent wrote into a skill"), "{:?}", bridge.asked.lock());
    }

    #[tokio::test]
    async fn the_output_is_masked_like_the_terminals() {
        if !has_powershell() {
            return;
        }
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        let result = run(
            &tools,
            "powershell",
            "Write-Output 'key sk-proj-abcdefghijklmnopqrstuvwxyz0123456789'",
            None,
        )
        .await;
        let text = result.value["output"].as_str().unwrap();
        assert!(!text.contains("abcdefghijklmnopqrstuvwxyz0123456789"), "{text}");
    }

    // -- Errors

    #[tokio::test]
    async fn bad_arguments_are_plain_errors_and_nothing_runs() {
        let bridge = AskBridge::new(ApprovalChoice::Once);
        let (_dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::Manual);
        for (args, expect) in [
            (json!({"language": "ruby", "code": "puts 1"}), "language must be one of"),
            (json!({"language": "", "code": "x"}), "language must be one of"),
            (json!({"language": "powershell", "code": "   \n"}), "code is empty"),
            (json!({"language": "powershell", "code": "a".repeat(MAX_CODE_BYTES + 1)}), "the limit is 65536"),
            (json!({"language": "powershell", "code": "a\u{0}b"}), "NUL"),
            (json!({"code": "echo"}), "Invalid arguments for run_script"),
        ] {
            let error = run_script(&tools, args, None).await;
            assert!(error.is_error, "{expect}");
            assert!(error.value["error"].as_str().unwrap().contains(expect), "{:?}", error.value);
        }
        // Nobody was asked about any of them.
        assert!(bridge.asked.lock().is_empty());
        assert_eq!(scripts_left(&tools), 0);
    }

    #[test]
    fn a_missing_uv_or_bash_is_a_clear_error_not_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_uv(&[dir.path().to_path_buf()], &[dir.path().join("none.exe")]).is_none());
        std::fs::write(dir.path().join("uv.exe"), "").unwrap();
        assert_eq!(find_uv(&[dir.path().to_path_buf()], &[]), Some(dir.path().join("uv.exe")));
        assert!(bash_for(&ShellKind::PowerShell, ShellKind::PowerShell).is_none());
        let bash = PathBuf::from("C:/Git/bin/bash.exe");
        assert_eq!(bash_for(&ShellKind::PowerShell, ShellKind::GitBash(bash.clone())), Some(bash.clone()));
        assert_eq!(bash_for(&ShellKind::GitBash(bash.clone()), ShellKind::PowerShell), Some(bash));
        assert!(NO_UV.contains("Microsoft Store") && NO_BASH.contains("powershell"));
    }

    // -- Review (CP10.1): hardening

    #[test]
    fn timeouts_in_any_shape_are_clamped() {
        assert_eq!(timeout_secs(None), DEFAULT_TIMEOUT_SECS);
        assert_eq!(timeout_secs(Some(&json!(30))), 30);
        assert_eq!(timeout_secs(Some(&json!(2.5))), 3);
        assert_eq!(timeout_secs(Some(&json!("45"))), 45);
        assert_eq!(timeout_secs(Some(&json!(1e12))), MAX_TIMEOUT_SECS);
        assert_eq!(timeout_secs(Some(&json!(u64::MAX))), MAX_TIMEOUT_SECS);
        for bad in [json!(-5), json!(0), json!("soon"), json!(null), json!([1]), json!("NaN"), json!("inf")] {
            assert_eq!(timeout_secs(Some(&bad)), DEFAULT_TIMEOUT_SECS, "{bad}");
        }
    }

    #[test]
    fn the_card_shows_control_and_bidi_characters_as_escapes() {
        let shown = visible("a\u{202e}b\u{1b}[2Kc\u{200b}d\te\nf\u{8}");
        assert_eq!(shown, "a\\u{202e}b\\u{001b}[2Kc\\u{200b}d\te\nf\\u{0008}");
    }

    #[tokio::test]
    async fn the_card_carries_the_escaped_script() {
        let bridge = AskBridge::new(ApprovalChoice::Deny);
        let (_dir, tools) = guarded(bridge.clone());
        tools.set_mode(ApprovalMode::AcceptEdits);
        let code = "Write-Output ok # \u{202e}\niex $x";
        assert!(check(&tools, Language::PowerShell, code).await.is_err());
        let asked = bridge.asked.lock().clone();
        assert!(asked[0].0.contains("\\u{202e}") && !asked[0].0.contains('\u{202e}'), "{asked:?}");
    }

    #[tokio::test]
    async fn credentials_and_login_scripts_named_in_a_script_ask_even_in_yolo() {
        for (language, code) in [
            (Language::Python, "import pathlib\np = pathlib.Path.home() / '.ssh' / 'authorized_keys'"),
            (Language::PowerShell, "Add-Content $PROFILE 'x'"),
            (Language::Bash, "git config core.hooksPath h"),
        ] {
            let bridge = AskBridge::new(ApprovalChoice::Deny);
            let (_dir, tools) = guarded(bridge.clone());
            tools.set_mode(ApprovalMode::Yolo);
            assert!(check(&tools, language, code).await.is_err(), "{code}");
            assert_eq!(bridge.asked.lock().len(), 1, "{code}");
        }
    }

    #[test]
    fn the_script_file_cannot_be_changed_while_it_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunDir::create(dir.path(), "ps1", b"Write-Output checked").unwrap();
        // Another process (a parallel script) tries to replace, append to, delete or move it.
        assert!(std::fs::write(&run.file, "iex $x").is_err());
        assert!(std::fs::OpenOptions::new().append(true).open(&run.file).is_err());
        assert!(std::fs::remove_file(&run.file).is_err());
        assert!(std::fs::rename(&run.path, dir.path().join("moved")).is_err());
        // Reading it (the interpreter) still works.
        assert_eq!(std::fs::read_to_string(&run.file).unwrap(), "Write-Output checked");
        let path = run.path.clone();
        drop(run);
        for _ in 0..100 {
            if !path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!path.exists());
    }

    #[test]
    fn a_junction_as_the_scripts_folder_is_refused_and_stale_runs_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let linked = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(scratch.join("scripts"))
            .arg(&elsewhere)
            .output()
            .is_ok_and(|o| o.status.success());
        if linked {
            assert!(RunDir::create(&scratch, "ps1", b"x").is_err());
        }
        // A leftover older than an hour goes; a fresh one stays.
        let scripts = dir.path().join("s2").join("scripts");
        let old = scripts.join("old");
        std::fs::create_dir_all(&old).unwrap();
        let hour_ago = std::time::SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::options().write(true).open(&old).map(|f| f.set_modified(hour_ago)).ok();
        let fresh = RunDir::create(&dir.path().join("s2"), "ps1", b"x").unwrap();
        if std::fs::metadata(&old).unwrap().modified().unwrap() <= hour_ago + Duration::from_secs(1) {
            let again = RunDir::create(&dir.path().join("s2"), "ps1", b"y").unwrap();
            assert!(!old.exists());
            drop(again);
        }
        assert!(fresh.path.exists());
    }

    #[tokio::test]
    async fn python_never_uses_a_repository_virtual_environment() {
        let (_dir, tools) = guarded(AskBridge::new(ApprovalChoice::Deny));
        tools.set_mode(ApprovalMode::Yolo);
        let Some(uv) = uv_for(&tools.home) else {
            return;
        };
        // A `.venv` in the working folder (a hostile repository could ship one).
        let made = std::process::Command::new(&uv)
            .args(["venv", "-q", ".venv"])
            .current_dir(tools.cwd())
            .output()
            .is_ok_and(|o| o.status.success());
        if !made {
            return;
        }
        let result = run(&tools, "python", "import sys\nprint(sys.executable)\nprint(sys.prefix)", Some(120)).await;
        if result.is_error {
            return; // no system Python on this machine
        }
        let text = result.value["output"].as_str().unwrap().to_lowercase();
        assert!(!text.contains(".venv"), "{text}");
    }

    #[test]
    fn bash_startup_variables_are_removed() {
        let mut command = tokio::process::Command::new("bash.exe");
        command.env("BASH_ENV", "x").env("ENV", "y").env("BASH_FUNC_ls%%", "() { calc; }").env("KEEP", "1");
        strip_bash_startup(&mut command, ["BASH_FUNC_ls%%", "KEEP"].into_iter().map(OsString::from));
        let envs: Vec<(String, bool)> = command
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().to_string(), v.is_some()))
            .collect();
        for name in ["BASH_ENV", "ENV", "BASH_FUNC_ls%%"] {
            assert!(envs.contains(&(name.to_string(), false)), "{name}: {envs:?}");
        }
        assert!(envs.contains(&("KEEP".to_string(), true)));
    }

    #[test]
    fn network_fetching_scripts_are_taint_sources_and_others_are_not() {
        for code in [
            "Invoke-WebRequest https://a.test/x",
            "curl -s https://a.test",
            "import requests\nrequests.get('https://a.test')",
            "from urllib.request import urlopen",
        ] {
            assert!(fetch_source(code).is_some(), "{code}");
        }
        for code in ["Get-ChildItem", "print('requests are fine')", "ls -la"] {
            assert!(fetch_source(code).is_none(), "{code}");
        }
    }
}
