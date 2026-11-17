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

## CP15.2 — labels, caches, grants, errors
Worktree: C:\Users\alexandros\repos\hermes-rs-p15   Branch: backend/p15-sandbox   Model: Opus 5.5 (subagent)
On top of 497b24c (CP15.1). Not committed.
Done:
- **`crates/tools/src/sandbox_labels.rs`** (new): the folders a `low` command may write to.
  - `prepare(data, workspace, scratch, grants)` (called by `ShellSession::prepare_low` on a blocking thread before every `low`
    spawn: terminal, background process, `run_script`): labels the workspace (repository root, else the session's start folder;
    `SessionTools::new` passes it via `ShellSession::with_sandbox_folders`), the session's scratch (`<data>\scratch\<id>`, where
    `TEMP`/`TMP` already point), the shared cache folder `<data>\sandbox-cache` and `terminal.sandbox_grants`; returns the cache
    variables, the writable folders and notes (a refused or missing folder: said in the result, the command still runs).
  - `ensure_low`: canonical path -> refusal check -> (tests: must be inside the temp folder, else panic) -> record lock -> read the
    label -> by state: none: **record first** (`sandbox-labels.json` in the data folder: path, original label SDDL, kind,
    time, `complete: false`), then our explicit label `S:(ML;OICI;NW;;;LW)` on the folder, then `label_tree` gives everything
    inside the inherited label (`(ML;ID;NW;;;LW)` files, `(ML;OICIID;NW;;;LW)` folders), then `complete: true`; ours and
    recorded and complete: nothing; ours but not complete (walk cut short): the walk again; inherited Low (inside a grant,
    `LocalLow`): nothing, not recorded; ours-shaped but not recorded (e.g. icacls), or any other label: refused.
  - Refusals (`Protected::refusal`): not absolute, drive root, the profile (`USERPROFILE` and `dirs::home_dir`) and the data
    folder and anything containing them (so `C:\Users` too), `SystemRoot`/`windir`/`ProgramFiles*`/`ProgramData` and anything
    containing them or inside them, anything inside the data folder except a session folder under `scratch\` and
    `sandbox-cache`.
  - `remove_all` (deepest recorded folder first): gone -> forgotten; ours (or never set) -> `unlabel_tree` (inherited labels
    off, deepest first, so a removal cut short can run again) and the original label back; someone else's label now -> left,
    forgotten, reported; failures stay recorded. `list` for `/sandbox`.
  - **Win32 (`mod win`)**: `GetFileSecurityW` / `SetFileSecurityW` with `LABEL_SECURITY_INFORMATION`, one object at a time,
    plus the SDDL conversions; `Local` frees what Windows allocated. **Not `SetNamedSecurityInfoW`** (the plan's call): tried
    first, it rewrote the DACLs of the folder and everything under it (inherited ACEs became explicit: this machine's repos
    have DACLs without the auto-inherited flag); `icacls /setintegritylevel` marks every DACL `D:AI`. Either way removal
    could not restore the descriptor. The walk skips reparse points (junctions, symlinks: not followed, not labelled) and
    stops at labels that aren't ours and protected SACLs.
  - `blocked_hint`: under `low`, an output line with "access is denied", "access to the path", "permission denied",
    "os error 5", "eperm" or "unauthorizedaccessexception" gives "blocked by sandbox: <path>; grant the folder in Settings
    (terminal.sandbox_grants in desktop-config.json) or move the work into the workspace"; the path is the first Windows path
    on that line (quoted or up to a space), "(a path the output does not show)" when none; no hint when the path is inside a
    writable folder (a locked file in the workspace).
  - `cache_env`: `CARGO_HOME`, `npm_config_cache`, `YARN_CACHE_FOLDER`, `UV_CACHE_DIR`, `PIP_CACHE_DIR`, `GOCACHE`,
    `GOMODCACHE` -> `<data>\sandbox-cache\...`; the user's cargo `config.toml`/`config` copied into the new `CARGO_HOME`
    (not `credentials`).
- **What breaks at Low, tested on this machine** (a throw-away ignored probe test, removed): `cargo build` of a dependency-
  free crate and of one whose crate is already cached: work with the default `CARGO_HOME`; a crate to download: "failed to
  write cache ... Access is denied" (fails); with `CARGO_HOME` in a Low folder: works (rustup's proxy fine). npm install:
  `EPERM` on `%LOCALAPPDATA%\npm-cache` (fails); with `npm_config_cache` moved: works. `uv run`: "Failed to initialize
  cache" (fails); with `UV_CACHE_DIR` moved: works. `git init/add/commit/status` (no hooks): work. pip, go, dotnet: not
  installed here (pip/go variables set from their docs; dotnet not handled).
- **Git Bash (orchestrator's decision)**: under `low` every command runs in PowerShell: `ShellSession::run` -> `run_low`
  (a running Medium bash is shut down first; `cwd` carries over), `process::spawn` builds PowerShell, `run_script` refuses
  `language: bash` up front. The result's new `sandbox` field (terminal, background start, `run_script`) carries
  `sandbox=low: ran in PowerShell, not bash`, label notes and the blocked hint. System prompt and tool descriptions are not
  touched (rule 8). `MSYS_AT_LOW_ERROR` / `msys_died_at_low` and the bash level-restart are gone (bash never starts at Low
  now); the relocated-`usr\bin` option is not implemented.
- `RunResult.sandbox: Vec<String>` (empty under `off`); `ShellSession::{with_sandbox_folders, prepare_low, run_low}`;
  `LowStart {env, notes, writable}`; test-only `set_grants_override`.
- **Setting / exposure**: `terminal.sandbox` stays default `off`. `terminal.sandbox_grants` (config record only, list of
  absolute paths) read by `apply_settings` -> `hermes_tools::set_sandbox_grants`; labelled by the next `low` command.
  **Removal is exposed as a slash command of ours**, `/sandbox` (level, grants, labelled folders) and
  `/sandbox remove-labels`, answered by `Gateway::builtin_command` through the existing `slash.exec` (`{"type":"exec",
  "output"}`, the shape `/import-hermes` uses) and listed in the `/` menu (`commands.rs`). No new method, route or Settings
  field; no page protocol shape added.
- `crates/tools/Cargo.toml`: `windows` feature `Win32_Security_Authorization` (no `Cargo.lock` change).
- Docs: PITFALLS "The shell sandbox" (rewritten for CP15.2: decision, what is labelled and refused, why `SetFileSecurityW`,
  the `S:NO_ACCESS_CONTROL` difference, slow first walk, hard links, caches, the hint, test guards), ARCHITECTURE (tools row
  and feature map row), PORTING `sandbox.rs` row (CP15.2 sentence), `crates/desktop/README.md` "Shell sandbox" (how to turn
  it on), module docs (`sandbox.rs`, `shell.rs`, `sandbox_labels.rs`). GAPS: no row (not a Hermes gap).
Live check: none. The page calls nothing new (`/sandbox` goes through `slash.exec`, whose answer shape is unchanged);
`gateway::tests::the_sandbox_command_reports_and_removes_labels` drives it through `Gateway::call`.
Tests (`CARGO_TARGET_DIR=target/claude`): `cargo test -p hermes_tools` 367 passed / 2 ignored (was 351; two full runs green);
`hermes_runtime --lib` 265; `--test turns` 258 passed / 7 ignored (unchanged); `hermes-desktop` 193 lib + 68 acp (three runs);
`cargo build --workspace` OK, no warnings. `cargo clippy -p hermes_tools -p hermes-desktop --tests`: nothing new in the touched
files (the `shell.rs` `redundant_locals` and the other files' findings are older). `scripts/clean-test-temp.ps1 -Delete` run
after (441 folders); no probe file left in `%USERPROFILE%`.
New tests:
- `sandbox_labels_tests.rs` (9): SDDL classification; refusals (drive root, profile, `C:\Users`, Windows and inside, Program
  Files and inside, the data folder, inside it, the scratch root, a folder containing the data folder; pure checks only, and
  `ensure_low` on the profile refuses before touching the record); **round trip**: every object of a small tree (folder,
  subfolders, files) read with `GetFileSecurityW` before, labelled (folder explicit, everything inside inherited, owner/group/
  DACL unchanged), removed -> equal to before (modulo `S:NO_ACCESS_CONTROL`, see below), twice; idempotence (`AlreadyOurs`,
  one record), a folder inside a labelled one is `AlreadyLow` and not recorded; foreign labels (explicit Medium, and our exact
  label set by hand) refused and left unchanged; removal with one folder deleted (`gone`), one relabelled by someone else
  (`changed`, left as is) and one restored; an unreadable record stops labelling (and removal); a junction inside a labelled
  folder is not followed (its target's descriptors unchanged through label and removal); an unfinished labelling
  (`complete: false`) is finished next time; `prepare` (workspace, scratch, cache, grant labelled, missing and relative grants
  noted, cache variables, a workspace containing the data folder refused); `blocked_hint` on PowerShell, cmd, git, cargo,
  Python, npm lines, and none for a path inside the workspace.
- `sandbox_tests.rs` (end to end, real `SessionTools` laid out as `<temp>\ws`, `<temp>\data`, `<temp>\data\scratch\ses_test`,
  with an `Unlabel` guard): workspace and `TEMP` writable, `%USERPROFILE%\<probe>` denied (absent afterwards) with the
  "blocked by sandbox: <probe>" note; `git init/add/commit/status/log` in the workspace; `cargo build --offline` of a
  dependency-free crate with `CARGO_HOME` in `sandbox-cache` and `CARGO_TARGET_DIR` in the workspace, and the exe runs; a grant
  is labelled and writable, a sibling folder is not; `off` labels nothing, writes no record, adds no `sandbox` field, runs Git
  Bash at Medium; a Git Bash session under `low` runs in PowerShell with the note (profile write denied); off -> low -> off
  moves a Git Bash session to PowerShell and back (folder carried over); a Git Bash session's background process under `low`
  runs in PowerShell with the note; background process and `run_script` at Low get the cache variables, `run_script` bash
  refused. Replaced CP15.1's two Git-Bash-at-Low tests (the case can no longer happen).
- `gateway::tests`: `/sandbox`, `/sandbox remove-labels`, an unknown argument through `slash.exec`; the grants' config parser.
Request counts: unchanged (no request-building code touched; `turns` passes unchanged).
Deviations from the plan / the brief:
- **`SetFileSecurityW` and our own inheritance walk instead of `SetNamedSecurityInfoW`** (reason above: it rewrote DACLs).
- **Caches go to a shared `<data>\sandbox-cache`, not the per-session scratch**: per session, every new session would download
  every crate / npm package again (and scratch folders are never cleaned). The cost: sandboxed sessions share (and could
  poison) one cache; unsandboxed use (`~\.cargo`, the user's own npm) is unaffected. `TEMP`/`TMP` stay per session.
- **Not exactly byte-for-byte**: an object that had no SACL reads `S:NO_ACCESS_CONTROL` after removal (an empty SACL: same
  meaning). Removing it needs `SeSecurityPrivilege`. Tests compare everything else exactly.
- Grants are labelled by the next `low` command, not at the moment the config changes (they need nothing before a
  sandboxed command runs, and `off` labels nothing). Removing a grant from the config doesn't unlabel it:
  `/sandbox remove-labels` does.
- The hint says "grant the folder in Settings (terminal.sandbox_grants in desktop-config.json)": there is no Settings field.
For the reviewer:
- Labels only on recorded folders: the record is written (and saved) before the first `SetFileSecurityW` on the root; children
  get only *inherited* labels and only under a recorded root; removal touches only inherited labels below recorded roots and
  the roots' own label. Check `ensure_low` / `remove_all` / `label_tree` / `unlabel_tree` against that; and the refusal list
  (is anything missing? e.g. other users' profiles are covered only as "contains `C:\Users`"... a folder *inside* another
  user's profile is not refused, nor `AppData` subfolders of our own profile).
- The first `low` command in a large workspace walks every file under the record's lock (other sessions' `low` commands
  wait). Not measured on a big repo (no perf work asked); hermes-rs itself with `target\` would be the test.
- A file hard-linked into the workspace (pnpm, some cargo setups) is labelled for all its links.
- The workspace is the repository root (`find_root`) or the start folder; a `cd` elsewhere is not labelled. A session started
  in the profile or in a folder that contains the data folder gets "the workspace folder is not writable" on every command.
- The hint's path extraction is heuristic (first `X:\` path on the line); a path with spaces and no quotes is cut at the space.
- `run_low` shuts down a running Medium Git Bash (exported variables lost) before each `low` command; `off` again starts a new
  bash in the folder PowerShell left.
- Background processes' output is not scanned for the hint (only the terminal and `run_script`).
- Not verified: the desktop app end to end with `terminal.sandbox = "low"` (no live check, the page calls nothing new);
  ACP shares the same `Gateway`, so `/sandbox` works there too (catalog entry), not tried.

### Review (Opus 5.5)
`main` had not moved (fc466d9). Read by hand (the `/code-review` skill was not run inside this subagent), with the review
focus "labels only on recorded folders, removal restores exactly" attacked case by case.
Checked and found right: the record is saved before the first label is set and the entry dropped again if that set fails
(a crash in between leaves a record without a label, which the next command finishes or removal skips, never the
reverse); a second run never overwrites the recorded original (`recorded.is_none()` guards the push; an incomplete or
relabelled entry keeps its first `original`); a walk cut short is resumed (`complete: false`); a corrupt record stops
labelling and removal; a missing record is empty (nothing to remove); the record is written to a temp file and renamed;
deleted folders are forgotten; someone else's label (explicit Medium, or our exact SDDL set by `icacls`) is refused and
left; nested grants (a grant inside a labelled folder is `AlreadyLow`, not recorded; a labelled folder inside a later one is
skipped by the outer walk and restored on its own, deepest first). **The `SetNamedSecurityInfoW`/`icacls` claim, checked
independently**: `icacls /setintegritylevel (OI)(CI)low` on a fresh `%TEMP%` folder turned every DACL below it from
`D:(...)` into `D:AI(...)` (read with `Get-Acl`, folder deleted after). The `S:NO_ACCESS_CONTROL` difference is real and
harmless (an empty SACL); no code path writes owner, group or DACL (only `LABEL_SECURITY_INFORMATION` is ever passed to a
set call). Files a Low command creates get the *inherited* label and removal clears them (new end-to-end assertion). `off`
paths: the same builder calls as 497b24c; nothing labelled, no record, no `sandbox` field (test). System prompt and tool
descriptions untouched; the `sandbox` field appears only under `low`; `/sandbox` is a user-typed `slash.exec` command (the
model has no tool that reaches it), listed in the `/` menu and, through `commands.catalog`'s pairs, to ACP editors like
`/import-hermes`; labelling and removal run on blocking threads (rule 7).
Fixed:
- **TOCTOU through junctions.** The walk listed entries with `read_dir`, then labelled by *path* with `SetFileSecurityW`,
  which follows reparse points; removal checked `exists()` and walked the path. A sandboxed command can rename a folder
  inside a Low folder and put a junction in its place (it holds `FILE_DELETE_CHILD` there), so a walk running at that
  moment (a grant or another session's scratch being labelled while a Low command runs) could label the junction's target,
  e.g. `Documents`, and descend into it; removal could walk a recorded folder that is now a junction. Now every object is
  opened with `FILE_FLAG_OPEN_REPARSE_POINT` (a link is opened as itself and skipped), its real path is read from the
  handle (`GetFinalPathNameByHandleW`) and must still be under the root, and the label is read and set through that same
  handle (`GetKernelObjectSecurity` / `SetKernelObjectSecurity`, label only: what `SetFileSecurityW` does after its own
  open). The root is opened the same way after `canonical`; removal reports a recorded folder that is now a link or leads
  elsewhere as `changed` and leaves it. Tests: `an_entry_reached_through_a_junction_is_skipped` (an entry under a junction
  is `Skip`), `removal_does_not_follow_a_recorded_folder_replaced_by_a_junction` (the target's descriptors unchanged; the
  old code reported it `restored`).
- **Hard links.** A file hard-linked into the workspace was labelled for all its names (pnpm's store, or a file outside the
  workspace, became Low-writable). Files with more than one name are now left unlabelled and counted; `ensure_low`
  answers `Partly(why)` (the folder still counts as writable; the note says how many). Before, any failed item made
  `ensure_low` an error, so a labelled workspace was left out of `writable` and the hint fired on its own paths. Test
  `hard_linked_files_are_left_alone`.
- **Refusal list.** A session started in `AppData`, `Documents`, `~\.cargo` or a repository holding a `PATH` folder would
  have labelled places a planted file runs from later at Medium (Startup, `Documents\WindowsPowerShell\profile.ps1`,
  `~\.cargo\bin\cargo.exe`, anything on `PATH`), and `C:\Users\Public` / another profile were allowed. Added: `APPDATA`,
  `LOCALAPPDATA` and every absolute `PATH` folder (is or contains); `PUBLIC`, `~\.ssh`, `~\.cargo`, `~\.rustup`, the Start
  Menu, `Documents\WindowsPowerShell` and `Documents\PowerShell` (is, contains or inside); anything under the profiles'
  parent but outside the user's own profile. The data folder's scratch/cache exception is checked first (it sits in
  `APPDATA`). Spellings (8.3 short name from `%TEMP%`, `\\?\`, upper case, `\.`, `..`) all reach the refusal through
  `canonical`. Tests `other_spellings_of_the_profile_are_refused_too`,
  `appdata_path_folders_startup_and_other_profiles_are_refused`.
- **Cross-process lock.** `RECORD_LOCK` was in-process only; the desktop app and `hermes-acp.exe` share the data folder, so
  two read-modify-writes could lose a folder from the record (a label without a record). Now the mutex plus an exclusive
  `File::lock` on `sandbox-labels.lock`.
- **Cargo credentials.** `config.toml` was copied verbatim into the Low-writable `sandbox-cache\cargo`; a `[registry]` /
  `[registries.x]` `token`, `secret-key` or `secret-key-subject` (inline tables too) is now dropped (`without_credentials`,
  `toml` parse; unparsable: not copied; a source config that is gone removes the copy). `credentials.toml` never was. npm
  and pip configs are not copied (only cache folders move). Test `the_cargo_config_copy_has_no_credentials`.
- Independent DACL check added to the round-trip test: `icacls <ws> /t` (read only) prints the same before labelling and
  after removal. `/sandbox remove-labels` says a moved folder keeps its label where it is now. Docs: PITFALLS (refusals,
  handles, hard links, lock, cargo config, the workspace-planting limit), module doc, `crates/desktop/README.md`, PORTING.
Left (noted, not worth it now or by design):
- A sandboxed command can still change files in the workspace that the user later runs at Medium (`build.rs`, npm
  scripts, `.git\hooks`): inherent to a writable workspace; said in PITFALLS and the README.
- The first walk of a large workspace holds the record lock with no time limit (blocking thread, not the async workers).
- Reparse points that aren't links (OneDrive placeholders) are skipped like links (read-only to commands).
- A moved recorded folder keeps its label at its new place (forgotten as `gone`).
- `blocked_hint` heuristics as the implementer described (an unquoted path with spaces is cut; a false hint costs one line).
- The pipe-inheritance window (CP15.1).
Tests (reviewer, `CARGO_TARGET_DIR=target/claude`): TESTS_PLACEHOLDER
`scripts/clean-test-temp.ps1 -Delete` run after. Checked after the runs: `%USERPROFILE%`, both checkouts and the
data folder carry no mandatory label and their DACL flags are unchanged (`D:P`, `D:`, `D:`; the data folder's `D:AI` is
`%APPDATA%`'s own); no `hermes-sandbox-probe*` in `%USERPROFILE%`; no `sandbox-labels.json` in `%APPDATA%\hermes-rs`.

### USER CHECKLIST (Phase 15)
1. Close the app. Open `%APPDATA%\hermes-rs\desktop-config.json` in an editor and add, at the top level (or merge into an
   existing `"terminal"` object): `"terminal": { "sandbox": "low" }`. Optionally add folders commands may also write to:
   `"terminal": { "sandbox": "low", "sandbox_grants": ["D:\\datasets"] }` (absolute paths, doubled backslashes). Save,
   start the app.
2. Open a chat in a repository folder (e.g. a throw-away clone) and type `/sandbox`. Expected: `terminal.sandbox: low`,
   your grants, "No folder is labelled Low by this app." (nothing is labelled before the first command).
3. Ask the agent to run `whoami /groups` (Windows' own: `C:\Windows\System32\whoami.exe /groups`). Expected: the
   result's `sandbox` field says "sandbox=low: ran in PowerShell, not bash" (if your terminal is Git Bash), and the
   groups list "Mandatory Label\Low Mandatory Level". The first command in a big repository may take a while (labelling).
4. Type `/sandbox` again. Expected: the repository folder (`workspace`), the chat's scratch folder, `sandbox-cache` and
   your grants listed as labelled.
5. Ask it to create a file in the repository (works) and one in your user folder, e.g.
   `Set-Content $env:USERPROFILE\sandbox-test.txt x` (fails: "Access to the path ... is denied", and the result says
   "blocked by sandbox: C:\Users\...\sandbox-test.txt; grant the folder ..."). Check that file does not exist.
6. Ask it to run `git status` and a `git commit` in the repository, and `cargo build` (or `npm install`, `uv run`) in a
   project there: they should work (downloads go to `%APPDATA%\hermes-rs\sandbox-cache`). Git hooks or an editor that
   need `sh` will fail at Low.
7. To turn it off: close the app, set `"sandbox": "off"` (or remove it), start the app. Then type
   `/sandbox remove-labels`. Expected: "Sandbox labels removed: N folder(s) put back as they were." and the folders
   listed; `/sandbox` then says nothing is labelled. (Removing the labels while still on `low` works too, but the next
   command labels the folders again.)
8. Optional: `icacls <your repo>` before step 3 and after step 7 should print the same lines (no "Mandatory Label").
