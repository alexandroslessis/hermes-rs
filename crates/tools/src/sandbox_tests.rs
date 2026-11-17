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

/// Labels a test folder Low (inherited by what is made in it) by hand, independent of
/// `sandbox_labels`: through `icacls`; `false` when that isn't possible here.
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

/// Git Bash does not start at Low (CP15.1 finding, see the progress note and
/// `msys_programs_do_not_start_at_low`), so under `low` a Git Bash session's commands run in
/// PowerShell and the result says so (`RAN_IN_POWERSHELL`); nothing reaches the profile.
#[tokio::test]
async fn a_low_git_bash_session_runs_in_powershell_and_says_so() {
    let dir = hermes_testdir::tempdir().unwrap();
    let Some(shell) = git_bash_shell(dir.path()) else {
        return;
    };
    shell.set_sandbox_override(Some(Sandbox::Low));
    let probe = Probe::new();
    let command = format!(
        "try {{ Set-Content -LiteralPath {} -Value x -ErrorAction Stop; 'profile=written' }} catch {{ 'profile=denied' }}\n{WHOAMI_PS}",
        crate::shell::powershell_quote(&probe.0.display().to_string()),
    );
    let result = run(&shell, &command).await;
    let exists = probe.0.exists();
    drop(probe);
    shell.shutdown().await;
    assert!(
        !exists,
        "the sandboxed shell wrote to %USERPROFILE%: {}",
        result.output
    );
    assert!(result.output.contains("profile=denied"), "{}", result.output);
    assert!(result.output.contains(LOW_SID), "{}", result.output);
    assert_eq!(
        result.sandbox.first().map(String::as_str),
        Some(crate::shell::RAN_IN_POWERSHELL),
        "{result:?}"
    );
}

/// `off` -> `low` -> `off` on a Git Bash session: bash at the app's level, then PowerShell at
/// Low (the bash started before is stopped: it ran unsandboxed), then bash again, in the folder
/// the PowerShell command moved to.
#[tokio::test]
async fn switching_low_on_and_off_moves_a_git_bash_session_between_shells() {
    let dir = hermes_testdir::tempdir().unwrap();
    let Some(shell) = git_bash_shell(dir.path()) else {
        return;
    };
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    shell.set_sandbox_override(Some(Sandbox::Off));
    let off = run(&shell, WHOAMI_BASH).await;
    assert_eq!(off.exit_code, Some(0), "{off:?}");
    assert!(!off.output.contains(LOW_SID), "{}", off.output);
    assert!(off.sandbox.is_empty(), "{off:?}");
    shell.set_sandbox_override(Some(Sandbox::Low));
    let low = run(&shell, &format!("Set-Location sub\n{WHOAMI_PS}")).await;
    assert!(low.output.contains(LOW_SID), "{low:?}");
    assert_eq!(low.sandbox, [crate::shell::RAN_IN_POWERSHELL], "{low:?}");
    shell.set_sandbox_override(Some(Sandbox::Off));
    let back = run(&shell, &format!("pwd; {WHOAMI_BASH}")).await;
    shell.shutdown().await;
    assert_eq!(back.exit_code, Some(0), "{back:?}");
    assert!(!back.output.contains(LOW_SID), "{}", back.output);
    assert!(back.output.contains("sub"), "{}", back.output);
    assert!(back.sandbox.is_empty(), "{back:?}");
}

/// A session as the app makes one, laid out so the folders `low` labels are the test's own:
/// `<temp>/ws` (workspace), `<temp>/data` (data folder), `<temp>/data/scratch/ses_test`.
/// Fields drop in order: the tools, then the guard that removes every label the session set,
/// then the folder.
struct LowSession {
    tools: SessionTools,
    ws: PathBuf,
    data: PathBuf,
    _unlabel: Unlabel,
    dir: hermes_testdir::TestDir,
}

/// Puts back every folder the test's record lists, on drop (also when the test panics).
struct Unlabel(PathBuf);

impl Drop for Unlabel {
    fn drop(&mut self) {
        if let Err(error) = crate::sandbox_labels::remove_all(&self.0) {
            eprintln!("could not remove the test's sandbox labels: {error}");
        }
    }
}

fn low_session(shell: ShellKind, level: Sandbox) -> LowSession {
    let dir = hermes_testdir::tempdir().unwrap();
    let ws = dir.path().join("ws");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let tools = SessionTools::new(SessionToolsConfig {
        session_id: "ses_test".into(),
        cwd: ws.clone(),
        home: dir.path().to_path_buf(),
        data_dir: data.clone(),
        scratch_dir: data.join("scratch").join("ses_test"),
        shell,
        bridge: std::sync::Arc::new(TestBridge::default()),
        todos: Vec::new(),
        memory: None,
        history: None,
        skills: None,
        approvals: None,
        checkpoints: None,
        web: None,
    });
    tools.shell.set_sandbox_override(Some(level));
    tools.shell.set_grants_override(Vec::new());
    LowSession {
        tools,
        ws,
        _unlabel: Unlabel(data.clone()),
        data,
        dir,
    }
}

async fn terminal(session: &LowSession, command: &str) -> crate::ToolOutput {
    crate::shell::terminal(
        &session.tools,
        json!({"command": command, "timeout": 300}),
        None,
    )
    .await
}

fn output(result: &crate::ToolOutput) -> String {
    result.value["output"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_low_session_writes_in_its_workspace_and_scratch_but_not_the_profile() {
    let session = low_session(ShellKind::PowerShell, Sandbox::Low);
    let probe = Probe::new();
    let command = format!(
        "Set-Content -LiteralPath {} -Value x\n\
         try {{ Set-Content -LiteralPath ws.txt -Value x -ErrorAction Stop; 'ws=written' }} catch {{ \"ws=denied $_\" }}\n\
         try {{ Set-Content -LiteralPath (Join-Path $env:TEMP t.txt) -Value x -ErrorAction Stop; 'temp=written' }} catch {{ \"temp=denied $_\" }}\n\
         New-Item -ItemType Directory -Path made\\deeper | Out-Null\n\
         Set-Content -LiteralPath made\\deeper\\inner.txt -Value x",
        crate::shell::powershell_quote(&probe.0.display().to_string()),
    );
    let result = terminal(&session, &command).await;
    let exists = probe.0.exists();
    let probe_path = probe.0.display().to_string();
    drop(probe);
    let text = output(&result);
    assert!(!exists, "the sandboxed shell wrote to %USERPROFILE%: {text}");
    assert!(text.contains("ws=written"), "{text}");
    assert!(text.contains("temp=written"), "{text}");
    assert!(session.ws.join("ws.txt").is_file());
    // The refused write is named, with what to do about it.
    let note = result.value["sandbox"].as_str().unwrap_or_default();
    assert!(
        note.contains(&format!("blocked by sandbox: {probe_path}; grant the folder")),
        "{:?}",
        result.value
    );
    // The workspace, the scratch and the cache folders are recorded.
    let kinds: Vec<String> = crate::sandbox_labels::list(&session.data)
        .unwrap()
        .into_iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(kinds, ["workspace", "scratch", "cache"]);
    // What the Low command made carries the inherited label, and removal takes it off as well
    // as the workspace's own: nothing in the workspace stays Low.
    let made = [
        session.ws.clone(),
        session.ws.join("ws.txt"),
        session.ws.join("made"),
        session.ws.join("made").join("deeper"),
        session.ws.join("made").join("deeper").join("inner.txt"),
    ];
    let label = |path: &Path| {
        crate::sandbox_labels::classify(&crate::sandbox_labels::win::read_label(path).unwrap())
    };
    for path in &made[1..] {
        assert_eq!(
            label(path),
            crate::sandbox_labels::Label::LowInherited,
            "{}",
            path.display()
        );
    }
    let removal = crate::sandbox_labels::remove_all(&session.data).unwrap();
    assert!(removal.failed.is_empty(), "{removal:?}");
    for path in &made {
        assert_eq!(
            label(path),
            crate::sandbox_labels::Label::None,
            "{}",
            path.display()
        );
    }
}

#[tokio::test]
async fn git_status_and_commit_work_in_a_low_workspace() {
    let session = low_session(ShellKind::PowerShell, Sandbox::Low);
    let command = "git init -q 2>&1\n\
         Set-Content -LiteralPath a.txt -Value one\n\
         git add a.txt 2>&1\n\
         git -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false commit -q -m first 2>&1\n\
         \"commit=$LASTEXITCODE\"\n\
         Set-Content -LiteralPath a.txt -Value two\n\
         git status --porcelain 2>&1\n\
         git log --oneline 2>&1";
    let result = terminal(&session, command).await;
    let text = output(&result);
    assert!(text.contains("commit=0"), "{text}");
    assert!(text.contains(" M a.txt"), "{text}");
    assert!(text.contains("first"), "{text}");
    assert!(result.value.get("sandbox").is_none(), "{:?}", result.value);
}

/// `cargo build` of a crate with no dependencies, offline, with `CARGO_HOME` in the cache folder
/// (`prepare` sets it) and `CARGO_TARGET_DIR` in the workspace.
#[tokio::test]
async fn cargo_builds_in_a_low_workspace() {
    let has_cargo = std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_cargo {
        eprintln!("no cargo here; skipped");
        return;
    }
    let session = low_session(ShellKind::PowerShell, Sandbox::Low);
    let krate = session.ws.join("tiny");
    std::fs::create_dir_all(krate.join("src")).unwrap();
    std::fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        krate.join("src").join("main.rs"),
        "fn main() { println!(\"tiny ran\"); }\n",
    )
    .unwrap();
    let command = "Set-Location tiny\n\
         $env:CARGO_TARGET_DIR = Join-Path (Get-Location) target\n\
         \"home=$env:CARGO_HOME\"\n\
         cargo build --offline 2>&1 | ForEach-Object { \"$_\" }\n\
         \"build=$LASTEXITCODE\"\n\
         & .\\target\\debug\\tiny.exe";
    let result = terminal(&session, command).await;
    let text = output(&result);
    assert!(text.contains("build=0"), "{text}");
    assert!(text.contains("tiny ran"), "{text}");
    let cargo_home = session
        .data
        .join(crate::sandbox_labels::CACHE_DIR)
        .join("cargo");
    assert!(
        text.contains(&format!("home={}", cargo_home.display())),
        "{text}"
    );
}

#[tokio::test]
async fn granted_folders_are_labelled_and_writable() {
    let session = low_session(ShellKind::PowerShell, Sandbox::Low);
    let granted = session.dir.path().join("granted");
    let other = session.dir.path().join("other");
    std::fs::create_dir_all(&granted).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    session
        .tools
        .shell
        .set_grants_override(vec![granted.clone()]);
    let quote =
        |p: &Path| crate::shell::powershell_quote(&p.join("f.txt").display().to_string());
    let command = format!(
        "try {{ Set-Content -LiteralPath {} -Value x -ErrorAction Stop; 'granted=written' }} catch {{ 'granted=denied' }}\n\
         try {{ Set-Content -LiteralPath {} -Value x -ErrorAction Stop; 'other=written' }} catch {{ 'other=denied' }}",
        quote(&granted),
        quote(&other),
    );
    let result = terminal(&session, &command).await;
    let text = output(&result);
    assert!(text.contains("granted=written"), "{text}");
    assert!(text.contains("other=denied"), "{text}");
    let granted = crate::project_skills::canonical(&granted).unwrap();
    let folders = crate::sandbox_labels::list(&session.data).unwrap();
    assert!(
        folders
            .iter()
            .any(|f| f.kind == "grant" && f.path == granted),
        "{folders:?}"
    );
}

/// `off` with the same session layout: no label, no record, no note, the app's own level, and
/// Git Bash stays Git Bash.
#[tokio::test]
async fn off_labels_nothing_and_adds_nothing() {
    let session = low_session(crate::shell::detect_shell(), Sandbox::Off);
    let command = match session.tools.shell.kind() {
        ShellKind::GitBash(_) => WHOAMI_BASH,
        ShellKind::PowerShell => WHOAMI_PS,
    };
    let result = terminal(&session, command).await;
    let text = output(&result);
    assert_eq!(result.value["exit_code"], 0, "{:?}", result.value);
    assert!(!text.contains(LOW_SID), "{text}");
    assert!(result.value.get("sandbox").is_none(), "{:?}", result.value);
    assert!(
        !session
            .data
            .join(crate::sandbox_labels::RECORD_FILE)
            .exists()
    );
    let label = crate::sandbox_labels::win::read_label(&session.ws).unwrap();
    assert_eq!(
        crate::sandbox_labels::classify(&label),
        crate::sandbox_labels::Label::None
    );
    session.tools.shutdown().await;
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
    let session = low_session(ShellKind::PowerShell, Sandbox::Low);
    let tools = &session.tools;
    let started = crate::shell::terminal(
        tools,
        json!({"command": WHOAMI_PS, "background": true}),
        None,
    )
    .await;
    let id = started.value["session_id"].as_str().unwrap().to_string();
    let waited = crate::process::process_manage(
        tools,
        json!({"action": "wait", "session_id": id, "timeout": 60}),
    )
    .await;
    assert_eq!(waited.value["exit_code"], 0, "{:?}", waited.value);
    let output = waited.value["new_output"].as_str().unwrap_or_default();
    assert!(output.contains(LOW_SID), "{output}");

    let script = crate::script::run_script(
        tools,
        json!({"language": "powershell", "code": format!("{WHOAMI_PS}\n$env:UV_CACHE_DIR")}),
        None,
    )
    .await;
    assert_eq!(script.value["exit_code"], 0, "{:?}", script.value);
    let output = script.value["output"].as_str().unwrap_or_default();
    assert!(output.contains(LOW_SID), "{output}");
    // The cache variables reach scripts too.
    assert!(
        output.contains(crate::sandbox_labels::CACHE_DIR),
        "{output}"
    );
    // Bash scripts are refused up front under `low` (Git Bash can't start at Low).
    let bash = crate::script::run_script(
        tools,
        json!({"language": "bash", "code": "echo hi"}),
        None,
    )
    .await;
    assert!(bash.is_error, "{:?}", bash.value);
    tools.shutdown().await;
}

/// Under `low`, a Git Bash session's background processes run in PowerShell too, and say so.
#[tokio::test]
async fn a_low_git_bash_session_starts_background_processes_in_powershell() {
    let ShellKind::GitBash(bash) = crate::shell::detect_shell() else {
        return;
    };
    let session = low_session(ShellKind::GitBash(bash), Sandbox::Low);
    let started = crate::shell::terminal(
        &session.tools,
        json!({"command": WHOAMI_PS, "background": true}),
        None,
    )
    .await;
    assert_eq!(
        started.value["sandbox"].as_str(),
        Some(crate::shell::RAN_IN_POWERSHELL),
        "{:?}",
        started.value
    );
    let id = started.value["session_id"].as_str().unwrap().to_string();
    let waited = crate::process::process_manage(
        &session.tools,
        json!({"action": "wait", "session_id": id, "timeout": 60}),
    )
    .await;
    let output = waited.value["new_output"].as_str().unwrap_or_default();
    assert!(output.contains(LOW_SID), "{output}");
    session.tools.shutdown().await;
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

