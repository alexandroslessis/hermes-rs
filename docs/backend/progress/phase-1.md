# Phase 1 progress

## CP1.1 — Path policy and `check_edit`
Worktree: C:\Users\alexandros\repos\hermes-rs-p1   Branch: backend/p1-path-policy   Model: Sonnet 5.5
Cut from `main` at 00da66d, i.e. WITHOUT Phase 0 (its branch backend/p0-groundwork is committed
but not merged); `turns.rs` here has no purpose counters or baselines. Merge conflicts with P0 are
possible only in `crates/runtime/tests/turns.rs` (this branch adds `harness_for_data` and one test
at the end of the file).
Done:
- `crates/tools/src/path_policy.rs`: `PathClass` (Workspace / AppData / SelfImprove / Persistence /
  Outside), `PolicyContext`, `classify`. Canonicalises the existing prefix (junctions, symlinks,
  8.3 names, `\?\`), cleans the missing tail (trailing dots/spaces, `:stream`), case-insensitive.
  Order: Persistence, scratch (Workspace), skills/memories (SelfImprove), data folder (AppData),
  cwd (Workspace), else Outside. 12 unit tests incl. a real junction and a `\?\` path.
- `SessionToolsConfig.data_dir` (new field; set at the 5 construction sites).
- `check_edit` (`tools/src/lib.rs`) now classifies for every mode: AppData/SelfImprove refused in
  every mode (message names `skill_manage` / the memory tool); Plan as before; Persistence always
  asks, even Yolo; Manual asks for all; AcceptEdits/Auto ask for Outside; Yolo allows Outside.
  `write_file` and `patch` both pass through it (both take a single `path`).
- Tests: 5 unit tests in `lib.rs` (every mode, deny/allow, `patch`) and a `turns.rs` scenario
  (`writing_into_the_app_data_folder_is_refused_and_the_file_is_unchanged`).
Files: crates/tools/src/{path_policy.rs,lib.rs,review.rs}, crates/runtime/src/{lib.rs,curator.rs},
  crates/runtime/tests/turns.rs
Tests: `cargo test -p hermes_tools` 101 passed (1 ignored); `cargo test -p hermes_runtime --test
  turns` 70 passed (7 ignored); `cargo build --workspace --exclude hermes-desktop` OK (frozen GPUI
  app builds). `cargo build -p hermes-desktop` FAILS in the Tauri build script in this fresh
  worktree (no `target/hermes-src`, gitignored); not investigated further. The desktop crate does
  not construct `SessionToolsConfig`, so the new field should not affect it. `cargo test -p
  hermes-desktop` not run.
Request counts: unchanged.
Deviations from the plan: none. Note `Auto` now asks for Outside edits (it used to let every edit
  through), per the plan's table.
For the reviewer:
- Checkpoint restore (writes files back) is not routed through `check_edit`; documented in the
  module doc as out of scope (user-initiated, only paths the agent already edited). Check that
  claim in `checkpoints.rs`.
- Persistence list is mine (plan only names categories): `~/.ssh`, `~/.config/{git,powershell}`,
  `~/.gitconfig`, `.bashrc`-style rc files, `*profile.ps1` in Documents\{Windows}PowerShell,
  both Startup folders, `System32\Tasks`, `Windows\Tasks`, `.git/hooks/**`, `.git/config`. OneDrive-
  redirected Documents is not covered. 8.3 short names have no test (only canonicalize covers them,
  for existing prefixes).
- `patch` has a single `path` argument in this codebase (no multi-file patches), so "checks each
  file" is trivially met.
- Every `classify` call does several `canonicalize` syscalls (blocking fs in an async fn,
  ground rule 7): small, but consider `spawn_blocking` if you disagree.
- Existing tests that write outside the workspace in AcceptEdits/Auto would now ask; none failed.

### Review (Opus 5.5)
Reviewed 2026-11-12 in this worktree. The branch is at 846be90 (it already has Phase 0, so the
"cut from 00da66d" line above is stale); `main` has not moved, no merge needed. `/code-review` was
not run (its skill reviews the session's own checkout, not this worktree); review by reading the
code against the CP1.1 focus.

Findings and fixes:
- **Fixed (bug): the checkpoint snapshot ran before the policy.** `ServerHandler::call` took the
  checkpoint (`checkpoint_before`) and only then dispatched, where `check_edit` ran. A refused or
  declined edit still snapshotted the folder it aimed at: e.g. a declined write to `~/.ssh/x`
  copied the private keys into the checkpoint store. Now `call` runs `check_edit` first and then
  `dispatch_checked` (`dispatch` keeps the check for its other callers). New turn test
  `a_declined_git_hook_write_asks_even_in_yolo_and_takes_no_checkpoint` (Yolo, denied, no file, no
  checkpoint); confirmed it fails with the old order. (A refused data-folder write does not show
  this: that snapshot fails anyway on the open `sessions.db`.)
- **Fixed: redirected folders.** Documents redirected to OneDrive and a redirected roaming AppData
  were not Persistence. `PolicyContext` gains `documents` (`dirs::document_dir`) and `roaming`
  (`dirs::config_dir`); the `~`-based paths are still checked too. Unit test added.
- **Fixed (rule 7):** `classify` (several `canonicalize` calls) now runs in `spawn_blocking`; a
  panic there falls back to `Persistence` (asks).
- **Fixed:** the module doc's checkpoint-restore claim was inexact (restore puts back a whole
  snapshotted folder, not only edited files); reworded: user-initiated only, and the policy runs
  before snapshots. `rustfmt` applied; a clippy `clone` on a `Copy` type in the new test removed.
- Checked, fine: `..` (lexical fold matches Win32's own), junctions/symlinks/8.3/`\?\` via the
  canonical existing prefix, trailing dots/spaces and `:stream`/`::$INDEX_ALLOCATION` in the
  missing tail, case. `write_file` and `patch` are the only file-writing tools (vision/web write
  into scratch with names they generate; review/curator sessions don't expose `write_file`).
  Checkpoint restore is only reachable from `/rollback` and the page.

Left, with reasons:
- UNC paths to a local share (`\localhost\C$\Users\...`) are not mapped back to the drive path,
  so they classify `Outside`: asks in AcceptEdits/Auto/Manual, allowed in Yolo. Yolo's terminal
  can write anywhere anyway; Phase 15's sandbox is the boundary. Noted in the module doc.
- Subagents (`ChildBridge` answers Deny) can no longer write outside their workspace in
  AcceptEdits/Auto, and persistence files never. Intended by the table; worth a line in
  `PITFALLS.md` at CP1.2 (step 6).
- `cargo build -p hermes-desktop` / `cargo test -p hermes-desktop` not run (needs
  `target/hermes-src` in this worktree); the desktop crate doesn't build `SessionToolsConfig`.
Tests after review: `cargo test -p hermes_tools` 102 passed (1 ignored); `cargo test -p
hermes_runtime --test turns` 77 passed (7 ignored); `cargo build --workspace --exclude
hermes-desktop` OK.

## CP1.2 — Terminal paths, env scrub, skill-script flag, docs, live check
Worktree: C:\Users\alexandros\repos\hermes-rs-p1   Branch: backend/p1-path-policy   Model: Sonnet 5.5
Done:
- `path_policy.rs`: `command_flag` / `command_paths` / `words` / `expand_variables`. Quote-aware word
  splitting; `~`, `$HOME`, `${HOME}`, `$env:X`, `%X%` expanded for HOME, USERPROFILE, APPDATA,
  LOCALAPPDATA, ProgramData, HERMES_RS_HOME; each path-like word is resolved and classified.
  Persistence -> flagged (always asks); AppData / skills+memories -> flagged like a dangerous
  pattern; a file under `skills/**/scripts/` -> "runs a script the agent wrote into a skill".
- `SessionTools::check_command` (`tools/src/lib.rs`): a flag turns a "Run" into an "Ask". Persistence
  asks in every mode (even Yolo and approvals "off"), once-only, never smart-approved; the others
  are remembered by description like patterns and skipped in Yolo/off. Bug found and fixed on the
  way: the `session_approved` lock was held across the `match` (deadlock); now taken per call.
- `skills::is_stock_script(root, path)`: compares the file with the embedded `Bundled` copy byte for
  byte (not a manifest hash), so editing the extracted copy or the manifest can't make it pass.
- Env scrub (`shell.rs`): `command()` now does `env_clear()` + the parent env minus names containing
  KEY/TOKEN/SECRET/PASSWORD/PASSWD/CREDENTIAL/AUTH, then `shell_env`. `terminal.env_passthrough`
  (Settings config record) keeps names; applied in `Gateway::apply_settings` through
  `hermes_tools::set_env_passthrough` (process-global list). MCP stdio servers untouched.
- Docs: PITFALLS section, PORTING rows (`path_policy.rs` diverged, `shell.rs`/`approval.rs`
  notes), perf README line.
- Live check `scripts/perf/path-policy-check.mjs`: mock model writes into the perf instance's data,
  skills and memories folders; all refused with the right message, nothing written.
Files: crates/tools/src/{path_policy,lib,shell,skills}.rs, crates/desktop/src/gateway.rs,
  crates/runtime/tests/turns.rs, docs/PITFALLS.md, PORTING.md, scripts/perf/{README.md,path-policy-check.mjs}
Tests: hermes_tools 112 passed (1 ignored); hermes_runtime turns 78 passed (7 ignored); hermes-desktop
  67 + 26 passed; `cargo build --workspace` OK; live check `path-policy-check.mjs` all ok on a perf
  instance built from this worktree (port 47810, stopped afterwards).
Request counts: unchanged.
Deviations from the plan:
- The plan's "turns scenario" for the env scrub became a `shell.rs` unit test that runs a real shell
  (`the_shell_does_not_see_secret_variables_but_keeps_the_basics`); it ran in Git Bash here, the
  PowerShell branch of the test command was not exercised.
- No GAPS.md row: GAPS tracks Hermes feature gaps and has no section for backend v2; the phase is
  documented in PORTING/PITFALLS instead. The reviewer may prefer a different place.
- To build `hermes-desktop` in this worktree I made a directory junction `target/hermes-src` ->
  the main checkout's `target/hermes-src` (gitignored, read-only use) and ran node from
  `../hermes-rs/tools/node`.
For the reviewer:
- **Env scrub is the riskiest part**: check nothing the shell needs is dropped (`SSH_AUTH_SOCK`
  goes by design; `GIT_ASKPASS`, `PATH`, `SystemRoot`, `USERPROFILE`, `ProgramFiles*` stay). The
  process-global passthrough list is simple but global.
- `command_flag` flags reads too, and an existing pattern ("overwrite system file via redirection")
  still wins the description for `>> ~/.bashrc`; the persistence flag only changes the
  "never remembered / even Yolo" behaviour there. Unquoted paths with spaces aren't recognised.
- `approvals: None` sessions (curator) skip `check_command` entirely, as before.
- Not done from the plan text: none. Not verified: PowerShell variant of the env test; Settings UI
  for `terminal.env_passthrough` (config record only, as the page already has the key).
- Last checkpoint of Phase 1: docs and phase-map ✅ and the merge into main are the reviewer's.

### Review (Opus 5.5)
Reviewed 2026-11-12 in this worktree. `main` is still at 846be90 (an ancestor of this branch), so
no merge needed. `/code-review high` run on the uncommitted diff, plus the CP1.2 focus by reading
the code.

Review focus:
- **Env scrub keeps the basics.** Only `shell::command()` (foreground terminal and background
  processes) is scrubbed; `PATH`, `SystemRoot`, `USERPROFILE`, `ProgramFiles*`, `ComSpec`,
  `PATHEXT`, `TEMP`, `GIT_ASKPASS` match none of the words. On this machine the only variables
  dropped are the API key and some Claude Code ones. Git/lint/checkpoint/desktop spawns are not
  the agent's shell and are untouched.
- **MCP servers keep their configured env.** `runtime/src/mcp.rs` is unchanged: servers inherit
  the whole environment plus their config. The plan also asked to scrub servers that have *no*
  env config; not done, on purpose: many read their key from an inherited variable and would
  break silently. Recorded in PITFALLS; a candidate for Phase 15 / 6.
- **Bundled-skill check can't be fooled by editing the copy.** `is_stock_script` compares the
  file's bytes with the embedded `Bundled` copy (no manifest, no hash on disk); a path that isn't
  lexically under `skills/` (case, junction, `..` left after normalisation) is "not stock", i.e.
  asks: the safe direction. Limit, noted: a stock script that runs a sibling file the agent wrote
  is not caught (token matching only).
- **`check_command` flow.** Persistence: always_ask → `once_only`, never smart-assessed, the Yolo
  early return is skipped, Session/Always answers are not remembered; with approvals "off" the
  gate returns Run and the flag still turns it into Ask. Tests cover every mode plus "off".
  Non-persistence flags behave like dangerous patterns (remembered by description, skipped in
  Yolo/off). When a dangerous pattern also matches, its description wins (the flag's is not
  shown); harmless.
- **Passthrough list** is process-global but set by `Gateway::build` → `apply_settings` for both
  the app and `hermes-acp.exe`, and after every config write. Fine for one data folder per
  process.

Findings fixed:
- Rule 7: `command_flag` (several `canonicalize` per path word) ran on the async worker; now in
  `spawn_blocking`, a panic falls back to the persistence flag (asks).
- `expand_variables` could loop forever when a value contained its own pattern (`C:\Users\$home`);
  it now searches past each replacement. And `$HOME` matched the start of `$HOMEPATH`
  (`$env:X` likewise); open-ended patterns now need a non-identifier character after them.
  Test `variables_expand_whole_names_only_and_never_loop`.
- `AUTH` matched `GIT_AUTHOR_NAME/EMAIL`: the agent's commits would have changed author when
  those are set. `AUTHOR` is ignored before matching; unit test extended.
- The "skill script" check looked for a `scripts` component anywhere in the path, including the
  data folder's own location; now only below `skills/`. Test added.
- `scrubbed_env` compares passthrough names case-insensitively itself instead of relying on the
  setter having uppercased them.
- `cargo fmt` on the new code. Docs: GAPS.md row (section 5, Backend v2), ARCHITECTURE.md
  (`path_policy.rs`), PITFALLS (MCP and `GIT_AUTHOR_*`), phase map ✅.

Left, with reasons:
- Terminal commands naming the data / skills / memories folders *ask* (and can be smart-approved
  in Auto or remembered with "Always"), while the file tools refuse them. Reading logs or skills
  from the shell is legitimate; the terminal check is defence in depth until Phase 15.
- Token matching misses unquoted paths with spaces, paths built in variables, script contents;
  documented in PITFALLS and the module doc.
- The PowerShell branch of the shell env test is still unexercised here (Git Bash present).
- Live check not re-run: the review's changes touch only the terminal guard and the env scrub,
  which `path-policy-check.mjs` (file tools) does not exercise; the implementer's run stands.
Tests after review: `cargo test -p hermes_tools` 114 passed (1 ignored); `turns` 78 passed (7
ignored); `hermes-desktop` 158 + 67 passed; `cargo build --workspace` OK.
