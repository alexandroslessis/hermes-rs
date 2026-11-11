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
