//! Tests of the shell sandbox (`sandbox.rs`). The ones that start processes set the sandbox
//! level on their own shell (`set_sandbox_override`), never the process-wide setting, so the
//! tests running beside them keep `off`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::shell::{ShellKind, ShellSession};
use crate::tests::TestBridge;
use crate::{SessionTools, SessionToolsConfig};

/// A session whose terminal is PowerShell whatever this machine has (Git Bash doesn't start at
/// Low; see `a_low_git_bash_writes_nothing_to_the_user_profile`).
fn powershell_tools() -> (hermes_testdir::TestDir, SessionTools) {
    let dir = hermes_testdir::tempdir().unwrap();
    let tools = SessionTools::new(SessionToolsConfig {
        session_id: "ses_test".into(),
        cwd: dir.path().to_path_buf(),
        home: dir.path().to_path_buf(),
        data_dir: dir.path().join(".data"),
        scratch_dir: dir.path().join(".scratch"),
        shell: ShellKind::PowerShell,
        bridge: std::sync::Arc::new(TestBridge::default()),
        todos: Vec::new(),
        memory: None,
        history: None,
        skills: None,
        approvals: None,
        checkpoints: None,
        web: None,
    });
    (dir, tools)
}

/// `whoami /groups` lists the token's label by SID: Low is `S-1-16-4096`, Medium
/// `S-1-16-8192`, High (an elevated app) `S-1-16-12288`. The SIDs don't depend on the
/// language Windows is in; the label's name does.
const LOW_SID: &str = "S-1-16-4096";
const MEDIUM_SID: &str = "S-1-16-8192";
const HIGH_SID: &str = "S-1-16-12288";

/// Windows' own `whoami.exe`, by path: a bare `whoami` finds Git's MSYS one first when Git's
/// `usr\bin` is on PATH (it takes no `/groups`).
const WHOAMI_PS: &str = "& \"$env:SystemRoot\\System32\\whoami.exe\" /groups";
const WHOAMI_BASH: &str = "\"$SYSTEMROOT/System32/whoami.exe\" /groups";

/// What every MSYS program (Git Bash and its `usr\bin` tools) prints when it starts at Low:
/// the MSYS runtime can't create its object directory under `\BaseNamedObjects`.
const MSYS_AT_LOW: &str = "NtCreateDirectoryObject";

fn powershell_shell(dir: &Path, level: Sandbox) -> ShellSession {
    let shell = ShellSession::new(
        ShellKind::PowerShell,
        dir.to_path_buf(),
        dir.join("scratch"),
    );
    shell.set_sandbox_override(Some(level));
    shell
}

async fn run(shell: &ShellSession, command: &str) -> crate::shell::RunResult {
    shell
        .run(command, None, Duration::from_secs(60), None, &|_| {})
        .await
}

/// A file in the real `%USERPROFILE%` that no one else would make: the sandboxed shell tries to
/// write it, and it must not be there afterwards. Removed if it is (the test then fails).
struct Probe(PathBuf);

impl Probe {
    fn new() -> Self {
        let profile = std::env::var_os("USERPROFILE").expect("USERPROFILE is set");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("hermes-sandbox-probe-{}-{nanos}.txt", std::process::id());
        Self(PathBuf::from(profile).join(name))
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Labels a test folder Low (inherited by what is made in it), as CP15.2 will label the
/// workspace. Test-only, through `icacls`; `false` when that isn't possible here.
fn label_low(dir: &Path) -> bool {
    use std::os::windows::process::CommandExt as _;
    std::process::Command::new("icacls")
        .arg(dir)
        .args(["/setintegritylevel", "(OI)(CI)low"])
        .creation_flags(0x0800_0000)
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn the_config_value_reads_off_and_low() {
    assert_eq!(Sandbox::from_config(&json!("low")), Sandbox::Low);
    assert_eq!(Sandbox::from_config(&json!(" LOW ")), Sandbox::Low);
    assert_eq!(Sandbox::from_config(&json!("off")), Sandbox::Off);
    assert_eq!(Sandbox::from_config(&json!("")), Sandbox::Off);
    assert_eq!(Sandbox::from_config(&serde_json::Value::Null), Sandbox::Off);
    // Unknown values are `off` (with a warning), never a level nobody asked for.
    assert_eq!(Sandbox::from_config(&json!("high")), Sandbox::Off);
    assert_eq!(Sandbox::from_config(&json!(true)), Sandbox::Off);
    assert_eq!(Sandbox::default(), Sandbox::Off);
}

#[test]
fn the_setting_defaults_to_off() {
    // Nothing in the test process sets it: every other test runs `off` through it.
    assert_eq!(sandbox(), Sandbox::Off);
}

#[test]
fn command_lines_are_quoted_as_std_quotes_them() {
    let line = |program: &str, args: &[&str]| {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let wide = command_line(OsStr::new(program), args.iter().map(OsString::as_os_str)).unwrap();
        String::from_utf16(&wide[..wide.len() - 1]).unwrap()
    };
    assert_eq!(
        line(
            "C:\\Program Files\\Git\\bin\\bash.exe",
            &["--noprofile", "--norc"]
        ),
        "\"C:\\Program Files\\Git\\bin\\bash.exe\" --noprofile --norc"
    );
    assert_eq!(
        line("a.exe", &["", "two words", "tab\there"]),
        "\"a.exe\" \"\" \"two words\" \"tab\there\""
    );
    // Backslashes are doubled only before a quote (inside, or the closing one).
    assert_eq!(
        line("a.exe", &["say \"hi\""]),
        "\"a.exe\" \"say \\\"hi\\\"\""
    );
    assert_eq!(line("a.exe", &["C:\\dir\\"]), "\"a.exe\" C:\\dir\\");
    assert_eq!(
        line("a.exe", &["C:\\my dir\\"]),
        "\"a.exe\" \"C:\\my dir\\\\\""
    );
    assert_eq!(line("a.exe", &["a\\\\\"b"]), "\"a.exe\" a\\\\\\\\\\\"b");
    // What CreateProcess can't take is refused, as std refuses it.
    assert!(command_line(OsStr::new("a.exe"), [OsStr::new("x\0y")].into_iter()).is_err());
    assert!(command_line(OsStr::new("a\"b.exe"), std::iter::empty()).is_err());
}

#[test]
fn the_environment_block_is_sorted_and_leaves_removed_variables_out() {
    let vars = [
        (OsStr::new("b"), Some(OsStr::new("2"))),
        (OsStr::new("Path"), Some(OsStr::new("C:\\x"))),
        (OsStr::new("A"), Some(OsStr::new("1"))),
        (OsStr::new("BASH_ENV"), None),
    ];
    let block = environment_block(vars.into_iter()).unwrap();
    let text = String::from_utf16(&block).unwrap();
    assert_eq!(text, "A=1\0b=2\0Path=C:\\x\0\0");
    assert_eq!(environment_block(std::iter::empty()).unwrap(), [0, 0]);
    assert!(environment_block([(OsStr::new("A=B"), Some(OsStr::new("1")))].into_iter()).is_err());
}

#[test]
fn programs_are_found_on_the_childs_path_then_in_system32() {
    let dir = hermes_testdir::tempdir().unwrap();
    std::fs::write(dir.path().join("tool.exe"), b"").unwrap();
    // An extensionless script beside it (as in Git's `usr\bin`) is not the program, as in std.
    std::fs::write(dir.path().join("tool"), b"#!/bin/sh").unwrap();
    let found = resolve_program(OsStr::new("tool"), Some(dir.path().as_os_str())).unwrap();
    assert_eq!(found, dir.path().join("tool.exe"));
    assert_eq!(
        resolve_program(dir.path().join("tool").as_os_str(), None).unwrap(),
        dir.path().join("tool.exe")
    );
    std::fs::write(dir.path().join("script-only"), b"").unwrap();
    assert!(resolve_program(OsStr::new("script-only"), Some(dir.path().as_os_str())).is_err());
    // Not on PATH: System32 (powershell.exe's folder is under it, cmd.exe is in it).
    let cmd = resolve_program(OsStr::new("cmd.exe"), Some(OsStr::new(""))).unwrap();
    assert!(cmd.ends_with("cmd.exe"), "{}", cmd.display());
    let absolute = dir.path().join("tool.exe");
    assert_eq!(
        resolve_program(absolute.as_os_str(), None).unwrap(),
        absolute
    );
    assert!(
        resolve_program(
            OsStr::new("no-such-program-here"),
            Some(dir.path().as_os_str())
        )
        .is_err()
    );
}

#[tokio::test]
async fn a_low_shell_runs_with_the_low_integrity_label() {
    let dir = hermes_testdir::tempdir().unwrap();
    let shell = powershell_shell(dir.path(), Sandbox::Low);
    let result = run(&shell, WHOAMI_PS).await;
    assert_eq!(result.exit_code, Some(0), "{}", result.output);
    assert!(result.output.contains(LOW_SID), "{}", result.output);
    assert!(!result.output.contains(MEDIUM_SID), "{}", result.output);
}

#[tokio::test]
async fn an_off_shell_runs_at_the_apps_own_level() {
    let dir = hermes_testdir::tempdir().unwrap();
    let shell = powershell_shell(dir.path(), Sandbox::Off);
    let result = run(&shell, WHOAMI_PS).await;
    assert_eq!(result.exit_code, Some(0), "{}", result.output);
    assert!(
        result.output.contains(MEDIUM_SID) || result.output.contains(HIGH_SID),
        "{}",
        result.output
    );
    assert!(!result.output.contains(LOW_SID), "{}", result.output);
}

#[tokio::test]
async fn a_low_shell_cannot_write_to_the_user_profile_but_can_write_where_low_may() {
    let dir = hermes_testdir::tempdir().unwrap();
    let allowed = dir.path().join("low");
    std::fs::create_dir(&allowed).unwrap();
    let labelled = label_low(&allowed);
    let probe = Probe::new();
    let shell = powershell_shell(dir.path(), Sandbox::Low);
    let command = format!(
        "try {{ Set-Content -LiteralPath {} -Value x -ErrorAction Stop; 'profile=written' }} catch {{ 'profile=denied' }}\n\
         try {{ Set-Content -LiteralPath {} -Value x -ErrorAction Stop; 'low=written' }} catch {{ 'low=denied' }}",
        crate::shell::powershell_quote(&probe.0.display().to_string()),
        crate::shell::powershell_quote(&allowed.join("ok.txt").display().to_string()),
    );
    let result = run(&shell, &command).await;
    let exists = probe.0.exists();
    drop(probe);
    assert!(
        !exists,
        "the sandboxed shell wrote to %USERPROFILE%: {}",
        result.output
    );
    assert!(
        result.output.contains("profile=denied"),
        "{}",
        result.output
    );
    // The shell did run, and could write: the denial is the label, not a broken process.
    if labelled {
        assert!(result.output.contains("low=written"), "{}", result.output);
        assert!(allowed.join("ok.txt").is_file());
    } else {
        eprintln!("icacls could not label a test folder Low; only the denial was checked");
    }
}

fn git_bash_shell(dir: &Path) -> Option<ShellSession> {
    let ShellKind::GitBash(bash) = crate::shell::detect_shell() else {
        return None; // no Git Bash on this machine
    };
    Some(ShellSession::new(
        ShellKind::GitBash(bash),
        dir.to_path_buf(),
        dir.join("scratch"),
    ))
}

/// The persistent bash ended before running the command: what a Git Bash at Low does (its
/// runtime's error goes to bash's stderr, which the terminal discards); the terminal then says
/// why (`MSYS_AT_LOW_ERROR`).
fn bash_died_at_start(result: &crate::shell::RunResult) -> bool {
    result.exit_code.is_none()
        && result.shell_restarted
        && result.output == crate::shell::MSYS_AT_LOW_ERROR
}

/// Git Bash does not start at Low (CP15.1 finding, see the progress note): the MSYS runtime
/// dies creating `\BaseNamedObjects\msys-2.0S5-<hash>`. This pins what happens today: the
/// command doesn't run, nothing is written, the model gets `MSYS_AT_LOW_ERROR` with the "shell
/// exited" note, and the next call tries again. Should Git Bash ever start at Low, the test checks the label and
/// the denial instead.
#[tokio::test]
async fn a_low_git_bash_writes_nothing_to_the_user_profile() {
    let dir = hermes_testdir::tempdir().unwrap();
    let Some(shell) = git_bash_shell(dir.path()) else {
        return;
    };
    shell.set_sandbox_override(Some(Sandbox::Low));
    let probe = Probe::new();
    let command = format!(
        "echo x > '{}' && echo profile=written || echo profile=denied; {WHOAMI_BASH}",
        crate::display_path(&probe.0)
    );
    let result = run(&shell, &command).await;
    let exists = probe.0.exists();
    drop(probe);
    shell.shutdown().await;
    assert!(
        !exists,
        "the sandboxed bash wrote to %USERPROFILE%: {}",
        result.output
    );
    if bash_died_at_start(&result) {
        eprintln!("Git Bash does not start at Low: {result:?}");
        return;
    }
    assert!(
        result.output.contains("profile=denied"),
        "{}",
        result.output
    );
    assert!(result.output.contains(LOW_SID), "{}", result.output);
}

#[tokio::test]
async fn changing_the_level_restarts_a_running_git_bash() {
    let dir = hermes_testdir::tempdir().unwrap();
    let Some(shell) = git_bash_shell(dir.path()) else {
        return;
    };
    shell.set_sandbox_override(Some(Sandbox::Off));
    let off = run(&shell, WHOAMI_BASH).await;
    assert_eq!(off.exit_code, Some(0), "{off:?}");
    assert!(!off.output.contains(LOW_SID), "{}", off.output);
    shell.set_sandbox_override(Some(Sandbox::Low));
    let low = run(&shell, WHOAMI_BASH).await;
    // A new shell started at Low (or failed to: see the test above); the Medium one is gone.
    assert!(!low.output.contains(MEDIUM_SID), "{low:?}");
    assert!(
        low.output.contains(LOW_SID) || bash_died_at_start(&low),
        "{low:?}"
    );
    shell.set_sandbox_override(Some(Sandbox::Off));
    let back = run(&shell, WHOAMI_BASH).await;
    shell.shutdown().await;
    assert_eq!(back.exit_code, Some(0), "{back:?}");
    assert!(!back.output.contains(LOW_SID), "{}", back.output);
}

/// MSYS programs started from a Low PowerShell fail the same way (Git's `usr\bin` tools on
/// PATH: `ls`, `sed`, MSYS `whoami`...); native programs (`git.exe`, `cargo`, `whoami.exe`)
/// run. Pinned so CP15.2 sees it.
#[tokio::test]
async fn msys_programs_do_not_start_at_low() {
    let ShellKind::GitBash(bash) = crate::shell::detect_shell() else {
        return;
    };
    let Some(usr_bin) = bash
        .parent()
        .and_then(Path::parent)
        .map(|git| git.join("usr").join("bin"))
    else {
        return;
    };
    let msys_echo = usr_bin.join("echo.exe");
    if !msys_echo.is_file() {
        return;
    }
    let dir = hermes_testdir::tempdir().unwrap();
    let shell = powershell_shell(dir.path(), Sandbox::Low);
    let command = format!(
        "& {} hello; & \"$env:SystemRoot\\System32\\cmd.exe\" /c echo native",
        crate::shell::powershell_quote(&msys_echo.display().to_string())
    );
    let result = run(&shell, &command).await;
    assert!(result.output.contains("native"), "{}", result.output);
    assert!(result.output.contains(MSYS_AT_LOW), "{}", result.output);
}

#[tokio::test]
async fn a_low_command_is_killed_on_timeout_and_on_stop() {
    let dir = hermes_testdir::tempdir().unwrap();
    let shell = powershell_shell(dir.path(), Sandbox::Low);
    let started = Instant::now();
    let result = shell
        .run(
            "Start-Sleep -Seconds 60",
            None,
            Duration::from_secs(2),
            None,
            &|_| {},
        )
        .await;
    assert!(result.timed_out, "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(20));

    let cancel = tokio_util::sync::CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        stopper.cancel();
    });
    let started = Instant::now();
    let result = shell
        .run(
            "'before'; Start-Sleep -Seconds 60",
            None,
            Duration::from_secs(60),
            Some(&cancel),
            &|_| {},
        )
        .await;
    assert!(result.interrupted, "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
async fn background_processes_and_scripts_run_sandboxed_too() {
    let (_dir, tools) = powershell_tools();
    tools.shell.set_sandbox_override(Some(Sandbox::Low));
    let started = crate::shell::terminal(
        &tools,
        json!({"command": WHOAMI_PS, "background": true}),
        None,
    )
    .await;
    let id = started.value["session_id"].as_str().unwrap().to_string();
    let waited = crate::process::process_manage(
        &tools,
        json!({"action": "wait", "session_id": id, "timeout": 60}),
    )
    .await;
    assert_eq!(waited.value["exit_code"], 0, "{:?}", waited.value);
    let output = waited.value["new_output"].as_str().unwrap_or_default();
    assert!(output.contains(LOW_SID), "{output}");

    let script = crate::script::run_script(
        &tools,
        json!({"language": "powershell", "code": WHOAMI_PS}),
        None,
    )
    .await;
    assert_eq!(script.value["exit_code"], 0, "{:?}", script.value);
    let output = script.value["output"].as_str().unwrap_or_default();
    assert!(output.contains(LOW_SID), "{output}");
    tools.shutdown().await;
}

/// The handle count of this process after many sandboxed spawns doesn't grow with them. Runs
/// in a child test process (the next test), where nothing else opens handles meanwhile.
#[test]
fn sandboxed_spawns_leak_no_handles() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "sandbox::tests::handle_count_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("1 passed"), "{text}");
}

#[test]
#[ignore = "run by sandboxed_spawns_leak_no_handles in its own process"]
fn handle_count_child() {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    fn handles() -> u32 {
        let mut count = 0u32;
        // SAFETY: the current-process pseudo-handle and a valid out-pointer.
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }.unwrap();
        count
    }
    const RUNS: usize = 30;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = hermes_testdir::tempdir().unwrap();
    let shell = powershell_shell(dir.path(), Sandbox::Low);
    let once = |n: usize| {
        let result = runtime.block_on(run(&shell, &format!("'run {n}'")));
        assert_eq!(result.output, format!("run {n}"), "{result:?}");
    };
    // Warm up: the blocking pool's threads, tokio's and Windows' own lazily made handles.
    for n in 0..5 {
        once(n);
    }
    let before = handles();
    for n in 0..RUNS {
        once(n);
    }
    let after = handles();
    eprintln!("handles before {before}, after {RUNS} sandboxed runs {after}");
    // Each run opens about ten handles (token copies, three pipes, the null device, process,
    // thread, job); one of them leaking would add RUNS. The slack is for pool threads.
    assert!(
        after <= before + 12,
        "handles grew from {before} to {after} over {RUNS} runs"
    );

    // A piped stdin (the background-process path), the child ending when it reads EOF.
    let piped = |n: usize| {
        runtime.block_on(async {
            let mut builder = crate::shell::command_for_powershell(dir.path(), dir.path());
            builder.args(["-NoProfile", "-Command", "$input | Out-Null"]);
            let spec = Spec {
                stdin_piped: true,
                kill_on_drop: true,
            };
            let mut spawned = spawn(&mut builder, spec, Sandbox::Low).unwrap();
            drop(spawned.stdin.take().expect("piped stdin"));
            let status = spawned.child.wait().await.unwrap();
            assert!(status.success(), "run {n}: {status:?}");
        });
    };
    piped(0);
    let before = handles();
    for n in 0..RUNS {
        piped(n);
    }
    let after = handles();
    eprintln!("handles before {before}, after {RUNS} piped-stdin runs {after}");
    assert!(
        after <= before + 12,
        "handles grew from {before} to {after} over {RUNS} piped-stdin runs"
    );

    // CreateProcessAsUserW failing (the folder doesn't exist) after the token, the pipes and
    // the handle list were made: everything made before it must be closed too.
    let missing = dir.path().join("no-such-folder");
    let failed = || {
        let mut builder = crate::shell::command_for_powershell(&missing, dir.path());
        let spec = Spec {
            stdin_piped: true,
            kill_on_drop: true,
        };
        assert!(runtime.block_on(async { spawn(&mut builder, spec, Sandbox::Low) }).is_err());
    };
    failed();
    let before = handles();
    for _ in 0..RUNS {
        failed();
    }
    let after = handles();
    eprintln!("handles before {before}, after {RUNS} failed spawns {after}");
    assert!(
        after <= before + 2,
        "handles grew from {before} to {after} over {RUNS} failed spawns"
    );
}
