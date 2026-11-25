//! The terminal over the real core: a real shell (the core's choice: PowerShell here) started
//! through the backend seam in a temporary thread folder, its output fed by the pump into the
//! emulator. A command and its output, the folder it starts in, a resize the shell sees
//! (`$Host.UI.RawUI.WindowSize`), a full-screen program (`less` from Git for Windows when it is
//! installed), and the end of the shell and of a program it started, on close and on quit. Only
//! harmless commands run, in `hermes_testdir` folders; every process started is checked gone.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use hermes_core::Core;
use hermes_runtime::RuntimeConfig;
use serde_json::json;

use super::colors::TermColors;
use super::emulator::Emulator;
use super::session::{Pty, Shared, pump};
use crate::model::{Backend as _, CoreBackend};
use crate::theme::ThemeDef;

struct World {
    backend: Arc<CoreBackend>,
    session: String,
    _core: Arc<Core>,
    _runtime: tokio::runtime::Runtime,
    _data: hermes_testdir::TestDir,
}

fn world(cwd: &Path) -> World {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let data = hermes_testdir::tempdir().unwrap();
    std::fs::write(
        data.path().join("config.json"),
        json!({
            "providers": [{"id": "mock", "name": "Mock", "base_url": "http://127.0.0.1:9/v1",
                           "models": ["mock"], "discover_models": false}],
            "provider": "mock", "model": "mock"
        })
        .to_string(),
    )
    .unwrap();
    let config = RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..RuntimeConfig::for_current_user().unwrap()
    };
    let core = runtime
        .block_on(async { Core::new(config, tokio::runtime::Handle::current()) })
        .unwrap();
    let (record, _) = core
        .create_session(hermes_core::NewSession {
            provider: hermes_runtime::ProviderConfig {
                id: "mock".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: Some("key".into()),
            },
            model: "mock".into(),
            cwd: cwd.to_path_buf(),
            title: Some("Terminal".into()),
            seeded: vec![],
            parent: None,
        })
        .unwrap();
    World {
        backend: Arc::new(CoreBackend::new(core.clone())),
        session: record.id,
        _core: core,
        _runtime: runtime,
        _data: data,
    }
}

/// A started terminal: the shell and the emulator its pump feeds.
struct Term {
    pty: Arc<dyn Pty>,
    shared: Arc<Shared>,
    pumping: Option<std::thread::JoinHandle<()>>,
}

impl Term {
    fn start(world: &World, cols: u16, rows: u16) -> Self {
        let pty = world
            .backend
            .terminal_spawn(&world.session, cols, rows)
            .expect("a shell starts");
        let shared = Shared::new(Emulator::new(
            usize::from(cols),
            usize::from(rows),
            TermColors::of(&ThemeDef::default_dark()),
        ));
        let pumping = {
            let (shared, pty) = (shared.clone(), pty.clone());
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap()
                    .block_on(pump(shared, pty))
            })
        };
        Self {
            pty,
            shared,
            pumping: Some(pumping),
        }
    }

    fn text(&self) -> String {
        self.shared.emulator.lock().text()
    }

    fn mode(&self) -> TermMode {
        self.shared.emulator.lock().mode()
    }

    /// Waits up to 90 s (a loaded machine) for `done`; the screen in the panic message otherwise.
    fn wait(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(90);
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "waited for {what}; the screen:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn run(&self, command: &str) {
        self.pty.write(format!("{command}\r").as_bytes());
    }

    /// Waits until the shell reads what is typed: a marker command is sent until its output (not
    /// its echo: the output is computed) shows. A program that just quit (`less`) may still have
    /// read the first ones; no timing is guessed.
    fn sync(&self) {
        let deadline = Instant::now() + Duration::from_secs(90);
        for n in 1.. {
            let output = format!("SYNC-{}-OK", n * 7);
            self.run(&format!("Write-Output ('SYNC-' + ({n}*7) + '-OK')"));
            let tried = Instant::now();
            while tried.elapsed() < Duration::from_secs(5) {
                if self.text().contains(&output) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(
                Instant::now() < deadline,
                "the shell never answered; the screen:\n{}",
                self.text()
            );
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.shared
            .emulator
            .lock()
            .resize(usize::from(cols), usize::from(rows));
        self.pty.resize(cols, rows);
    }

    fn close(mut self) {
        self.pty.close();
        if let Some(pumping) = self.pumping.take() {
            pumping.join().unwrap();
        }
    }
}

impl Drop for Term {
    /// A failed test still ends its shell (and lets its folder go).
    fn drop(&mut self) {
        self.pty.close();
    }
}

/// Whether process `pid` runs (`tasklist`).
fn alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt as _;
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let out = std::process::Command::new(Path::new(&system).join(r"System32\tasklist.exe"))
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .creation_flags(0x0800_0000)
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

fn gone_within(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while alive(pid) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

fn is_powershell(term: &Term) -> bool {
    let name = term.pty.name();
    name.starts_with("pwsh") || name.starts_with("powershell")
}

/// A hidden `ping` of a minute started from the shell (a console of its own: only the tree kill
/// ends it), and its pid.
fn start_child(term: &Term, tag: &str) -> u32 {
    term.run(&format!(
        r#"$p = Start-Process -PassThru -WindowStyle Hidden -FilePath "$env:SystemRoot\System32\PING.EXE" -ArgumentList '-n','60','127.0.0.1'; "{tag}=$($p.Id)=""#
    ));
    let marker = format!("{tag}=");
    let pid = |t: &Term| {
        let text = t.text().replace('\n', "");
        text.rmatch_indices(&marker).find_map(|(at, _)| {
            let rest = &text[at + marker.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty() && rest[digits.len()..].starts_with('='))
                .then(|| digits.parse::<u32>().ok())
                .flatten()
        })
    };
    term.wait("the child's pid", |t| pid(t).is_some());
    pid(term).unwrap()
}

#[test]
fn a_real_shell_runs_a_command_resizes_runs_less_and_ends_with_its_children() {
    let folder = hermes_testdir::tempdir().unwrap();
    std::fs::write(
        folder.path().join("long.txt"),
        (1..=300).map(|i| format!("row {i}\n")).collect::<String>(),
    )
    .unwrap();
    let world = world(folder.path());
    let term = Term::start(&world, 100, 30);
    // The pseudo-console's first question was answered by the emulator, so a prompt comes.
    term.wait("a prompt", |t| t.text().contains('>'));
    if !is_powershell(&term) {
        term.close();
        return; // The commands below are PowerShell's (the core prefers it; cmd is the fallback).
    }
    // A command and its output, in the thread's folder.
    term.run("Write-Output ('hermes-' + (6*7)); (Get-Location).Path");
    term.wait("the output", |t| t.text().contains("hermes-42"));
    let name = folder
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    term.wait("the folder", |t| t.text().contains(&name));
    // A resize the shell sees.
    term.resize(72, 20);
    term.run("Write-Output ('width=' + $Host.UI.RawUI.WindowSize.Width)");
    term.wait("the new width", |t| t.text().contains("width=72"));
    // A full-screen program: `less` from Git for Windows, if this machine has it.
    let less = Path::new(r"C:\Program Files\Git\usr\bin\less.exe");
    if less.is_file() {
        term.run(&format!("& '{}' long.txt", less.display()));
        term.wait("less on the alternate screen", |t| {
            t.mode().contains(TermMode::ALT_SCREEN) && t.text().contains("row 19")
        });
        // The first page, on the alternate screen: the shell's lines are not on it.
        let page = term.text();
        assert!(page.lines().any(|l| l.trim() == "row 1"), "{page}");
        assert!(!page.contains("row 25") && !page.contains("hermes-42"), "{page}");
        // A page down, then quit: the shell's screen comes back.
        term.pty.write(b" ");
        // (The second page replaces the first: `row 1` scrolls away.)
        term.wait("the next page", |t| {
            let text = t.text();
            text.contains("row 30") && !text.lines().any(|l| l.trim() == "row 1")
        });
        term.pty.write(b"q");
        term.wait("less gone", |t| !t.mode().contains(TermMode::ALT_SCREEN));
        term.wait("the shell's screen back", |t| {
            t.text().contains("width=72") && !t.text().contains("row 30")
        });
        // Until `less` has exited, what is typed may still be read by it.
        term.sync();
        println!("less: alternate screen entered and left");
    } else {
        println!("less: not installed here (Git for Windows' usr/bin); skipped");
    }
    // Closing ends the shell and what it started.
    let child = start_child(&term, "CHILD");
    let shell = term.pty.pid().expect("the shell's pid");
    assert!(alive(child) && alive(shell));
    term.close();
    assert!(
        gone_within(child, Duration::from_secs(10)),
        "child {child} outlived its terminal"
    );
    assert!(
        gone_within(shell, Duration::from_secs(10)),
        "shell {shell} outlived its terminal"
    );
}

#[test]
fn quitting_ends_every_shell_and_its_children() {
    let folder = hermes_testdir::tempdir().unwrap();
    let world = world(folder.path());
    let terms = [Term::start(&world, 80, 24), Term::start(&world, 80, 24)];
    for term in &terms {
        term.wait("a prompt", |t| t.text().contains('>'));
    }
    if !terms.iter().all(is_powershell) {
        world.backend.terminals_close_all();
        return;
    }
    let mut pids = Vec::new();
    for (ix, term) in terms.iter().enumerate() {
        pids.push(start_child(term, &format!("KID{ix}")));
        pids.push(term.pty.pid().unwrap());
    }
    assert!(pids.iter().all(|p| alive(*p)));
    // What the app does when its window closes.
    world.backend.terminals_close_all();
    for pid in &pids {
        assert!(
            gone_within(*pid, Duration::from_secs(10)),
            "{pid} outlived the app's quit"
        );
    }
    for mut term in terms {
        if let Some(p) = term.pumping.take() {
            p.join().unwrap();
        }
    }
}
