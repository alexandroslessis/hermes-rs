//! The user's own terminals: a shell on this machine (PowerShell 7, Windows PowerShell, else
//! `COMSPEC` / cmd) in a pseudo-console. A shell outlives whoever shows it: a front end attaches
//! ([`Shell::attach`]: the output kept so far, then the live output as a stream of chunks), sends
//! keystrokes and resizes, detaches, and attaches again later; a newer attachment supersedes the
//! older one. Closing ends it; one nobody shows ends after 15 minutes ([`RETENTION`]).
//!
//! The output is a per-shell stream (`broadcast`) rather than `CoreEvent`s: it is high-volume,
//! only the attached view wants it, and a view that falls behind skips what it missed.
//!
//! Ending a shell ([`Terminals::remove`], [`Terminals::close_all`], the idle reaper) kills what it
//! started ([`kill_descendants`]), then the shell itself, then the descendants again (one started
//! meanwhile), then closes the pseudo-console (which ends what is still attached to it and lets
//! the reader thread go): a program started from the terminal does not outlive it. The tree is
//! walked from the shell's process handle (held by its killer, so its id cannot be reused) and
//! only through children created after their parent, so a process whose parent id is a recycled
//! number of an older, unrelated process is never killed (`taskkill /T` does not check this).
//! No job object: a GUI program started from the terminal whose launcher has exited (`code .`,
//! `start notepad`) keeps running after the terminal closes, as in Windows Terminal; the cost is
//! that a crash of the app leaves a program started in a console of its own running (CP6.3 review).
//!
//! The environment: the app's own, minus `NO_COLOR` / `FORCE_COLOR` / `COLORFGBG`, plus `TERM`,
//! `COLORTERM`, `TERM_PROGRAM(_VERSION)` and `HERMES_DESKTOP_TERMINAL`. Nothing is added that the
//! user's own environment did not hold (the app never puts a provider key in its environment: keys
//! live in Settings and the keyring), so a terminal sees what a terminal opened beside the app sees.
//!
//! Hermes: `hermes_cli/web_host_terminal*.py` (`shell_spec`, `_MAX_SHELLS`, `RETENTION_SECONDS`),
//! `pty_session.py`. The page's socket protocol is `crates/desktop/src/terminal.rs`.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{Notify, broadcast};

/// The output kept for a reattach (the page caps a snapshot at 1 MiB).
const HISTORY_BYTES: usize = 1024 * 1024;
/// A shell nobody shows ends after this (`RETENTION_SECONDS`).
pub const RETENTION: Duration = Duration::from_secs(15 * 60);
/// At most this many shells (`_MAX_SHELLS`).
const MAX_SHELLS: usize = 16;

/// The last `HISTORY_BYTES` of output, and whether older output was dropped.
#[derive(Default)]
struct History {
    bytes: Vec<u8>,
    truncated: bool,
}

impl History {
    fn append(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        if self.bytes.len() > HISTORY_BYTES {
            let overflow = self.bytes.len() - HISTORY_BYTES;
            self.bytes.drain(..overflow);
            self.truncated = true;
        }
    }
}

/// One running shell.
pub struct Shell {
    id: String,
    name: String,
    cwd: String,
    /// The shell's process id (`None` when the platform did not say).
    pid: Option<u32>,
    /// The pseudo-console; `None` once the shell was ended (dropping it closes the console).
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    /// Keystrokes go to a writer thread (a blocked pipe must not stall the caller).
    input: std::sync::mpsc::Sender<Vec<u8>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Output, and the live feed: appended and sent under one lock, so an attach's snapshot
    /// and its subscription never miss or repeat a chunk.
    history: Mutex<History>,
    live: broadcast::Sender<Vec<u8>>,
    exited: AtomicBool,
    exit: Notify,
    /// Which attachment is current: a newer one supersedes the old.
    viewer: AtomicU64,
    /// When the last view detached (`None` while one is attached).
    idle_since: Mutex<Option<Instant>>,
}

/// A view attached to a shell: what it printed so far, and its output from then on.
pub struct Attachment {
    /// This attachment's number: it is current while [`Shell::is_current`] says so.
    pub viewer: u64,
    /// The output kept so far (the last 1 MiB).
    pub snapshot: Vec<u8>,
    /// Older output was dropped from the snapshot.
    pub truncated: bool,
    /// The output from the snapshot on, chunk by chunk.
    pub output: broadcast::Receiver<Vec<u8>>,
}

impl Shell {
    /// The shell's id (what a front end attaches by).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The program's name (`pwsh.exe`, `powershell.exe`, `cmd.exe`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The folder it started in.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// The shell's process id.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Whether the shell's process has ended.
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Resolves when the process ends (check [`Shell::has_exited`] first: a wake-up is not kept
    /// for a waiter that comes later).
    pub async fn exited(&self) {
        self.exit.notified().await
    }

    /// Attaches a view: the snapshot and the live output from the same instant; it supersedes
    /// any earlier view.
    pub fn attach(&self) -> Attachment {
        let viewer = self.viewer.fetch_add(1, Ordering::SeqCst) + 1;
        *self.idle_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        Attachment {
            viewer,
            snapshot: history.bytes.clone(),
            truncated: history.truncated,
            output: self.live.subscribe(),
        }
    }

    /// Whether `viewer` is still the current attachment.
    pub fn is_current(&self, viewer: u64) -> bool {
        self.viewer.load(Ordering::SeqCst) == viewer
    }

    /// The view `viewer` went away: the retention clock starts (unless a newer one is attached).
    pub fn detach(&self, viewer: u64) {
        if self.is_current(viewer) {
            *self.idle_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
    }

    /// Keystrokes (or pasted text) for the shell.
    pub fn write(&self, data: &[u8]) {
        let _ = self.input.send(data.to_vec());
    }

    /// Resizes the pseudo-console.
    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(master) = self.master.lock()
            && let Some(master) = master.as_ref()
        {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Ends the shell and everything it started: its descendants, the shell, its descendants again
    /// (started in between), then the pseudo-console. The killer's handle keeps the shell's
    /// process id from being reused, even after it exited.
    fn kill(&self) {
        if let Some(pid) = self.pid {
            kill_descendants(pid);
        }
        if let Ok(mut killer) = self.killer.lock() {
            let _ = killer.kill();
        }
        if let Some(pid) = self.pid {
            kill_descendants(pid);
        }
        let master = self.master.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(master);
    }
}

/// Kills every process descended from `root` (not `root` itself), which must be a process the
/// caller holds a handle to (so `root` names it and no other). A child counts only when it was
/// created after its parent: Windows keeps a dead parent's id in its children, and a process whose
/// parent id belonged to an older process that has since gone (the number now `root`'s, or a
/// descendant's) is not ours. Each process is opened before it is judged and killed through that
/// handle, so none can be swapped in between.
#[cfg(windows)]
pub(crate) fn kill_descendants(root: u32) {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        TerminateProcess,
    };

    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful open and is closed once, here.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    let open = |pid: u32| -> Option<Owned> {
        // SAFETY: a plain Win32 call; the handle is owned by the returned value.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid) }
            .ok()
            .map(Owned)
    };
    let created = |process: &Owned| -> Option<u64> {
        let mut times = [FILETIME::default(); 4];
        let [c, e, k, u] = &mut times;
        // SAFETY: `process` is open; the four out-pointers are live locals.
        unsafe { GetProcessTimes(process.0, c, e, k, u) }.ok()?;
        Some((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
    };
    // (pid, parent pid) of every process now.
    let mut table = Vec::new();
    // SAFETY: a snapshot handle, owned and closed by `Owned`; the entry's size is set as required.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return;
        };
        let snapshot = Owned(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot.0, &mut entry).is_ok() {
            loop {
                table.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snapshot.0, &mut entry).is_err() {
                    break;
                }
            }
        }
    }
    let Some(root_process) = open(root) else {
        return;
    };
    let Some(root_created) = created(&root_process) else {
        return;
    };
    let mut seen = std::collections::HashSet::from([root, 0]);
    let mut queue = vec![(root, root_created)];
    let mut found = Vec::new();
    while let Some((parent, parent_created)) = queue.pop() {
        for &(pid, ppid) in &table {
            if ppid != parent || seen.contains(&pid) {
                continue;
            }
            let Some(process) = open(pid) else { continue };
            let Some(child_created) = created(&process) else {
                continue;
            };
            if child_created < parent_created {
                continue; // Its parent id is a recycled number: not a child of ours.
            }
            seen.insert(pid);
            queue.push((pid, child_created));
            found.push(process);
        }
    }
    for process in &found {
        // SAFETY: `process` is open with PROCESS_TERMINATE.
        unsafe {
            let _ = TerminateProcess(process.0, 1);
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn kill_descendants(_root: u32) {}

/// Why a shell didn't start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// [`MAX_SHELLS`] are running.
    Capacity,
    /// The pseudo-console or the shell failed to start.
    Failed(String),
}

/// The running shells.
pub struct Terminals {
    shells: Mutex<HashMap<String, Arc<Shell>>>,
    /// The app's version, which a shell sees as `TERM_PROGRAM_VERSION`.
    version: &'static str,
}

/// The shell to run: PowerShell 7, Windows PowerShell, `COMSPEC`, else cmd (Hermes's
/// `shell_spec`): `(argv, name)`.
fn shell_spec() -> (Vec<String>, String) {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let windows_powershell =
        Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let ladder = [
        on_path("pwsh.exe"),
        Some(windows_powershell).filter(|p| p.is_file()),
        on_path("powershell.exe"),
        std::env::var("COMSPEC")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file()),
    ];
    let command = ladder
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| PathBuf::from("cmd.exe"));
    let name = command
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut argv = vec![command.display().to_string()];
    if name.starts_with("pwsh") || name.starts_with("powershell") {
        argv.push("-NoLogo".into());
    }
    (argv, name)
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// The folder a shell starts in: the one asked for (a file's folder for a file), else home.
fn safe_cwd(requested: Option<&str>) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"));
    let Some(requested) = requested.map(str::trim).filter(|r| !r.is_empty()) else {
        return home;
    };
    let candidate = crate::files::resolve(requested, None);
    if candidate.is_dir() {
        candidate
    } else if candidate.is_file() {
        candidate.parent().map_or(home, Path::to_path_buf)
    } else {
        home
    }
}

/// The last owner going away ends every shell still running (no shell outlives its `Terminals`).
impl Drop for Terminals {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl Terminals {
    /// No shells yet; `version` is the app's (a shell sees it as `TERM_PROGRAM_VERSION`).
    pub fn new(version: &'static str) -> Self {
        Self {
            shells: Mutex::default(),
            version,
        }
    }

    /// Ends shells nobody has looked at within the retention window, and ended ones.
    fn reap_idle(&self) {
        let mut shells = self.shells.lock().unwrap_or_else(|e| e.into_inner());
        shells.retain(|_, shell| {
            let stale = shell
                .idle_since
                .lock()
                .map(|since| since.is_some_and(|t| t.elapsed() > RETENTION))
                .unwrap_or(false);
            let gone = stale || shell.exited.load(Ordering::SeqCst);
            if gone {
                shell.kill();
            }
            !gone
        });
    }

    /// Starts a shell of `cols` × `rows` in `cwd` (a page path; home when missing). Blocking.
    pub fn spawn(&self, cols: u16, rows: u16, cwd: Option<&str>) -> Result<Arc<Shell>, SpawnError> {
        self.reap_idle();
        if self.shells.lock().map(|s| s.len()).unwrap_or(0) >= MAX_SHELLS {
            return Err(SpawnError::Capacity);
        }
        let failed = SpawnError::Failed;
        let pty = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| failed(format!("Could not open a terminal: {e}")))?;
        let (argv, name) = shell_spec();
        let cwd = safe_cwd(cwd);
        let mut command = CommandBuilder::new(&argv[0]);
        command.args(&argv[1..]);
        command.cwd(&cwd);
        for key in ["NO_COLOR", "FORCE_COLOR", "COLORFGBG"] {
            command.env_remove(key);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "Hermes");
        command.env("TERM_PROGRAM_VERSION", self.version);
        command.env("HERMES_DESKTOP_TERMINAL", "1");
        let mut child = pty
            .slave
            .spawn_command(command)
            .map_err(|e| failed(format!("Could not start {name}: {e}")))?;
        drop(pty.slave);
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(|e| failed(format!("Could not read the terminal: {e}")))?;
        let mut writer = pty
            .master
            .take_writer()
            .map_err(|e| failed(format!("Could not write to the terminal: {e}")))?;
        let (input, keystrokes) = std::sync::mpsc::channel::<Vec<u8>>();
        let shell = Arc::new(Shell {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name,
            cwd: cwd.display().to_string(),
            pid: child.process_id(),
            master: Mutex::new(Some(pty.master)),
            input,
            killer: Mutex::new(child.clone_killer()),
            history: Mutex::new(History::default()),
            live: broadcast::channel(1024).0,
            exited: AtomicBool::new(false),
            exit: Notify::new(),
            viewer: AtomicU64::new(0),
            idle_since: Mutex::new(None),
        });
        std::thread::spawn(move || {
            for data in keystrokes {
                if writer
                    .write_all(&data)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        let output = shell.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 16 * 1024];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let mut history = output.history.lock().unwrap_or_else(|e| e.into_inner());
                history.append(&buffer[..n]);
                let _ = output.live.send(buffer[..n].to_vec());
            }
        });
        // (The pseudo-console keeps its output pipe open after the shell exits: the exit is
        // the process's own.)
        let waiter = shell.clone();
        std::thread::spawn(move || {
            let _ = child.wait();
            waiter.exited.store(true, Ordering::SeqCst);
            waiter.exit.notify_waiters();
            waiter.exit.notify_one();
        });
        self.shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(shell.id.clone(), shell.clone());
        Ok(shell)
    }

    /// The running shell with this id.
    pub fn get(&self, id: &str) -> Option<Arc<Shell>> {
        self.shells.lock().ok().and_then(|s| s.get(id).cloned())
    }

    /// Ends a shell and forgets it.
    pub fn remove(&self, id: &str) {
        let shell = self
            .shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        // (Killed outside the lock: walking the process table takes a moment.)
        if let Some(shell) = shell {
            shell.kill();
        }
    }

    /// Ends every shell and what each started (the app is closing).
    pub fn close_all(&self) {
        let shells: Vec<_> = self
            .shells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, shell)| shell)
            .collect();
        for shell in shells {
            shell.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_the_newest_output() {
        let mut history = History::default();
        history.append(&vec![b'a'; HISTORY_BYTES]);
        assert!(!history.truncated);
        history.append(b"tail");
        assert!(history.truncated);
        assert_eq!(history.bytes.len(), HISTORY_BYTES);
        assert!(history.bytes.ends_with(b"tail"));
    }

    #[test]
    fn the_shell_is_powershell_or_cmd() {
        let (argv, name) = shell_spec();
        assert!(
            Path::new(&argv[0]).is_file() || argv[0] == "cmd.exe",
            "{argv:?}"
        );
        assert!(
            name.starts_with("pwsh") || name.starts_with("powershell") || name.starts_with("cmd")
        );
        assert_eq!(
            safe_cwd(Some("C:/no/such/folder")),
            dirs::home_dir().unwrap()
        );
    }

    fn output(shell: &Shell) -> String {
        String::from_utf8_lossy(&shell.history.lock().unwrap().bytes).to_string()
    }

    fn wait_until(done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_shell_runs_echoes_and_exits() {
        let terminals = Terminals::new("test");
        let shell = terminals.spawn(80, 24, None).expect("a shell starts");
        assert!(terminals.get(shell.id()).is_some());
        // The pseudo-console asks where the cursor is (`ESC[6n`) and waits for the answer, which
        // the page's xterm.js gives; answer as it would.
        wait_until(|| output(&shell).contains("\x1b[6n"));
        let first = shell.attach();
        shell.write(b"\x1b[1;1R");
        shell.write(b"echo hermes-terminal-check\r");
        wait_until(|| output(&shell).contains("hermes-terminal-check\r\n"));
        assert!(output(&shell).contains("hermes-terminal-check"));
        // The attachment's stream carried the new output; a second view supersedes the first.
        let mut streamed = first.output;
        assert!(streamed.try_recv().is_ok());
        let second = shell.attach();
        assert!(!shell.is_current(first.viewer) && shell.is_current(second.viewer));
        assert!(!second.snapshot.is_empty());
        shell.resize(100, 30);
        shell.write(b"exit\r");
        wait_until(|| shell.has_exited());
        assert!(shell.has_exited(), "exit ends the shell");
        terminals.close_all();
        assert!(terminals.get(shell.id()).is_none());
    }

    /// Whether process `pid` is running (`tasklist`, from System32).
    pub(crate) fn process_alive(pid: u32) -> bool {
        use std::os::windows::process::CommandExt as _;
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let out = std::process::Command::new(Path::new(&system).join(r"System32\tasklist.exe"))
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .creation_flags(crate::git::CREATE_NO_WINDOW)
            .output()
            .expect("tasklist runs");
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    }

    /// Ending a shell ends what it started: a `ping` that would run for a minute is gone with it,
    /// and so is the shell (closing a terminal, and the app's quit, both go through `kill`). The
    /// `ping` runs in a console of its own (hidden), so closing the pseudo-console alone would not
    /// end it: only the tree kill does.
    #[test]
    fn ending_a_shell_ends_the_programs_it_started() {
        let terminals = Terminals::new("test");
        let shell = terminals.spawn(80, 24, None).expect("a shell starts");
        if shell.name().starts_with("cmd") {
            terminals.close_all();
            return; // The command below is PowerShell's.
        }
        wait_until(|| output(&shell).contains("\x1b[6n"));
        shell.write(b"\x1b[1;1R");
        let ping = r#"$p = Start-Process -PassThru -WindowStyle Hidden -FilePath "$env:SystemRoot\System32\PING.EXE" -ArgumentList '-n','60','127.0.0.1'; "CHILD=$($p.Id)=""#;
        shell.write(format!("{ping}\r").as_bytes());
        wait_until(|| child_pid(&output(&shell)).is_some());
        let child = child_pid(&output(&shell)).expect("the child's pid is printed");
        let parent = shell.pid().expect("the shell's pid");
        assert!(process_alive(child) && process_alive(parent));
        terminals.remove(shell.id());
        wait_until(|| !process_alive(child) && !process_alive(parent));
        assert!(
            !process_alive(child),
            "the child {child} outlived its terminal"
        );
        assert!(
            !process_alive(parent),
            "the shell {parent} outlived its terminal"
        );
        // A second end is harmless.
        terminals.close_all();
    }

    fn child_pid(text: &str) -> Option<u32> {
        let at = text.rfind("CHILD=")? + "CHILD=".len();
        let rest = &text[at..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (rest[digits.len()..].starts_with('=') && !digits.is_empty())
            .then(|| digits.parse().ok())
            .flatten()
    }
}
