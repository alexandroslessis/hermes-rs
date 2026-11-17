# Phase 15 progress

Worktree: C:\Users\alexandros\repos\hermes-rs-p15   Branch: backend/p15-sandbox

## CP15.1 — low-integrity token
Worktree: C:\Users\alexandros\repos\hermes-rs-p15   Branch: backend/p15-sandbox   Model: Opus 5.5 (subagent)
Cut from `main` at fc466d9 (Phase 14 merged). Not committed.
Done:
- **`crates/tools/src/sandbox.rs`** (new): `Sandbox { Off, Low }`, `Sandbox::from_config` (`"low"` any case; `"off"`, empty,
  null and anything unknown are `off`, unknown with a warning), a process-wide level (`set_sandbox` / `sandbox()`, an
  `AtomicU8`, read at every spawn), and one spawn function for every agent command: `sandbox::spawn(&mut Command, Spec
  {stdin_piped, kill_on_drop}, level) -> Spawned {child, stdin, stdout, stderr, tree}`.
  - `off`: `spawn_os` makes the same calls the callers made before (`kill_on_drop`, `stdin(piped|null)`, `stdout/stderr(piped)`,
    `spawn()`, `ProcessTree::contain(child.id())`); the only change on this path is that the pipes are boxed
    (`Box<dyn AsyncRead/AsyncWrite>`) and the child is wrapped in `enum Child { Os, Low }`.
  - `low` (`mod low`): `OpenProcessToken` -> `DuplicateTokenEx` (primary, QUERY|DUPLICATE|ASSIGN_PRIMARY|ADJUST_DEFAULT) ->
    `CreateWellKnownSid(WinLowLabelSid)` (S-1-16-4096) -> `SetTokenInformation(TokenIntegrityLevel)`; `CreatePipe` x3 (or the
    `NUL` device for stdin), all non-inheritable; a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` with exactly the three child ends;
    the child ends made inheritable just before `CreateProcessAsUserW(CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT |
    CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT)` and closed right after; `Job::new` + `Job::assign_handle` while suspended
    (nothing it starts can run outside the job), then `ResumeThread`. Program, arguments, folder and environment are read
    back from the same tokio `Command` the `off` path spawns (`as_std().get_program/get_args/get_current_dir/get_envs`);
    the command line and environment block are built as Rust's `std` builds them (`make_command_line`/`append_arg`,
    `make_envp`: sorted case-insensitively, `env_remove`d names left out); a bare program name is looked up in the child's
    `PATH`, then `System32`. Our pipe ends become `tokio::fs::File` (blocking-thread reads/writes, as tokio's own child pipes
    on Windows). `LowChild` gives `try_wait` (`WaitForSingleObject(0)`), `wait` (one `spawn_blocking` waiter per process,
    kept across dropped `wait` futures) and kill-on-drop (`TerminateProcess`, as tokio's).
  - RAII: every handle is a `std::os::windows::io::OwnedHandle` from the moment a call returns it; `HandleList` deletes the
    attribute list in `Drop` (only if initialised); `Suspended` terminates the created process in `Drop` unless it was
    resumed (an error after `CreateProcessAsUserW` leaves no frozen process); the `Job` closes itself. Each `unsafe` block
    is one Win32 call with a `SAFETY` comment.
  - Non-Windows: `low` logs a warning and spawns as `off` (cfg branches; the crate is Windows-only in practice).
- **Spawn sites moved onto `sandbox::spawn`** (every path that runs a command the agent wrote): the terminal's persistent Git
  Bash (`shell.rs` `spawn_bash`), the PowerShell terminal (`run_powershell`), background processes (`process.rs` `spawn`,
  `kill_on_drop: false` as before), `run_script` (`script.rs`, all three languages). Delegation uses these through the
  child's `SessionTools`. Not moved (not agent commands): git helpers (checkpoints, references, skills, context pack,
  commands), `lint.rs`, MCP stdio servers, `uv python find`, `taskkill`.
- `ShellSession::sandbox()`: Settings' level, or (tests only, `#[cfg(test)]`) a per-shell override, so tests never flip the
  process-wide level under the tests running beside them. `BashProcess.level`: a running bash started at another level is
  killed and restarted on the next command.
- `job.rs`: `Job::assign_handle(HANDLE)`; `shell.rs`: `ProcessTree::with_job(pid, Option<Job>)` (no job: `taskkill`
  fallback, as `contain`).
- Setting: `Gateway::apply_settings` (startup and every config change) calls
  `hermes_tools::set_sandbox(Sandbox::from_config(config_value("terminal.sandbox")))`. Not in `config_defaults`, so the
  page's schema and Settings show nothing new (no protocol shape changed, rule 5), as `terminal.env_passthrough`.
- Docs: `PORTING.md` row (`sandbox.rs`, diverged: ours, Hermes's local backend has no sandbox), `docs/PITFALLS.md` section
  "The shell sandbox", the module doc (sources: Microsoft's low-integrity guide, Rust std's `make_command_line` / `make_envp`).
- `crates/tools/Cargo.toml`: `windows` feature `Win32_System_Pipes` added (the others come from the workspace entry).
Files: `crates/tools/src/sandbox.rs` (new), `crates/tools/src/sandbox_tests.rs` (new), `crates/tools/src/{shell,process,script,job,lib}.rs`,
`crates/tools/Cargo.toml`, `crates/desktop/src/gateway/settings.rs`, `PORTING.md`, `docs/PITFALLS.md`, this note.
Tests (`CARGO_TARGET_DIR=target/claude`): `cargo test -p hermes_tools` 351 passed / 2 ignored (was 337; +14 new, 1 ignored
helper); `cargo test -p hermes_runtime --lib` 265; `--test turns` 258 passed / 7 ignored (unchanged); `cargo test -p
hermes-desktop` 191 lib + 68 acp; `cargo build --workspace` OK, no warnings. The worktree had no `target/hermes-src`: built with a
one-file stub `target/hermes-src/apps/desktop/dist-webapp/index.html` (gitignored), as in phase 14. `cargo clippy -p hermes_tools
--tests`: nothing new (pre-existing `skill_manage.rs` errors and a `shell.rs` `redundant_locals` warning are not from this change).
`scripts/clean-test-temp.ps1 -Delete` run afterwards (245 folders). No probe file left in `%USERPROFILE%`.
New tests (`sandbox_tests.rs`, 14 + 1 ignored helper):
- `a_low_shell_runs_with_the_low_integrity_label`: PowerShell at `low`, `whoami.exe /groups` shows S-1-16-4096 and not Medium.
- `an_off_shell_runs_at_the_apps_own_level`: `off` shows Medium (or High if the app is elevated), not Low.
- `a_low_shell_cannot_write_to_the_user_profile_but_can_write_where_low_may`: one PowerShell run tries
  `%USERPROFILE%\hermes-sandbox-probe-<pid>-<nanos>.txt` (denied; asserted absent afterwards, deleted if present) and a file
  in a test folder labelled Low with `icacls /setintegritylevel (OI)(CI)low` (test-only helper; written), so the denial
  can't be a broken process. Without `icacls` success only the denial is checked (printed).
- `a_low_git_bash_writes_nothing_to_the_user_profile`, `changing_the_level_restarts_a_running_git_bash`,
  `msys_programs_do_not_start_at_low`: see "For the reviewer" (Git Bash at Low).
- `a_low_command_is_killed_on_timeout_and_on_stop`: `Start-Sleep 60` with a 2 s timeout -> `timed_out`; with Stop after 2 s ->
  `interrupted`; both in well under 20 s.
- `background_processes_and_scripts_run_sandboxed_too`: `terminal(background=true)` + `process_manage wait` and
  `run_script` (PowerShell) both report S-1-16-4096.
- `sandboxed_spawns_leak_no_handles`: re-runs the test binary on the ignored `handle_count_child` (its own process, one test
  thread): 5 warm-up runs, then `GetProcessHandleCount` before/after 30 `low` runs; allowed growth 12. Measured: 145 -> 145.
  Mutation check: forgetting the token handle (`mem::forget`) fails it (158 -> 188).
- Pure: `the_config_value_reads_off_and_low`, `the_setting_defaults_to_off`, `command_lines_are_quoted_as_std_quotes_them`,
  `the_environment_block_is_sorted_and_leaves_removed_variables_out`, `programs_are_found_on_the_childs_path_then_in_system32`.
Request counts: unchanged (no request-building code touched; `turns` passes unchanged).
Live check: none. The page calls nothing new (no method, route or Settings field); the setting is config-only.
Deviations from the plan:
- **Git Bash can't run at Low** (see below). The plan assumed the shell would just run with fewer rights; with Git Bash (this
  machine's default terminal) `low` makes every terminal command fail. Not worked around in CP15.1 (it's a design choice for
  CP15.2/the user); pinned by tests and in PITFALLS.
- The setting is read at each spawn (the plan says per turn): it doesn't change what the model sees (no prompt-cache
  concern), and a running bash at another level is restarted.
- `run_script` and background processes are sandboxed too (the plan names "the shell"; both run model-written commands).
For the reviewer:
- **Git Bash at Low**: `bash.exe` (and every MSYS program, e.g. Git's `usr\bin\echo.exe`, `whoami`) dies at start:
  `NtCreateDirectoryObject(\BaseNamedObjects\msys-2.0S5-<hash>): 0xC0000022`, exit 0xC0000142. The terminal discards bash's
  stderr, so the model gets an empty output plus "The shell exited during this command..." on every call. Options for CP15.2:
  use PowerShell when `low` is on (and say so in the tool description? that changes the prompt mid-session: rule 8), or
  pre-create that directory object with a Low label (fragile: Medium MSYS processes the user runs create it first, and the
  objects inside it get Medium labels), or surface a clear tool error. Native tools (`git.exe`, `cargo`, `whoami.exe`) are
  fine from a Low PowerShell. `git commit` running hooks or an editor through `sh` would hit the same wall.
- **Inheritance window**: the child's pipe ends are inheritable between `SetHandleInformation` and their close right after
  `CreateProcessAsUserW`. A concurrent `std::process` spawn (git helpers, a tokio `Command` in another session) in that
  window inherits them (std passes `bInheritHandles=TRUE` under its own private lock). Effect: our reader sees EOF only
  when that other child exits (the PowerShell path reads to EOF; bash reads to the sentinel). Bounded by the other child's
  life; not a handle leak in our process. A fix would need a broker process or std's lock.
- `off` byte-for-byte: please diff `shell.rs` / `process.rs` / `script.rs`: the builder calls are the same set as before
  (moved into `spawn_os`), `ProcessTree::contain(child.id())` at the same point; differences are boxing and the `Child`
  enum. `script.rs` was not rustfmt-clean at HEAD; my edit there follows its existing style. `cargo fmt -p hermes_tools -p
  hermes-desktop` reformatted ~30 unrelated files (the tree isn't fmt-clean): those were restored from HEAD.
- `LowChild::wait`'s waiter is a `spawn_blocking` thread per waited process (tokio's own child uses
  `RegisterWaitForSingleObject`, no thread): a long-lived background process holds one blocking-pool thread while waited.
- Program lookup for a bare name (only `powershell.exe` today) is child `PATH` then `System32`; std also tries the app's
  folder and the parent's `PATH`. Same result for the programs we start.
- The token is made per spawn (cheap); `CreateProcessAsUserW` needed no privilege on this machine (non-elevated user; the
  token is a restricted copy of our own, so assignable). Not tried from an elevated app.
- Not verified: the desktop app with `terminal.sandbox = "low"` end to end (no live check; the gateway line is three lines
  in `apply_settings`), and ACP (`hermes-acp.exe` goes through the same `Gateway`, so the same `apply_settings`).

### Review (Opus 5.5)
`main` had not moved (fc466d9). Read by hand plus `/code-review high`.
Checked and found right: every handle is an `OwnedHandle` from the call that returns it (token, duplicate, pipe ends,
`NUL`, process, thread); the child's ends, the attribute list and the token are dropped before `created?`, so a failing
`CreateProcessAsUserW` leaks nothing; `Suspended` terminates the process if anything fails before `ResumeThread`, and the
`Job` terminates on drop too; the SID buffer outlives `SetTokenInformation`; the command line is a mutable NUL-terminated
`Vec<u16>`; the environment block is double-NUL with `CREATE_UNICODE_ENVIRONMENT`; `bInheritHandles = TRUE` with a
`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` of exactly the three child ends; flags match today's spawn (`CREATE_NO_WINDOW` from
`shell::command` is the only flag any caller sets; no `CREATE_NEW_PROCESS_GROUP` anywhere); suspended -> job -> resume
leaves no moment where the child runs outside the job. `off` path diffed against HEAD: same builder calls (`kill_on_drop`
false only for background processes; stdin piped for bash/background, null for PowerShell/scripts; stdout/stderr
piped), `ProcessTree::contain(child.id())` at the same point; only the boxing and the `Child` enum differ. The setting
is read from the config record only, unknown values are `off` with a warning, no protocol shape added.
Fixed:
- **The inheritance window is real.** A full `cargo test -p hermes_tools` failed
  `background_processes_and_scripts_run_sandboxed_too` once (exit 0, empty output): an `off` spawn beside it inherited the
  `low` script's stdout, and `run_script`'s 5 s drain gave up and returned *nothing*. Closing the window is not cheap (the
  listed handles must be inheritable, and std's spawn lock is private: it needs a broker, or
  `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS` with a helper process), so the readers no longer depend on EOF: `run_script`'s
  `finished` keeps what it read when the drain times out (shared buffer; the old code dropped it, also under `off`: the
  one change there, and only where output used to be lost), and `run_powershell` under `low` stops reading 1 s after
  PowerShell exits (unchanged under `off`). 5 full `hermes_tools` lib runs afterwards: all green. PITFALLS updated.
- Git Bash at Low now answers `MSYS_AT_LOW_ERROR` (exit status 0xC0000142 under `low`, checked both when the read ends
  and when writing the command fails) instead of an empty output; the tests assert that text.
- `LowChild::try_wait` returned "still running" on `WAIT_FAILED`; now the error, as tokio's.
- `resolve_program` tried an extensionless file before `name.exe`; std tries only `name.exe` on `PATH` (Git's `usr\bin`
  has extensionless scripts beside its programs). Test extended.
- The leak test now also counts 30 piped-stdin spawns and 30 failing `CreateProcessAsUserW` (missing folder): 150 -> 150
  each. Mutation check: leaking the token and a pipe end only on the failure path fails it (152 -> 212).
- Comments and the error text: a hand edit of `desktop-config.json` needs a restart (`apply_settings` runs at startup
  and on the page's config saves).
Git Bash finding, refined: `\BaseNamedObjects` is labelled Low (`S:AI(ML;;NW;;;LW)`), and a Low process *can* create a
new directory object there (checked: status 0). What fails is opening the existing `msys-2.0S5-<hash>` for write: it has
no label (= Medium) because Medium MSYS programs created it, and it exists while any MSYS program of that Git install
runs (the hash is of the install path; no env setting changes it). A copy of Git's `usr\bin` (`msys-*.dll`, `bash.exe`,
`echo.exe`) in a Low-labelled folder did start at Low from a Low PowerShell (`bash -c` ran; writing to `%USERPROFILE%` was
"Permission denied"; it warned about `/tmp` and has no mounts without an `etc`). Options for CP15.2: PowerShell under
`low`; a Low-labelled relocated copy of Git's `usr\bin` (its own object directory; needs `etc/fstab` and `/tmp`, and the
disk space); or the error as now.
Left (noted, not CP15.1 scope or not worth it now):
- The window itself; a long-lived inheritor (an MCP server started in that instant) keeps its copy of the pipe end.
- `LowChild::wait` parks a blocking-pool thread per waited process (tokio's uses `RegisterWaitForSingleObject`).
- `sandbox::spawn` runs on the async worker (as tokio's own spawn does); the `low` path adds a token copy and three
  `CreatePipe`s per spawn (could be cached; no perf work now).
- Background processes and bash scripts under Git Bash at Low fail with MSYS's own message on their stderr (captured,
  so the model sees it); only the terminal needed the explicit text.
- The non-Windows arm of `sandbox::spawn` can't compile anyway (`job.rs` is Windows-only); harmless.
Tests (reviewer, `CARGO_TARGET_DIR=target/claude`): `hermes_tools` 351 passed / 2 ignored (and 5 more lib runs green);
`hermes_runtime --lib` 265; `--test turns` 258 passed / 7 ignored; `hermes-desktop` 191 lib + 68 acp;
`cargo build --workspace --locked` OK, no warnings. `scripts/clean-test-temp.ps1 -Delete` run after; no probe file left in
`%USERPROFILE%`.
