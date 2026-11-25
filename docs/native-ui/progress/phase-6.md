# Phase 6 progress

Worktree: C:\Users\alexandros\repos\hermes-rs-n6   Branch: native/p6-panel

## CP6.1 — Diff
Worktree: C:\Users\alexandros\repos\hermes-rs-n6   Branch: native/p6-panel   Model: Sonnet 5.5 (subagent)
Cut from `main` at 2a5fbf8. Uncommitted.

### Files
- New `crates/native/src/panel/`: `mod.rs` (`Tab`, `Tabs` = the tab model, `RightPanel` = the strip and the open tab, the actions it
  answers), `diff.rs` (`DiffPanel`: reading, scopes, the file list or tree, revert with its confirmation, open in editor, the toolbar and
  the diff over GPUI Kit's `Diff`), `data.rs` (no GPUI views: `Scope`, `DiffRequest`, `DiffData`, `Revert`, `Entry`, `Parsed`/`parse_patch`,
  `names_file`, `merge_turn_files`, `patch_from_changes`, `editor_command`), `backend.rs` (the real core's calls), `fixture.rs` (a fake
  repository), tests `tests.rs` (fixture, 21), `real_tests.rs` (real core and git, 5); new `gallery/panel.rs` (story `panel-diff`).
- Edited: `model/backend.rs` (`Backend::diff`, `revert_file`, `head_commit`, `open_in_editor`; `CoreBackend` delegates to `panel/backend.rs`,
  `FixtureBackend` to `panel/fixture.rs`), `model/mod.rs` (`AppModel::backend()`), `thread/model.rs` (`turn_base` read at a turn's start,
  `turn_context()`, `refine_footer`, `ThreadEvent::TurnEnded`), `thread/work.rs` (`Footer::replace_files`), `workspace.rs` (the right
  panel entity replaces the placeholder; `OpenDiffTab` / `OpenFileInPanel` go to it; visibility follows the panel's open state),
  `design/icons.rs` (7 Lucide icons), `testing.rs` (`no_overflow` skips the diff's clipped rows), `lib.rs`, `gallery/mod.rs`, two
  tests that expected the Phase 4 placeholder (`thread/tests.rs`, `thread/tests/tools.rs`).
- **Core (`hermes_core`):** `git.rs` (`GIT_TIMEOUT`, `run_limited`, `git_timed`, `git_program`; `git()` keeps its shape), `git_review.rs`
  (`run()` uses the same limit; new `review_patch` and `ReviewPatch`). Nothing else; `crates/desktop` is untouched.
- Docs: AGENTS.md row, ARCHITECTURE.md row, TESTING.md §6 (the panel paragraph), PITFALLS.md (6 bullets), PHASES.md status, native-ui/README.md
  module row, reference/t3code/README.md (no new screenshot), PORTING.md (`git_review.rs` row, marked diverged).

### What it does
1. **The right panel** (`panel/mod.rs`): a 40 px tab strip over the open tab. `Tabs` is the tab model: `Tab::ALL = [Diff, Files]`, Diff is
   always open, others appear when asked for (`show`) and the strip shows only open tabs, so CP6.2 adds its body and CP6.3 a `Tab` variant
   without touching the strip. The panel reads nothing while it is closed or on another tab (the diff has a `visible` flag the panel sets).
2. **The diff tab** (`panel/diff.rs`). Toolbar: three scope buttons (This turn, This thread, Working tree), a summary (`7 files +15 -8`, click
   hides the list), refresh, collapse or open all files, unified or side by side, wrap or scroll long lines, flat list or folder tree. Under it
   the changed files (a virtual `uniform_list`, 26 px rows, at most 7 rows then it scrolls): a status letter in a tinted box (A new, M
   modified, D deleted, R renamed, C copied, U conflicted), the path cut in the middle, `binary`, `+N -M` in two right-aligned columns; a
   click scrolls the diff to that file and opens it. Then GPUI Kit's `Diff` over a parsed patch: unified or split, syntax colours, word-level
   changes, hunk separators, line numbers, a collapse chevron per file, selection and copy, horizontal scroll or soft wrap, binary notes,
   rename headers. Each file's header has two buttons: open in editor and revert.
3. **Scopes.** *Working tree*: `review_patch("uncommitted")` = one `git diff HEAD` (renames detected, `-c core.quotepath=false`; the empty tree
   when there is no commit) plus each untracked file as an all-added diff (at most 100, each at most 1 MiB; the rest are counted and the panel
   says so). *This turn*: `review_patch("lastTurn", base)` with the commit `HEAD` was at when the newest live turn started (the thread model
   reads it in the background at `TurnStarted`); untracked files that were already there are dropped (a new file counts only when a card wrote
   it). With no base (a turn from the store) or a base that is gone: the working tree narrowed to the files the turn's edit cards (its footer)
   named, with a note; with nothing named, an empty list and a note. *This thread*: the runtime's own per-thread review (`Runtime::changes`:
   what this thread's `write_file` / `patch` calls touched, against the checkpoint taken at its first edit when checkpoints are on, else
   `HEAD`), turned into a patch (`patch_from_changes`); asking wakes the session in the runtime.
4. **Revert.** A file's revert button asks first: a bar at the bottom names the file and what will happen and has Cancel and a destructive
   button; nothing is done until it is pressed. *Working tree*: `git_review::revert` (checkout HEAD, clean): "Discard all changes to X? It goes
   back to HEAD (a new file is deleted). This cannot be undone." (labelled destructive); afterwards `git status` is asked about the file and a
   failure is reported (the core's `revert` swallows its own errors). *This thread*: `Runtime::revert_file`: back to before the thread's first
   edit, and the core takes a checkpoint first, so `/rollback` can undo it ("Revert file"). *This turn*: no revert (it would go to HEAD, not to
   before the turn); renamed and copied files are not offered one in the working tree (their new name is not in HEAD). The folder is
   resolved from the repository's root (`:(top,literal)path`), so a thread in a subfolder reverts the right file. After a revert the panel reads
   again, a toast says so, and the thread's header re-reads git. **The core has no hunk revert, so only whole files** (see "Missing core
   operations").
5. **Open in editor** (`Backend::open_in_editor`, `panel/backend.rs`): there is no editor setting in the core or Settings, so: Zed
   (`zed.exe` on the `PATH`), else VS Code (`code.cmd` / `code.exe`, `-g file:line`), else the system's default program through
   `hermes_core::open_file` (which shows an executable in Explorer instead of running it). The command is built by `data::editor_command`
   (pure, tested without launching anything). A file that is not on disk (a deleted one) is refused; its button is disabled. Phase 7 adds the
   setting.
6. **App actions.** *Open diff* (an edit card's button, the turn footer's file rows): the panel opens on the Diff tab; with a path, on This turn
   when the thread knows its turn (a base or footer files), scrolled to that file. *A file link* (`OpenFileInPanel`): the file opens in the Diff
   tab when it is one of the working tree's changes (scrolled to it); else the **Files tab appears with a placeholder** that names
   `path:line` and says "The file view arrives with CP6.2" (no toast; the Diff tab is one click away). A link that arrives before the
   changes are read waits for the read, then decides.
7. **The turn footer from git (CP4.2's "left for later").** At a live turn's end `ThreadModel::refine_footer` asks the backend for the turn's patch
   from the recorded base commit and replaces the cards' file list with `merge_turn_files`: git's tracked changes (including what a terminal
   command changed, which no edit card shows), a new file only when a card wrote it, files a card named that git does not list (ignored, or
   edited then put back: the cards cannot tell those apart) and git's counts. The edit-result sums stay as the footer's first value and as the
   fallback when there is no base (a thread read from the store), no repository, or the read fails. History footers still come from the cards.
8. **`hermes_core::git` kills a hung `git`** (CP4.3's "left for later"). `run_limited` (pipes read on their own threads, `try_wait` every 5 ms,
   kill at the limit, the pipes abandoned after 200 ms because a grandchild such as an `fsmonitor` daemon may keep them open): `git()` uses
   `GIT_TIMEOUT` = 30 s and answers `(false, "")` like a missing git (no behaviour change for the Tauri app but a hung process now dies);
   `git_review`'s `run()` uses it too (120 s for `gh`) and answers git's text "git X did not answer within 30 s and was stopped"; the new
   `git_timed` / `review_patch` return it as `Err`, which the diff tab shows with a "Try again" button.
9. **Refresh.** On open, on a scope switch, at a live turn's end (if on screen; else when it is shown), when the window comes forward (if the
   last read is over 2 s old), after a revert, and with the refresh button. One answer per request: a read that returns after a newer one
   is dropped.

### Tests (all `CARGO_TARGET_DIR=target/claude`)
- `scripts/native/check.ps1`: **"native check: all ok"** (build with `--features dev`, `cargo test -p hermes-native`: **514 passed, 2 ignored** (Phase 3's
  seed helpers), then with `--features snapshots`: 517 passed, 2 ignored, the snapshot test 292 s).
- New tests in `hermes-native` (37): `panel::data` (11: git patches to entries with every status and counts, no newline at the end, CRLF, an empty
  file, a 5,000-line hunk, a patch cut at a size limit, a file the parser refuses next to good ones, a card's path naming its changed file,
  `retain`, the runtime's per-thread changes as a patch, the editor command for Zed / VS Code / neither, programs found on a `PATH`, the tree,
  `merge_turn_files`), `panel::tests` (21: the read once and never while hidden, letters and counts, a failed read with its note and retry, a stale
  answer dropped, the layout at 640 / 1100 / 1920 (tab strip 40 px, toolbar, scope buttons, 26 px rows, one letter column, one counts right edge,
  `no_overflow`), paths and lines far too long wrapped and scrolling at two widths, the folder tree and closing a folder, each scope's file set
  through the buttons, This turn from a base commit and narrowed to the footer's files, the footer following git at a turn's end, the revert flow
  (nothing before the confirmation, cancel, confirm reaching the backend and the list, a failure toast, which scopes and kinds allow it), open in
  editor reaching the backend, `OpenDiffTab` and `OpenFileInPanel` (changed file in the diff, other file in the Files placeholder, a link before the
  read waits), the tab model, `collapse_plan`, collapse all, split / wrap / list toggles and a row opening its file, the 6,000-line diff),
  `panel::real_tests` (5, real `Core`, real git repositories in `hermes_testdir` folders, a wiremock agent: see TESTING.md §6). Two older tests were
  adapted to the new panel (`thread/tests.rs` wiring, `thread/tests/tools.rs` actions).
- **The live check** is `panel::real_tests::an_agents_edits_show_in_the_turn_footer_and_in_the_diff_tab_from_git` (the whole path on a real core and a
  temporary repository: edit card, new file, a file changed by a terminal command, the footer from git without the pre-existing untracked file, the
  Diff tab on This turn then Working tree, a revert only after its confirmation) plus `scripts/perf/native-thread-check.ps1` (below).
- **Performance** (debug build, headless, real text; `cargo test -p hermes-native --lib six_thousand -- --nocapture`): parsing a 6,000-line patch **7.5 ms**;
  installing it in Kit's diff and drawing the first frame **60 ms** on the UI thread; the following 35 frames (30 idle, 5 scrolled to a file) **mean 22 ms, max 23 ms**
  (a frame draws what is visible); Kit's syntax and word-level preparation **9.6 s** on the background executor (release: not measured). The panel
  opens collapsed: files over 1,000 changed lines, and everything after the first 5,000 (`collapse_plan`).
- `cargo test -p hermes_core`: **146 passed** (140 + 3 `git::tests` for the timeout: a hung child is killed at the limit, a fake `git` script whose helper
  holds the pipes still returns an error, a quick git is untouched; + 3 `git_review::tests` for `review_patch`: renames, deletes, binary and untracked
  files, a base commit, no commit, the caps). `cargo test -p hermes-desktop`: **130 + 68 passed**, the RPC-table fixture unchanged
  (the webapp's `dist-webapp` was copied from the main checkout's `target/hermes-src` for the build, as in earlier phases, and removed after).
  `cargo build --workspace`: passes.
- `cargo clippy -p hermes-native -p hermes_core --all-targets --no-deps`: nothing from this checkpoint (the Phase 5 composer files have redundant
  clones this toolchain reports; not touched). Files new here are `rustfmt`-formatted; other files are as they were.
- `scripts/perf/native-thread-check.ps1`: "native-thread-check: ok" (1,200 characters streamed and stored, equal), run after the ThreadModel changes (turn base read at a turn's start, footer refinement); no process or folder left.
- `scripts/clean-test-temp.ps1`: dry run 14 folders (4.3 MB), `-Delete` removed 14, second dry run 0.

### Snapshots
New (2), `crates/native/snapshots/`: `panel-diff-t3-light.png`, `panel-diff-t3-dark.png` (the panel in six states, each 380 x 620 as the app draws it:
`working` the working tree as a flat list with the first files open, `tree` the folder tree, `split` side by side, `confirm` a file's revert waiting for
its confirmation, `empty` This turn with the "No edits of the latest turn are known." note, `files` the Files tab's placeholder after a file link).
**Pre-existing baselines that changed (2):** `app-frame-t3-light.png`, `app-frame-t3-dark.png`, only in the right panel's area (the Phase 4 placeholder
text became the tab strip and "Open a thread to see its changes."; the frame story has no open thread). `UPDATE_SNAPSHOTS=1` rewrote every baseline
once: `git status` shows only these four paths, so nothing else changed. I looked at all six dark states and the light `working` state, and at the dark
`app-frame` crop. Compared with `sidebar-diff.png` / `sidebar-diff-content.png`: the tab strip, the status marks, the `+N -M` colours, line numbers and
the green added ground match in kind; differences are Kit's own (its file header with chevron and counts, the hunk header, word-level highlights) and
ours (the scope buttons, the letters in boxes); not pixel-compared. A Haiku check should look at the two new images and the two changed ones.

### Deviations
- **t3code shows the diff as an accordion** (each file a row that opens in place) with a tree behind a toolbar button; ours is GPUI Kit's diff view
  (the accordion: a header per file with a chevron) under a **file list** that is flat or a folder tree, so the changed-files tree and the diff are
  both always there; it is not t3code's inline layout. t3code has no scope switch; this one follows the brief.
- **No hunk revert** (the core has none), **no revert in This turn**, and **none for a rename** (above).
- **No syntax-colour or word-level preparation limit**: Kit prepares the whole patch on the background executor (a 6,000-line patch took 9.6 s in a
  debug build, the frames stay ~22 ms; the text is plain until it lands). The brief's "collapse huge files" is done on the UI side (files of
  over 1,000 changed lines, and everything past the first 5,000 changed lines, open collapsed); whether Kit skips preparing collapsed files is
  not known.
- **Wrap is the default** (as in t3code's screenshot); the toggle scrolls long lines instead (Kit's horizontal scroll). The scroll itself is Kit's
  and has its own tests; ours assert nothing overflows the panel in both modes.
- **Binary files** show Kit's own note plus git's extended header lines ("index ...", "Binary files ... differ").
- **The Files tab's placeholder** is a stand-in for CP6.2 (no toast: the panel is the answer).
- **No "+" new-tab button, no "Open a surface" chooser** (`sidebar-right.png`): nothing to open until CP6.2 and CP6.3.
- **The scope, view mode and wrap are per run** (not saved in `native-ui.json`); the panel opens on the working tree.
- **No keyboard shortcut** for the diff tab or the scopes (t3code has D for "Diff" in the chooser, which does not exist yet); the diff's own keys
  are Kit's (arrows, page up/down, Home/End inside the focused diff).
- **No t3code screenshot was taken** (t3code's window cannot be driven from here): the light theme, the scope switch, the confirmation, binary
  and huge-diff states follow the source and the tokens (`reference/t3code/README.md`).

### Missing core operations
- **Hunk revert** (`git apply -R` of one hunk, or per-hunk checkpoint restore): not in `hermes_core::git_review` nor the checkpoints (`Checkpoints::
  revert_file` is per file). Without it only whole files can be reverted.
- A **ranged/untracked-aware turn diff** with a base (the page's `lastTurn` has the same shape): `review_patch` was added for the panel; nothing else
  was needed.
- **No editor setting** (Phase 7).
- `git_review::revert` returns `Ok` whatever happened (its `run` results are dropped): the panel checks afterwards. It is shared with the Tauri
  app, so it was not changed.

### For the reviewer
1. **`Runtime::changes` / `revert_file` wake the session** (`Runtime::open`): asking for This thread loads the session (its tools, MCP...) in the
   runtime if it was only in the store. `revert_file` also needs the session idle: on a busy thread the toast says why. Paths in that scope are
   relative to the checkpoint's folder or the repository root, which the backend treats as the root (a thread in a subfolder of a repository
   that uses checkpoints is not tested).
2. **`review_patch` runs one `git` per untracked file** (`git diff --no-index`, up to 100): about 25 ms each on this machine, so a tree with
   100 loose files takes a few seconds on the background thread (never on the UI's). The list itself is one call.
3. **The footer's replacement is visible**: a live turn's footer first shows the cards' files and a moment later git's (the tree rebuilds,
   `remeasure`). It never changes a footer that was pushed by a replay (`replaying`) or a partial turn.
4. **`merge_turn_files` keeps a card's file that git does not list** (edit-then-revert and ignored files look the same to it). The alternative
   (drop it when `git ls-files` says it is tracked and unchanged) costs another git call per file.
5. **`DiffPanel::show_data` is not test-only code**: the gallery needs a panel that is already loaded (a story is drawn twice and cannot wait
   for a background read). It skips the Turn-scope filter that `refresh` applies (it is only used with the working tree and the narrowed turn).
6. **`RightPanel::open_file` decides from the *working tree's* changes**, whatever scope is showing: a link to a file changed only in This
   thread's scope (reverted by hand since) opens the Files placeholder. `set_scope_quietly(WorkingTree)` is called when nothing was read yet.
7. **The panel's visibility is the workspace's `right_panel.open`** (`Workspace::changed` and `with_model` set it); a panel that is open but
   too narrow to draw (the frame hides it under 280 px) still reads. Hidden panels read nothing (asserted).
8. **`run_limited` polls `try_wait` every 5 ms** and spawns two reader threads per call; at the 30 s limit it kills the process tree
   (review: it was only the child), then waits 200 ms for the pipes and abandons the reader threads if a grandchild still holds them. Every `git` the core runs
   goes through it (the Tauri page's git routes too): a long `git` that is legitimately slow (a huge repo's first `status`) is now cut at 30 s.
9. **The two real-core tests need Git Bash** (the agent test's `terminal` call, as the recorder's) and `git` on the `PATH`.
10. **Kit's diff prepares syntax for the whole patch** after `set_files` (9.6 s for 6,000 lines in a debug build, in the background): whether
    collapsed files are skipped is not checked. The frames in the meantime show plain text.
11. **`AppModel::backend()`** is a new public accessor (the panel asks through it, as a view must not hold the core).
12. **Pre-existing, not touched:** clippy (this toolchain) reports redundant clones in the Phase 5 composer files and `gallery/composer.rs`;
    `cargo fmt` would reformat many Phase 4/5 files. Mine are formatted; the others are as they were.
13. **Baselines:** `app-frame-t3-{light,dark}` changed only in the right panel's area (the placeholder text became the tab strip and
    "Open a thread to see its changes."); checked by looking at the dark crop.

### User checklist (CP6.1, the diff)
Build (PowerShell, in `C:\Users\alexandros\repos\hermes-rs-n6`): `$env:CARGO_TARGET_DIR="target/claude"; cargo build -p hermes-native --bins`, then
`$env:HERMES_RS_HOME="$PWD\target\native-test"` (a synthetic folder, never `%APPDATA%`). **Use a throwaway git repository, never your own, for the revert
steps (4, 5, 8)**: `git init` a folder with a couple of committed files, edit them, add the folder as a project, open a thread in it.
A. Recorded fixture, no backend: `.\target\claude\debug\hermes-native.exe --fixture`.
 1. Open "halo", press Ctrl+Alt+B (or the right-panel toggle): a panel with a "Diff" tab, three buttons "This turn / This thread / Working tree",
    "7 files +15 -8", the file list (letters A M D R, `binary`, counts), and under it the diff with a header per file.
 2. Click "docs/guide/intro.md" in the list: the diff scrolls to it. Click a file header's chevron: it collapses. The double chevron in the toolbar
    collapses or opens all. The next icon toggles side by side. The next scrolls long lines instead of wrapping them: open "data/long.json" and
    scroll sideways (hand-only: the feel). The last icon makes the list a folder tree.
 3. Click "This thread": three files. "This turn": "No edits of the latest turn are known." Back to "Working tree".
 4. Hover a file header: two small icons. Click the arrow (revert) on "Cargo.toml": a bar at the bottom asks "Discard all changes to Cargo.toml?
    ... This cannot be undone." Cancel: nothing. Ask again, "Discard changes": the file leaves the list and a toast says it was reverted.
    (The fixture only drops it from its fake patch; the real revert is item 8.)
 5. Click the editor icon on a file: a toast "Opened in the fixture editor" (nothing launches in the fixture).
 6. Open "Fix the sidebar width", click a link to a file in its reply (`workspace.rs`): the panel gets a "Files" tab saying
    "crates/native/src/workspace.rs:89" and "The file view arrives with CP6.2." Click "Diff": back.
 7. Click a file row in a turn footer ("Open diff"): the panel opens on the diff (the fixture has no turn base: "This turn" narrows to the file).
B. Real core on a throwaway repository (hand-only: the feel of a real window): run the mock provider as in Phase 4's checklist or a real model
   and ask it to change two files and create one:
 8. After the turn the footer lists the files with git's counts. Open the Diff tab, "This turn": the same files; "Working tree": also anything
    you changed by hand. Revert a modified file: it goes back on disk only after "Discard changes" (check in Explorer or `git status`);
    a new untracked file is deleted by "Discard changes" (that is the warning's "a new file is deleted"). In "This thread" the same button
    says "Revert file" and `/rollback` can undo it.
 9. If Zed or VS Code is on the PATH, the editor icon opens the file in it (at the top; line numbers are not passed yet); else your default app.
10. Switch to another window, edit a file in the repository, switch back: the list refreshes (within a couple of seconds).
Hand-only (no test can judge them): scrolling and selection feel in the diff (also drag-selecting across lines and Ctrl+C), the look of the
confirm bar and of hover states, a very large real diff, the editor actually starting, resizing the panel wider and narrower.

### Visual check (Haiku 5.5)
Opened: `sidebar-diff.png`, `sidebar-diff-content.png`, `sidebar-right.png` (t3code references, dark). All six dark and all six light panel-diff states viewed at full size; app-frame crops compared with the HEAD versions (pixel diff bounding box only).

- panel-diff-t3-dark.png: differs | vs the t3 references: tab strip, toolbar icons and +/- colours match in kind; the file list uses letter boxes (M, R, D, A) where t3 uses green "+" boxes, and it sits above one diff instead of t3's per-file accordion (documented deviation); the syntax palette differs (keywords blue here, red/orange in t3); no "+" tab button or surface chooser (documented, nothing to open until CP6.2/6.3).
- panel-diff-t3-light.png: expected | all six states legible, no clipping or overlap; status letters, tints and the confirm bar have readable contrast.
- app-frame-t3-dark.png: changed-as-expected | only x 3254-3833, y 119-720 (the right panel): the Phase 4 placeholder became the "Diff" tab strip and "Open a thread to see its changes."
- app-frame-t3-light.png: changed-as-expected | same bounding box, light theme, same change.

### Review (Opus 5.5)
Verdict: sound checkpoint; revert safety and the git timeout needed fixes, now done. `main` had not moved (no merge).

**Findings and fixes**
1. **Wrong file reverted from a subfolder (fixed, serious).** `review_patch` ran `git ls-files --others` in the thread's folder:
   untracked files were named relative to it (`sub/new.txt` as `new.txt`) while `git diff` names files from the top, and the
   revert's `:(top,literal)new.txt` aimed at the top's `new.txt` (another file: its changes discarded, or deleted if untracked).
   `review_patch` now runs everything from `--show-toplevel` (and lists the whole repository's untracked files). Tests: core
   `from_a_subfolder_every_path_is_named_from_the_repository_top`; real `a_thread_in_a_subfolder...` (an untracked `sub/b.txt`
   next to an edited, tracked `b.txt` at the top: only the shown file goes).
2. **Stale revert (fixed).** The diff could be older than the file (the agent or an editor wrote since); confirming discarded the
   newer content unseen. The patch's `index ..<blob>` line is kept per file (`Parsed::blobs`, `data::blob_of`); the confirmation
   carries it (`Confirm::seen`, `Backend::revert_file(.., seen)`); the backend first asks `git status` that the file is still
   changed and `git hash-object` that it is still that blob (`0000000`: still absent), else refuses with "nothing was done". A
   re-read that changes the confirmed file's blob, drops it, or is another scope removes the bar. Before, a path that matched
   nothing (a C-quoted name) "succeeded" because `git status` of nothing is empty: now refused. Tests: real
   `a_file_changed_after_the_diff_was_read_is_not_reverted` (modified, untracked with spaces and parentheses, deleted then
   recreated; contents on disk asserted); fixture `a_confirmation_goes_when_the_file_changes_before_it_is_confirmed`.
3. **Confirm gating verified:** nothing reverts without the bar's second button (elsewhere than the header icon, not focused, so a
   held Enter cannot confirm). `confirm_revert` also refuses while a revert is busy or when the scope is not the one the question
   was asked in; `set_scope_quietly` clears it. The text now says staged changes go too (`checkout HEAD` resets the index): the
   `confirm` state of `panel-diff-t3-{light,dark}` was re-accepted after looking (only that bar differs). Staged-only new files
   (no HEAD version) are reported "still changed", not deleted. Folders (submodules, nested repositories) are refused.
4. **"This thread" revert without its checkpoint (fixed, `hermes_tools::checkpoints::revert_file`).** The pre-revert snapshot's
   failure was ignored (`.ok()`, also inside `restore`). Now a snapshot is taken first in both branches and a failure (git error,
   or a folder past the file cap) aborts the revert; the test asserts one new checkpoint per revert.
5. **The git timeout killed only the child (fixed).** `run_limited` kills the tree with `%SystemRoot%\System32\taskkill.exe /T /F`
   (full path) while the child is alive, then `kill`/`wait`; also on a `try_wait` error. Test
   `a_timeout_kills_the_whole_process_tree` (a fake git whose PowerShell grandchild would write a file 3 s later; it fails without
   the tree kill). Pipes are drained on threads; `git()` unchanged in shape and normal behaviour; RPC-table fixture untouched.
6. **Syntax preparation (fixed).** Kit (`gpui-component` `diff/state.rs`, `ensure_presentation`) prepares every file it holds,
   collapsed or not. Files over `HUGE_FILE` and past `HIGHLIGHT_BUDGET` (2,000 changed lines) get `with_language("text")`, which
   Kit skips (`plain_plan`, tested). 6,000-line test: 8.0 s -> 3.3 s of background preparation (debug), frames unchanged (~22 ms).
   The palette is the app's `cx.theme().highlight_theme`, the same as the thread's code blocks; t3code's Pierre/shiki colours stay
   a deviation.
7. **Open in editor:** a relative `PATH` entry (`.`) would have started a repository's own `zed.exe`: relative entries are skipped
   (tested). Absolute paths and `..` are refused before anything starts (tested). The resolved absolute program is started with
   `Command` arguments (no shell; `code.cmd` uses Rust's batch-argument escaping). The fallback, `hermes_core::open_file`,
   reveals executables in Explorer and never runs them; file links never open or run anything.
8. `all_added` passes `--no-color --no-ext-diff` (a `color.ui=always` config or an external diff tool would have broken or run
   for it; shared with the Tauri page, harmless); `review_patch`'s `git diff` ends with `--`.

**Left, and why**
- **Hunk revert:** the core has none (`git_review::revert` and `Checkpoints::revert_file` are per file). `git apply -R` of one hunk
  needs a per-hunk button (Kit offers only the file header suffix) and its own stale checks: not contained here.
- Accordion: Kit's view already collapses per file; the file list above it stays (documented deviation).
- The footer's git list replaces the cards' a moment after the turn; the cards' order is kept first, so rows rarely move.
- A tiny race remains between the blob check and `git checkout` (a write in the same milliseconds). Release-build preparation
  time not measured. Asking for "This thread" wakes the session in the runtime (as noted).

Tests after the fixes (`CARGO_TARGET_DIR=target/claude`): `scripts/native/check.ps1` "all ok" (`hermes-native` 517 passed,
2 ignored; `--features snapshots` 520 passed, 2 ignored; `panel-diff` baselines re-accepted for the confirm text only);
`cargo test -p hermes_core` 148 passed; `cargo test -p hermes-desktop` 130 + 68 passed (RPC-table fixture unchanged; the webapp's
`dist-webapp` copied from the main checkout for the build and removed after); `hermes_tools --lib checkpoints` 4, `hermes_runtime
--lib changes` 6; `scripts/perf/native-thread-check.ps1` ok; `cargo build --workspace --locked` passes;
`scripts/clean-test-temp.ps1`: 8 folders removed, then 0; no process left under `target\claude`.

## CP6.2 — Files
Worktree: C:\Users\alexandros\repos\hermes-rs-n6   Branch: native/p6-panel   Model: Sonnet 5.5 (subagent)
Cut from CP6.1 (40e2b96). Uncommitted. The run was interrupted by a rate limit and resumed; the worktree was re-checked against the brief.

### Files
- New in `crates/native/src/panel/`: `fs.rs` (disk rules, no views: `locate` with the refusals `Refused::{Outside, Escapes, Sensitive, NotFound, NotAFile, TooLarge, ...}`, `list_dir` with git-ignored marks, `read_file` + `classify`, `index_files`, `Ignores` over the `ignore` crate's matcher), `tree.rs` (the tree as data: lazy loads, open folders, flat rows, keys), `files.rs` (`FilesPanel`: toolbar, virtual tree, tree/file modes, keys), `viewer.rs` (`FileView`: read-only code editor, markdown preview, SVG, pictures, binary, refusals, jump to line), `finder.rs` (`Ctrl+P`: chunked index, background ranking, dialog), `fixture_files.rs` (virtual or on-disk fake project), tests `files_tests.rs`, `finder_tests.rs`, plus a real-core test in `real_tests.rs`; new story file `gallery/panel_files.rs` (`panel-files`, `file-finder`).
- Edited: `panel/mod.rs` (Files is a tab that is always open; the placeholder, `file`, `waiting`, `show_changed` are gone; `open_file(file, window, cx)`, `view_diff`, `show_tab`, visibility per tab), `panel/diff.rs` (`working_tree_has`), `panel/backend.rs` (`project_list/read/index/open`; the editor launcher shared with CP6.1's), `model/backend.rs` (4 trait methods; Core and Fixture impls), `workspace.rs` (finder layer, `open_finder`, `open_file_in_panel` takes the window, panel events with window), `keymap.rs` (`files.find`, `Ctrl+P`), `app.rs` (binds), `palette.rs` (`highlighted` is `pub(crate)`), `thread/markdown.rs` (`preview`, `preview_link`), `design/icons.rs` (FileCode, FileImage), `testing.rs` (scrolling lists exempt from `no_overflow`), `gallery/panel.rs` (the `files` state moved out), `Cargo.toml`/`Cargo.lock` (`ignore`, already in the lock). Old tests adapted: `panel/tests.rs` (two tabs, the file-link test, the tab model; "link before the read waits" removed: the Files tab needs no diff read), `thread/tests.rs`.
- **Core (`hermes_core`): unchanged.** Docs: AGENTS.md, ARCHITECTURE.md, TESTING.md, PITFALLS.md (3 bullets), PHASES.md status, native-ui/README.md row, reference/t3code/README.md (no screenshot). PORTING.md: no row (no Hermes port; `fs.rs` reuses the core's sensitive-path check).

### What it does
1. **Tree.** Root = the open thread's folder. Only the root is read when the tab is first on screen; a folder when opened (background task); ignored folders (`.gitignore` from the repository top down, nested files with negation, `.git/info/exclude`) are dimmed, listed on demand as ignored, never read until opened; a listing is cut at 10,000 entries with a "N more" row. Keys: Up/Down/Home/End, Left/Right, Enter/Space. Refresh (button, window forward, turn end) re-lists open folders and keeps rows; collapse all; thread switch starts over.
2. **File view.** Replaces the tree with a back button (stacked, not side by side: deviation). Source = Kit's read-only code editor (syntax, line numbers, horizontal scroll, Ctrl+F); markdown rendered with a Source switch (links: web ones open, relative ones open that file resolved from the file's folder, the rest nothing); SVG as a picture with a Source switch; PNG/JPEG/GIF/WebP/BMP/TIFF/ICO decoded and shrunk (over 2048 px) in a background task and drawn from a renderer image (animated GIFs show frame 1); binary: "Binary file, N bytes" + Open in editor; non-UTF-8: replacement characters + a note; over 2 MB: plain text + note; over 20 MB: refused with the editor offered. `path:line` selects the line and scrolls it into view (4 rows of context).
3. **Refusals** (message, no editor offer except too-large): outside the folder (also `..`, absolute, `~`), a link/junction that leaves, credentials (`is_sensitive` on the asked and the resolved path), a folder, missing. Open in editor checks the same rules, then CP6.1's launcher.
4. **Finder** `Ctrl+P` (and the toolbar field): index built by a background walk in batches of 2,000 (ignored folders, `.git`, links, credential files left out; cap 200,000), ranked on the background executor (byte filter, cheap score for the best 400, the palette's matcher for the final order and bold positions, a whole-word bonus), 100 results, virtual list; Enter opens at no line, Esc closes, same overlay place as the palette.
5. **Links.** `OpenFileInPanel` (timeline, edit cards, finder) opens the Files tab on the file; when the diff has read the working tree and the file is changed, the header shows "Diff" (Diff tab on it).

### Tests (`CARGO_TARGET_DIR=target/claude`, run from the worktree root)
- Results of the final runs are in the last block of this section ("Final runs").
- New tests: `fs` 14 (listing, nested ignores + negation, exclude file, subfolder of a repo, cut listing, index limits, `..`/outside/absolute/`file:`, credentials, links/junctions (made with `mklink /J`, no rights needed), classification, 2 MB / 20 MB, the 100,000-file tree), `tree` 5, `finder` 6, `fixture_files` 1, `markdown` 1 (preview links), `files_tests` 17, `finder_tests` 4, `real_tests` 1 (real `Core`: list/read/index/refusals/open for a thread in a repository's subfolder).
- **100,000-file tree** (1,000 folders, empty files, debug build): the `fs` test prints the timings (`cargo test -p hermes-native --lib panel::fs::tests::indexing -- --nocapture`); asserted bound 20 s. Ranking 100,000 synthetic paths (`finder` test): bound 3 s per query, timings printed. Over the UI, 20,000 real files: indexed while typing, a two-letter query ranked in the printed time, results capped at 100.
- A 2.4 MB, 40,000-line file opens as plain text and jumps to line 30,000 (time printed by its test, bound 30 s).
- Not asserted (headless limit): that the picture decode runs on another thread; the test platform runs background tasks on the test's own thread. The code path is `background_spawn`.

### Snapshots
New (4): `panel-files-t3-{light,dark}.png` (7 states: tree, code with line 5, markdown, image, svg, binary, refused), `file-finder-t3-{light,dark}.png` (results, none, indexing). **Pre-existing baselines that changed (6):** `app-frame-t3-{light,dark}` (right panel: two tabs), `palette-t3-{light,dark}` (a new "Go to file  Ctrl+P" row), `panel-diff-t3-{light,dark}` (the `files` placeholder state removed: 5 states). I looked at the dark `panel-files` and `file-finder` sheets: layout and colours are as intended. The light ones and the three changed pairs were not inspected here (for the Haiku check).

### Deviations
- Tree and file share the panel by turns (back button), not t3code's tree column beside the file; no resizable column; no git dots on folders, no context menu, no drag-to-mention; four generic file icons instead of per-type colour icons.
- The finder re-indexes each time it opens (no cache). The user's global git excludes are not read.
- Files is always in the tab strip (no "+" chooser exists).
- No t3code screenshot was taken (cannot drive its window): the file view, finder, markdown, picture and light themes follow the source and tokens.
- Core operations that do not exist (implemented in `panel/fs.rs` instead): a project listing with ignore marks, a bounded classified read, a finder index. `files::read_text` caps at 512 KB, `paths::list_dir` is not ignore-aware or root-bound, `looks_binary` is private (copied: 10 lines).

### For the reviewer
1. **Where `fs.rs` lives.** Native, not `hermes_core`; moving it to the core is mechanical if another front end needs it.
2. **`locate` is the security core**: canonical both sides, `starts_with` on canonical paths, `Escapes` vs `Outside` decided lexically. Please look for TOCTOU (a file swapped for a link between the check and the read) and Windows oddities (8.3 names, verbatim prefix, case); `plain()` strips the verbatim prefix before the editor launch.
3. **Kit's editor as a viewer**: `EditorState` read-only, `set_scroll_offset` (deferred to the next layout) for the jump, row height assumed 1.5 x the mono size (a test asserts the line is among the laid-out rows). 40,000-line plain file performance: see the test's printed time.
4. **A jump to a line on a markdown file shows the source** (the line is only meaningful there).
5. **`RightPanel::view_diff` sets the diff scope to the working tree** (as CP6.1's link did); the "Diff" button needs the working tree to have been read, no read is started for it.
6. **Finder keys**: `Ctrl+P` inside the finder is "previous" (as in the palette); the global `Ctrl+P` toggles it only when focus is outside the dialog.
7. A tree refresh keeps stale rows for a deleted folder until its listing fails (shown as an error row).
8. `Tree::is_ignored` only reads the folder's own flag; children of an ignored folder are flagged by the inherit rule at listing time.
9. A click on a link in rendered markdown dispatches the window action `OpenFileInPanel`; no test clicks it (the link rules are unit-tested).
10. Pre-existing and untouched: clippy notes and unformatted Phase 4/5 files; mine are `rustfmt`ed.

### User checklist (CP6.2, the files)
Build as in CP6.1 (`target/claude`, `HERMES_RS_HOME=target\native-test`); use a throwaway folder, never `%APPDATA%`.
A. `hermes-native.exe --fixture`: open "halo", Ctrl+Alt+B.
 1. Click the "Files" tab: a tree (assets, crates, ... README.md); `node_modules`, `target` and `debug.log` are dim; `outside` has a lock.
 2. Click a folder: it opens; click a file (`src/main.rs`): the file with line numbers and colours; the arrow goes back to the tree with the file selected.
 3. Up/Down/Left/Right/Home/End/Enter on the tree move and open (focus the tree by clicking a row).
 4. `README.md`: rendered, Preview/Source switch; click a relative link ("the guide"): that file opens.
 5. `assets/logo.png`, `logo.svg`, `data.bin`, `docs/old.txt`, `data/huge.sql`, `.env`: a picture, a vector picture with Source, "Binary file, 2,048 bytes", a note about replacement characters, a "34.0 MB, files over 20.0 MB are not opened" message with Open in editor, "holds credentials".
 6. Ctrl+P: type "main": `src/main.rs` first, bold letters; Enter opens it; Esc closes; the "Search files" field does the same.
 7. In "Fix the sidebar width" click `workspace.rs` in the reply: the Files tab opens on `crates/native/src/workspace.rs`.
B. Real core, a throwaway folder with a `.gitignore`: the same on real files; the editor icon opens Zed or VS Code (or the default app) at the line; a changed file shows a "Diff" button (after the Diff tab has been read).
Hand-only: scrolling and selection feel in the editor, a huge real repository (`node_modules` open, Ctrl+P on 100,000 files), the light theme, dragging the panel wider.

### Final runs
- `scripts/native/check.ps1`: "native check: all ok" (build, `cargo test -p hermes-native`, then with `--features snapshots`, baselines as listed above).
- Timings (debug build, this machine): index of 100,000 real files first batch 17.5 ms, all 708 ms (50 batches); ranking 100,000 paths: "f" 66 ms, "fi" 73 ms, "src rs" 163 ms, no match 13 ms; UI over 20,000 real files indexed in 878 ms, a 2-letter query ranked in 206 ms; a 2.4 MB / 40,000-line file opened, jumped to line 30,000 and drawn in 592 ms.
- `cargo test -p hermes-native --lib panel::real_tests`: 7 passed (incl. the new Files test). `hermes_core` is unchanged, so its tests and `hermes-desktop`s were not rerun. `cargo build --workspace` passes (the webapp dist copied from the main checkout for the build and removed after). `scripts/perf/native-thread-check.ps1`: ok. `scripts/clean-test-temp.ps1`: 13 folders removed, then 0.

### Visual check (Haiku 5.5)
References opened: `docs/native-ui/reference/t3code/sidebar-filetree.png` and `sidebar-right.png` (for the dark new sheets). Previous versions of the changed sheets: `HEAD` copies, compared by eye.
- panel-files-t3-dark: differs vs sidebar-filetree | tree is a separate state, not a column beside the file (documented); selected row is a grey fill, not t3's blue outline; file icons are generic, no git dots (documented). Otherwise expected: chevrons and indent, dimmed ignored rows, lock on `outside`, gutter numbers, syntax colours, markdown, picture, SVG, binary and refused states legible, no clipping beyond the expected code scroll.
- panel-files-t3-light: expected | legible and readable; dimmed `debug.log` and `node_modules` are low-contrast grey on white but still readable.
- file-finder-t3-dark: expected | input, bold match in `main.rs`, selected row, "18 files" and the Enter/Esc hint, no-match and indexing states all visible. No t3 finder reference exists in the set.
- file-finder-t3-light: expected | same layout, legible.
- app-frame-t3-dark: changed-as-expected | right panel now shows Diff and Files tabs. Versus sidebar-right: differs, no "+" tab button and no "Open a surface" chooser (documented).
- app-frame-t3-light: changed-as-expected | Diff and Files tabs; legible.
- palette-t3-dark: unexpected change | "Go to file  Ctrl P" row added as intended, but the "Toggle right panel  Ctrl Alt B" action row is gone from the list and from the "tog" search (present in HEAD).
- palette-t3-light: unexpected change | same as dark: "Toggle right panel" row missing.
- panel-diff-t3-dark: changed-as-expected | the `files` placeholder state (`workspace.rs:89`, "The file view arrives with CP6.2") is removed; 5 states remain.
- panel-diff-t3-light: changed-as-expected | same removal; legible.

### Review (Opus 5.5)
Verdict: a good checkpoint whose path rules needed hardening for Windows; fixed. `main` had not moved (no merge).

**Findings and fixes**
1. **Sensitive-path parity (fixed, core).** The Tauri preview's `files::is_sensitive` (Hermes's guard: `.env`, `auth.json`,
   `mcp-tokens`...) is narrower than what the runtime refuses: `id_rsa`, `*.pem`, `*.key`, `.netrc`, `.npmrc`, shell profiles
   (`redact::is_secret_file`), `.ssh`/`.aws`/`.gnupg`/`.kube`/`.docker`/`.azure`/`.config/gh|gcloud` (the `@file:` rule), and
   `.git/config` (a token in a remote URL) was readable. New `hermes_core::files::is_private` = all of those + `.git`, used by
   `fs.rs` (locate, the finder's walk) and the fixture; `is_sensitive` (and so the page's routes) is unchanged. The app's
   data folder is kept out of any project that holds it (`fs::keep_out(core.data_dir())`, not when the project is inside it).
   Tests: core `a_project_browser_also_keeps_out_...`; fs `keys_credential_folders_and_git_internals_...`,
   `a_kept_out_folder_...`; the real-core test reads an `id_ed25519` inside the folder (refused).
2. **Network paths were touched (fixed, serious on Windows).** `locate` canonicalised before checking containment, and
   `list_dir` canonicalised every link: a `\\host\share` link in a message, a `file:////host/...`, or a symbolic link in a
   cloned repository pointing at a share would make Windows connect and send the user's sign-in (NTLM). Now a path must be
   lexically below the folder (names without case; as given or as the disk spells it, 8.3 included) before any disk call;
   links on the way are `read_link`ed, recursively (16 deep), and only followed when they stay inside; `.gitignore` and
   `.git` are looked at without following links. Then canonical + `starts_with` as before (Escapes). Tests:
   `windows_path_forms_that_lead_elsewhere_...` (UNC, `\\?\UNC`, `\\.\pipe`, `\\.\C:`, `C:foo`, `..`, all Outside in < 2 s;
   other case and the verbatim form of an inside file fine), `a_link_to_another_machine_...` (skipped here: no symlink rights).
3. **Windows names (fixed).** `.env::$DATA` / `a:stream` (an alternate data stream: bypassed the name check), `CON`,
   `nul.txt`, `COM1`, trailing dots/spaces are refused before the disk (`odd_name`); the canonical re-check covers 8.3
   names. Test `names_windows_reads_as_...`.
4. **TOCTOU and bounded read (fixed).** `read_file` stat-checked a path and then `fs::read` it again. Now it opens once,
   checks the handle (`GetFileType` = disk, so no device or pipe; `GetFinalPathNameByHandleW` still inside and not private),
   stats the handle, and reads `take(OPEN_MAX + 1)` (a growing log is refused at 20 MB, a sparse file by its length first).
   What remains: "Open in editor" hands a checked path to another program, which opens it again (inherent).
5. **Palette row (fixed).** Not a cap or a cfg: the gallery story shows the first five actions, and "Go to file" was the
   third command, pushing "Toggle right panel" out. `files.find` moved after the panel toggles; the `palette-t3-*`
   baselines are HEAD's again. Test `the_actions_are_listed_in_their_order_and_tog_finds_both_panel_toggles`.
6. **A 5 MB minified file froze the UI ~11.5 s (debug)** laying out its one line on the UI thread. Lines over 10,000 bytes
   are now cut (in `classify`, off the UI thread) with a note; 0.6 s. Markdown and SVG over 2 MB are shown as text (they
   were rendered regardless of size). UTF-8 BOM dropped. Test `a_five_megabyte_single_line_...` (timing printed).
7. **SVG (fixed).** Rasterised by GPUI (usvg/resvg, at most 8192 px a side, on GPUI's background executor; no webview, no
   script, no HTTP), but usvg reads `<image href="C:\...">` or `\\host\...` from disk: every `href` that is not `#...` or
   `data:` is emptied first (`viewer::svg_without_references`, tested incl. entity-hidden values). The picture is made once
   per file, not copied and hashed on every frame.
8. **Raster decode:** off the UI thread (code path `background_spawn`), 256 MB decoder allocation cap, shrunk to 2048 px;
   now also asserted deterministically: right after `show_read` of a 4000x3000 PNG, inside the same UI update, nothing is
   decoded; one decode follows on the task.
9. **Markdown images** are never fetched: GPUI's default `NullHttpClient` is the app's (none is installed); local images do
   not load either (documented in PITFALLS). Links follow the thread's rules (tested by the implementer).
10. **Names drawn escaped:** control and bidi-override characters (`invoice\u{202E}gpj.exe`) are drawn as U+FFFD, one for
    one, in the tree, the finder (highlight positions still fit) and the view's header (`fs::shown_name`, tested).
11. **Finder focus:** closing (Escape, a click outside, `Ctrl+P` again) gives the focus back where it was (test
    `closing_the_finder_gives_the_focus_back_where_it_was`); opening a file leaves it with the panel.
12. Selected row: t3code's tree selection is a 12% `currentColor` fill (`pierre-tree-theme.ts`,
    `--trees-selected-bg-override`), which ours matches; the outline in `sidebar-filetree.png` is Pierre's keyboard
    focus ring. Left as is.

Checked and fine: views never touch the disk or the core (only `Backend::project_*` on `background_spawn`); `fs.rs` is
called from the backends only; tokens, no raw colours; `ignore` was already in `Cargo.lock`; no store change; tests use
`hermes_testdir`; the finder's walk stops when the finder closes (the channel closes); ranking prefers basename matches.

**Left, and why**
- `Ctrl+P` is a global binding: CP6.3's terminal must take it in its own key context so the shell gets it (noted in
  `keymap.rs`).
- The finder re-indexes on each open (0.7 s for 100,000 files, off the UI thread): acceptable; no cache.
- A listing reads a whole folder before cutting at 10,000 (memory ~ names only). Markdown parse and the editor's rope are
  built on the UI thread for files up to 2 MB / 20 MB (measured: 0.6 s for 2.4 MB debug).
- UTF-16 files are shown as binary. The user's global git excludes are not read (documented).
- Junctions / links whose target is spelled a third way (not the session's spelling nor the canonical one) count as
  leading out (shown with the lock).
- Tree as a separate state, generic icons, no git dots, no "+" tab (documented deviations / CP6.3).

Tests after the fixes (`CARGO_TARGET_DIR=target/claude`): `scripts/native/check.ps1` "native check: all ok" (lib tests
and `--features snapshots`; the `palette-t3-*` baselines are HEAD's again and match, no other baseline changed by the
review); `cargo test -p hermes_core` 149 passed; `cargo test -p hermes-desktop` 130 + 68 passed (RPC-table fixture
unchanged; the webapp's `dist-webapp` copied from the main checkout for the build and removed after);
`cargo build --workspace --locked` passes; `scripts/perf/native-thread-check.ps1` ok; `scripts/clean-test-temp.ps1`:
5 folders removed, then 0; no process left under `target\claude`.

## CP6.3 — Terminal
Worktree: C:\Users\alexandros\repos\hermes-rs-n6   Branch: native/p6-panel   Model: Opus 5.5 (subagent)
Cut from CP6.2 (b5abf69). Uncommitted. The run was interrupted by a rate limit while writing the view tests and resumed;
the worktree was re-checked against the brief.

### Files
- New `crates/native/src/terminal/` (~6,200 lines with tests): `mod.rs` (actions, `bind`, `RESERVED_IN_TERMINAL`,
  `passed_through`), `emulator.rs` (`Emulator` over `alacritty_terminal::Term` + `vte` `Processor`: feed, replies,
  title, bell, OSC 52, resize, scroll, selection, `Screen` = resolved cells rebuilt from the grid's damage),
  `colors.rs` (`TermColors`: theme tokens + t3code's 16 per light/dark, a Zed theme's `terminal.ansi.*`, xterm cube and
  greys, OSC overrides), `input.rs` (keys, paste, mouse reports as bytes), `links.rs` (http(s) and `path:line`),
  `boxdraw.rs` (box-drawing and block characters as rectangles), `session.rs` (the `Pty` seam, `CorePty` /
  `spawn_core`, `Shared`, the pump), `view.rs` (`TerminalView`: painting, keys, mouse, IME, clipboard, resize, bell,
  blink, the "lines below" indicator, `script` for stories), `group.rs` (`TerminalGroup` = a thread's terminals in one
  surface, `TerminalHub`), `fixture.rs` (`FixturePty` scripted shell, `colours_demo`, `tui_demo`, `numbered_lines`);
  tests `tests.rs` (scripted session, 17), `view_tests.rs` (GPUI, 17), `real_tests.rs` (real core, 2), plus unit tests in
  `colors`, `input`, `links`, `boxdraw`, `fixture`; new story file `gallery/terminal.rs` (`terminal`, `panel-terminal`).
- Edited: `panel/mod.rs` (`Tab::Terminal`, `Tab::CHOOSER`, the strip's "+" button and "Open a surface" chooser with
  T/F/D, a closable Terminal tab labelled by its terminal, the panel's own `TerminalHub`), `workspace.rs` (the drawer
  holds the open thread's terminal group; focus follows open/close; `drawer_terminals()`), `model/backend.rs`
  (`Backend::terminal_spawn`, `terminals_close_all`; `CoreBackend` holds an `Arc<Terminals>` (`with_terminals`);
  `FixtureBackend` makes `FixturePty`s: `ptys()`, `fail_terminals`, `quiet_terminals`), `app.rs` (`terminal::bind` after
  the global bindings; one `Terminals` shared with the core backend, `close_all` on `on_app_quit` and again after the
  event loop), `lib.rs`, `gallery/mod.rs`, `Cargo.toml` (`alacritty_terminal = "=0.26.0"`, no default features),
  `Cargo.lock` (7 packages added, nothing else changed: see Deviations).
- **Core (`hermes_core`):** `terminal.rs`: `Shell::pid`; ending a shell (`remove`, `close_all`, the idle reaper) kills
  its process tree (review: `kill_descendants`, replacing the implementer's `taskkill /T /F`; see Review), then
  the shell, then drops the pseudo-console (`master` is now an `Option`: closing it ends console-attached processes and
  lets the reader thread finish); kills happen outside the shells lock; `Drop for Terminals` closes what is left. Test
  `ending_a_shell_ends_the_programs_it_started` (a hidden `ping` in a console of its own: it **fails without the tree
  kill**, checked once by taking it out). `crates/desktop` is untouched (its socket uses the same `remove` / `close_all`).
- Docs: AGENTS.md, ARCHITECTURE.md, TESTING.md §6 (the terminal paragraph), PITFALLS.md (5 bullets), PHASES.md status,
  native-ui/README.md rows, PORTING.md (`terminal.rs` row: the tree kill is ours), vendor/SOURCES.md (alacritty_terminal).

### What it does
1. **Emulator off the UI thread.** The pump (`session::pump`, a GPUI background task) attaches to the shell (the core's
   per-shell broadcast), takes what is waiting up to 1 MiB per batch, parses it 64 KiB per hold of a `FairMutex`
   (`lock_unfair`), writes the emulator's replies back (ConPTY's `ESC[6n` at start, DA, OSC colour queries, size
   queries), sets a dirty flag and wakes the view once (`Notify`: one permit however many batches). The view takes the
   lock fairly when it draws and rebuilds only the rows the grid's damage names (all after a scroll, a resize or a theme
   change). Drop to latest: frames draw the newest state; the emulator parses every byte.
2. **Backpressure policy.** The core's channel holds 1,024 chunks of at most 16 KiB; if the pump falls further behind
   (`Lagged`), the skipped bytes cannot be parsed: the pump resets the emulator and replays the shell's 1 MiB history
   (`Shared::resyncs`). Memory is bounded by the scrollback (10,000 lines, ~29 MB of cells at 120 columns) plus the
   channel and the history.
3. **Renderer** (`view.rs`): canvas `terminal_background`; background runs; selection (`terminal_selection`; double-width
   characters selected whole); cursor (block with its character drawn in the cell's background, beam, underline; an
   outline when unfocused; blinks only when the program asks, DECSCUSR); text per row: runs of single-width cells
   shaped as one line with `force_width` = the cell width (every glyph on the grid), bold / italic fonts, underline
   (single, double and curly drawn as underline / wavy), strike; dim, inverse and hidden resolved into colours; 256 and
   true colour; double-width (CJK, emoji) characters centred on two cells; box drawing and blocks as rectangles; OSC 8
   links; Ctrl-hovered links underlined; the IME's composition drawn at the cursor; the bell's flash (150 ms;
   `BellStyle::None` exists, no setting yet). Font: the theme's monospace (Consolas, else Cascadia Mono) at t3code's
   12 px; cell = advance of `m` x max(ascent + descent, 1.2 lines), whole pixels; fallbacks Cascadia Mono, Segoe UI
   Emoji / Symbol, MS Gothic, Malgun Gothic.
4. **Size.** cols / rows from the element's bounds minus 6 / 4 px padding; the emulator resizes at once (Alacritty
   reflows wrapped lines), the shell is told at once the first time and then after 100 ms of a stable size.
5. **Keys** (decision below). Text goes through the input handler (IME). Typing scrolls to the bottom and drops the
   selection. Application cursor keys when the program asks.
6. **Mouse.** Programs that ask get reports (X10 / SGR, press / release / drag / motion / wheel, modifiers); Shift
   overrides. Otherwise: drag selects, double click a word (Alacritty's semantic characters), triple a line,
   Shift+click extends. The wheel scrolls the scrollback, or sends arrows on the alternate screen (alternate scroll).
   Shift+PageUp/PageDown a page, Ctrl+Shift+Home/End the top / bottom; a "N lines below" pill (click: bottom).
7. **Links.** Ctrl+click: `http(s)://` opens in the browser (`cx.open_url`; nothing else is opened), a file path
   (`a/b.rs:12:5`, `a\b.rs:40`, `a.txt(7,3)`, `README.md`) dispatches `OpenFileInPanel` (the Files tab and its rules:
   inside the thread's folder, no credentials). OSC 8 links are not followed (review). Nothing is ever run.
8. **Surfaces.** The drawer (`Ctrl+J`) shows the open thread's group; the right panel's "+" opens t3code's "Open a
   surface" (Terminal T, Files F, Diff D; Esc closes) and the Terminal tab has its own group (t3code keeps drawer and
   panel terminals apart). One terminal: t3code's floating toolbar (new, close) top right. Several: t3code's 144 px
   list (22 px header with new / close, 24 px rows after the review, a close per row), labels = OSC title or "Terminal N". Empty: "No terminal
   sessions for this thread yet." + New terminal. At most 8 per thread per surface (the core's 16 shells overall).
9. **Lifetime.** Terminals belong to (thread, surface); switching threads, hiding the drawer or closing the Terminal tab
   keep the shells running (output still parsed). `exit` removes the terminal; the trash button, Ctrl+Shift+W and a
   row's x kill the process tree; a `TerminalView` dropped without closing closes its shell on a thread; at quit the
   app calls `Terminals::close_all` (`on_app_quit`, and after the event loop), and `Drop for Terminals` covers the rest.
10. **Shell and environment.** The core decides: PowerShell 7, Windows PowerShell, `COMSPEC`, cmd (`shell_spec`), in
    the thread's folder (refused if the folder is gone: the core would fall back to home). Environment: the app's own
    minus `NO_COLOR` / `FORCE_COLOR` / `COLORFGBG`, plus `TERM=xterm-256color`, `COLORTERM`, `TERM_PROGRAM(_VERSION)`,
    `HERMES_DESKTOP_TERMINAL`. Checked: neither the core nor the native app sets an environment variable (provider keys
    live in Settings and the keyring; `set_var` appears only in tests), so nothing secret is injected. Kept as is: the
    user's own variables are theirs, as in any terminal beside the app (scrubbing as the agent's shell does would also
    change the Tauri app's terminal).

### Reserved keys (decision)
While a terminal has the focus the app keeps **`Ctrl+J`** (the drawer: t3code's `terminal.toggle` has no
`!terminalFocus`), **`Ctrl+Alt+B`** (right panel, idem), **`F6` / `Shift+F6`** (leave the terminal by keyboard) and
**`Ctrl+Shift+P`** (all actions; Windows Terminal's palette key). Every other global binding reaches the shell: `Ctrl+K`,
`Ctrl+P`, `Ctrl+B`, `Ctrl+N`, `Ctrl+,`, `Ctrl+Alt+Shift+A`, and GPUI Kit's `Tab` / `Shift+Tab` / `Ctrl+Shift+I`
(`NoAction` in the `Terminal` context; `terminal::bind` must run after `keymap::bind`). **Deviation from t3code:** t3code
keeps `mod+b` (sidebar) in the terminal; `Ctrl+B` goes to the shell here (readline, tmux's prefix), as the brief asks.
The terminal's own keys follow Windows Terminal: `Ctrl+Shift+C/V`, `Ctrl+C` copies only with a selection (and drops it),
`Ctrl+V` pastes, `Shift+PageUp/Down`, `Ctrl+Shift+Home/End`, `Ctrl+Shift+T` new, `Ctrl+Shift+W` close (t3code's
in-terminal `mod+n` / `mod+w` are shell keys on Windows). With `Ctrl+J` reserved the keyboard cannot send a bare LF
(Enter sends CR; as in t3code).

### Tests (`CARGO_TARGET_DIR=target/claude`, from the worktree root)
- `terminal::tests` (scripted session, no window): SGR 16 / 256 / true colour (fg and bg), bold / italic / underline /
  double / curly / inverse / dim / strike / hidden, CUP / CUB / CUD / CUF / EL 0 and 1 / ED 2 / DECSC-DECRC / ICH / DCH,
  wide and emoji characters (spacers, a wide character wrapping whole), the alternate screen (primary kept), a scrolling
  region (DECSTBM, reverse index), a full-screen program's screen (`tui_demo`: box drawing, inverse status bar, SGR mouse
  modes, cursor addressing), resize with reflow and scrollback, replies (DSR `ESC[6n` -> `ESC[1;3R`, DA, OSC 10 query),
  titles OSC 0 / 2, bell, OSC 52 (refused both ways after the review), selection text (wrapped line joined, trailing blanks dropped, wide
  characters whole, word, line), damage-only rebuilds, theme change, cursor shapes, the colours demo, and three volume
  tests. Plus unit tests in `colors` (2), `input` (4: keys, Ctrl/Alt, paste incl. a pasted `ESC[201~` removed, mouse X10 /
  SGR), `links` (2), `boxdraw` (1), `fixture` (1).
- **Numbers** (debug build, this machine, `cargo test -p hermes-native --lib terminal::tests -- --nocapture`):
  1,000,000 lines (12.9 MB) parsed in **1.00 s (12.9 MB/s)**; scrollback held at **10,000 lines, grid 28.9 MB** (the
  bound); pump test: 200,000 lines (2.5 MB) through the fixture channel in **0.22 s** while a "frame" took the lock every
  16 ms: longest wait for the lock **5.4 ms**, 5 wake-ups for 12 frames (coalesced); a 3,000-chunk flood before the pump
  read anything: **1 resync**, the screen ends with the last chunk.
- `terminal::view_tests` (17, workspace over the fixture): drawer at 640 / 1100 / 1920 (one shell started, prompt drawn,
  toolbar 8 px from the top right, grid origin = terminal origin + padding, cols / rows = floor of the padded size over
  the cell, whole-pixel rows, emulator and shell at that size at once, `no_overflow`); a drawer drag resizes the grid at
  once and the shell only after 100 ms, a TUI then draws at the new size; keys (typed text, Ctrl+P / Ctrl+K / Ctrl+B /
  Ctrl+N / Tab / Shift+Tab / arrows / Alt+B / Ctrl+Left / Esc / Backspace reach the shell as their bytes and no app action
  runs; DECCKM; Ctrl+J toggles the drawer and sends nothing; the shell keeps running); the reserved / passed-through sets
  cover `keymap::COMMANDS`; Ctrl+C (^C without a selection, copies and drops it with one; Ctrl+Shift+C keeps it; double
  click word, triple click line, Shift+click extends; typing drops the selection); paste plain and bracketed; IME
  composition drawn and not sent, committed text sent, candidate window at the cursor; mouse reports in a full-screen
  program (click press / release in SGR, wheel 64, Shift selects instead, `q` leaves); wheel / Shift+PageUp /
  Ctrl+Shift+Home / output while scrolled up / typing jumps to the bottom / the indicator's click; Ctrl+click on
  `src/main.rs:5:1` opens the right panel's Files tab on it, Ctrl-hover marks the link's cells; several terminals (the
  144 px list, 24 px rows (review), titles, row click, another thread's own terminal, the same entities back, output while the
  drawer is hidden, row close ends the shell, `exit` removes it, the empty state and its button); Ctrl+Shift+T / W; the
  bell's flash; a failed start says why; the "+" chooser at three sizes (32 px rows, T / F keys, a Terminal tab with
  its own shell, its label from OSC 2, closing the tab keeps the shell); a dropped view ends its shell.
- `terminal::real_tests` (2, real `Core`, `hermes_testdir` folders): PowerShell through `Backend::terminal_spawn` in the
  thread's folder: `Write-Output ('hermes-' + (6*7))` -> `hermes-42`, `(Get-Location)` is the folder, a resize to 72
  columns -> `$Host.UI.RawUI.WindowSize.Width` = 72, **`less` from Git for Windows** (`C:\Program Files\Git\usr\bin`)
  enters the alternate screen, pages, `q` leaves; a hidden `ping` started from the shell and the shell are gone after
  close (`tasklist`); two shells and their children are gone after `terminals_close_all` (the app's quit). 4.3 s alone.
  Under the full suite's load an earlier version failed: the command typed right after `less` quit was lost (typed
  while `less` was still reading its tty); the test now waits for the prompt after `less`, waits up to 90 s, and a
  `Term` closes its shell on drop (a failed run left its folder held until the sweep).
- `hermes_core` `terminal::tests::ending_a_shell_ends_the_programs_it_started` (see Files).
- Runs: `cargo test -p hermes-native` **626 passed, 1 failed (the real test above, timeout under load; fixed), 2 ignored**
  in a first run; after the fixes `scripts/native/check.ps1` ran **627 passed, 2 ignored** (lib tests) and **630 passed, 2
  ignored** with `--features snapshots` (the snapshot test 411 s); `cargo test -p hermes_core` **150 passed**; `cargo test -p hermes-desktop` **130 + 68 passed** (the
  webapp's `dist-webapp` copied from the main checkout for the build, removed after); `cargo build --workspace --locked`
  passes; `scripts/perf/native-thread-check.ps1` "native-thread-check: ok"; `scripts/native/check.ps1` **"native check: all ok"** (last run).
  `scripts/clean-test-temp.ps1`: dry run 27 folders (5.7 MB), `-Delete` removed 27, then 0. No shell left: after the runs no `pwsh` / `powershell -NoLogo` / `PING.EXE` / `less.exe` process of ours
  (checked with `Get-CimInstance Win32_Process`), nothing under `target\claude` running.

### Snapshots
New (4), `crates/native/snapshots/`: `terminal-t3-{light,dark}.png` (720 x 300 drawer, 5 states: `prompt`, `colours`,
`tui`, `list`, `scrolled`) and `panel-terminal-t3-{light,dark}.png` (380 x 620 panel: `chooser`, `tab`). Copied from the
harness's `.actual.png` (no `UPDATE_SNAPSHOTS=1`, so no other baseline was rewritten). **Pre-existing baselines that
changed (6):** `app-frame-t3-{light,dark}` (the drawer's "Terminal" header and "arrives in Phase 6" text became "Open a
thread to use its terminal."; the strip's "+"), `panel-diff-t3-{light,dark}` and `panel-files-t3-{light,dark}` (the
strip's "+" only). I looked at both new dark sheets, the light `terminal` sheet and the `app-frame` dark diff.
Compared with `app-terminal-open.png` (prompt top left on black, toolbar pill top right, a filled cursor) and
`sidebar-right.png` ("Open a surface": centred title, icon / label / letter rows): same structure; ours has two toolbar
buttons, not four (no split). A Haiku / Sonnet visual check should look at the four new and six changed images.

### Deviations
- **No split panes** (t3code's split horizontally / vertically and groups): one terminal shown per surface, a list for
  several. **No rename** (the brief: titles from OSC only). No "add terminal context to chat".
- **Drawer and panel terminals are separate** (as t3code); a thread's drawer terminals are not shown in the panel.
- **Ctrl+B reaches the shell** (t3code keeps it for the sidebar); new / close terminal are Ctrl+Shift+T / W (Windows
  Terminal), not t3code's in-terminal `mod+n` / `mod+w`.
- **Right-click does nothing** (no context menu; Windows Terminal's right-click paste was left out: a stray click
  could run a pasted line in a shell without bracketed paste). Middle and right buttons are reported to programs.
- **No cursor blink by default** (only when a program asks with DECSCUSR); bell is flash, with no setting yet (Phase 7).
- **Cargo.lock edited by hand:** adding the crate made Cargo re-point 18 existing dependency edges at other
  `windows-sys` / `windows-link` / `libloading` versions; the lock keeps the old edges and adds only the 7 packages
  (`cargo metadata --locked` and `cargo build --workspace --locked` accept it).
- **No t3code screenshot was taken** (t3code's window cannot be driven from here): several terminals, light theme,
  selection and the scrolled indicator follow `ThreadTerminalDrawer.tsx` / `RightPanelTabs.tsx` and the tokens.
- File links are resolved against the thread's folder, not the shell's current folder (after a `cd` a relative path
  may name another file or be refused by the Files tab).

### For the reviewer
1. **Process cleanup without a job object.** Close and quit kill the tree (`taskkill /T`) while the shell is alive;
   a crash of the app leaves a process started in its own console (e.g. `Start-Process -WindowStyle Hidden`) running.
   A Windows job object with KILL_ON_JOB_CLOSE (needs the `Win32_System_JobObjects` feature of `windows`) would cover
   crashes; not done (feature change for the whole workspace).
2. **`Terminals::remove` now kills the tree for the Tauri page's terminal too** (same core type); its socket tests
   pass. `Drop for Terminals` calls `close_all` (taskkill per remaining shell on the dropping thread).
3. **The pump's lag policy resets the emulator** and replays the 1 MiB history: the scrollback before that is lost and
   a program on the alternate screen is redrawn from whatever the history holds.
4. **Synchronized updates** (`CSI ? 2026`): a pending one is ended only when the view draws or more output comes (no
   timer in the pump).
5. **The view updates its own entity in the canvas's prepaint** (`set_layout`, the gpui-component pattern); a size
   change costs one extra frame.
6. **`EntityInputHandler`**: only the marked (composition) text is exposed; `selected_text_range` is its end.
   Real IME behaviour (Japanese / emoji panel) is hand-only.
7. **Fonts**: CJK and emoji rely on DirectWrite fallback through the listed families; box drawing is ours (rounded
   corners drawn square, dashed lines left to the font).
8. **OSC 52 copy is allowed** (a program can set the clipboard, as in Windows Terminal); reading it is refused.
   (Review: both are refused now.)
9. The **environment is unchanged** (point 10 above): the user's own variables reach their terminal.
10. `TerminalGroup` keeps every thread's terminals until closed or quit; a deleted thread's terminals are not closed
    when the thread goes (they end at quit).
11. Pre-existing and untouched: clippy notes and unformatted Phase 4/5 files. New files and `app.rs` / core `terminal.rs`
    are `rustfmt`ed; `panel/mod.rs`, `workspace.rs`, `model/backend.rs` were not formatted before and were left so
    (rustfmt on `panel/mod.rs` would reformat its child modules too).
12. **Visual check pending:** 4 new and 6 changed baselines (Snapshots above); the cursor in stories is drawn filled
    because `new_terminal` focuses the terminal (unfocused would be an outline).

### Phase 6 Done
- "Snapshots of each panel with fixture repos": `panel-diff-t3-*` (CP6.1, fake repository `panel/fixture.rs`),
  `panel-files-t3-*` and `file-finder-t3-*` (CP6.2, fake project `panel/fixture_files.rs`), `terminal-t3-*` and
  `panel-terminal-t3-*` (CP6.3, the scripted shell). Live: `panel::real_tests` (git), `terminal::real_tests` (shell).
- "The terminal passes a scripted session (colours, a full-screen program like `less` or `vim` if present, resize)":
  `terminal::tests` (colours, attributes, cursor, erase, wide, alt screen, scroll region, the TUI screen, resize and
  reflow), `terminal::view_tests` (keys, mouse, resize through the frame), `terminal::real_tests` (real PowerShell, real
  `less`, a resize the shell reports).

### User checklist (Phase 6: diff, files, terminal)
Build (PowerShell, in `C:\Users\alexandros\repos\hermes-rs-n6`): `$env:CARGO_TARGET_DIR="target/claude"; cargo build -p
hermes-native --bins`, then `$env:HERMES_RS_HOME="$PWD\target\native-test"` (synthetic, never `%APPDATA%`). Use a
throwaway git repository for anything that reverts or runs commands.
A. Fixture, synthetic data: `.\target\claude\debug\hermes-native.exe --fixture`, open "halo".
 1. Diff (CP6.1 checklist items 1-7 above) and Files (CP6.2 items 1-7 above) still work; the strip now ends in "+".
 2. Ctrl+J: the drawer opens with "Hermes fixture shell..." and a prompt, the cursor in it. Type `colors` Enter: the
    16 colours, a 256 strip, a true-colour ramp, bold / dim / italic / underline / inverse / strike / curly, `漢字かな`,
    emoji, a box table and blocks. Type `tui` Enter: a framed full-screen screen; click in it (nothing selects: the
    program has the mouse); Shift+drag selects; `q` leaves.
 3. Type `lines 500` Enter, scroll up with the wheel: "N lines below" appears bottom right; Shift+PageUp; type a letter:
    back to the bottom. `title build` Enter: nothing visible with one terminal; click + (top right): a second terminal and
    the list at the right, the first row now "build". `bell`: a short flash.
 4. Drag over text, Ctrl+C: copied (paste it in Notepad), and Ctrl+C again sends ^C (a new prompt). Double click a
    word, triple click a line, Shift+click extends. Ctrl+V pastes (multi-line pastes as several lines).
 5. Press Ctrl+P, Ctrl+K, Ctrl+B in the terminal: they are typed into the fixture shell (nothing opens); Ctrl+J closes
    the drawer; F6 moves the focus out.
 6. Switch to "ayo": its own terminal; back to "halo": the two terminals with their output. Close one with its x;
    type `exit` in the other: the empty state and "New terminal".
 7. Right panel (Ctrl+Alt+B), "+": "Open a surface"; press T: a Terminal tab with its own terminal; its x closes the
    tab; "+" then T brings the same terminal back; F and D open Files and Diff.
B. Real core on a throwaway folder (hand-only: the real shell): add the folder as a project, open a thread in it.
 8. Ctrl+J: PowerShell in the thread's folder (`pwd`). Run `git status`, `dir`, a long-running command and Ctrl+C.
 9. If Git for Windows is installed: `& 'C:\Program Files\Git\usr\bin\less.exe' somefile` (wheel and keys page it, `q`
    quits); `vim` from the same folder if you use it.
10. Drag the drawer's top edge: the prompt reflows; `$Host.UI.RawUI.WindowSize` shows the new size.
11. Print a URL and a `path:line` (`Write-Output https://example.com README.md:3`): Ctrl+hover underlines, Ctrl+click
    opens the browser / the Files tab.
12. Start `ping -t 127.0.0.1` and close the terminal (trash): ping is gone in Task Manager; open two terminals, quit the
    app: no `pwsh` / `ping` left.
13. (Review) Copy some text in Notepad, then in the terminal run `$e=[char]27; $b=[char]7` and
    `Write-Output "$e]52;c;aGVsbG8=$b"`: Ctrl+V in Notepad still pastes your text (a program cannot set the
    clipboard). Run `Write-Output "$e]8;;https://example.com$e\click me$e]8;;$e\"`: Ctrl+click on "click me" opens
    nothing (only addresses visible in the text open).
14. (Review) Switch to a light theme, run `colors` in the fixture shell (A.2): every colour of the 16 is readable on
    the light canvas (bright green, bright yellow and bright white are darker than t3code's).
Hand-only (no test can judge them): real shell feel and typing latency, IME (Win+. emoji panel, a Japanese or Greek
IME), mouse selection feel, vim / less in daily use, Ctrl+click links in the real browser, copy / paste with the real
clipboard, wide and emoji glyph rendering and font fallback, resize by dragging, the light theme, cursor look.

### Visual check (Sonnet 5.5)
Opened: `app-terminal-open.png` (dark), `sidebar-right.png`; t3code's `ThreadTerminalDrawer.tsx` was not opened (the screenshots only). Crops of every state, old vs new for the six changed.
- terminal-t3-dark: matches app-terminal-open (prompt top left on black, toolbar pill top right, filled light-blue cursor). Colours legible on black (0 invisible by nature), bold/dim/inverse/strike/curly, wide and emoji on the grid, box drawing continuous, blocks ok, selection visible, "12 lines below" pill ok. Differs: the toolbar has 2 buttons, not 4 (documented); no scrollbar is drawn in the scrolled state; in `tui` the toolbar overlaps the program's top-right border (floating, as t3code).
- terminal-t3-light: expected; marginal contrast for bright colours on the near-white (15 about #a5a5a5, 11 olive-yellow, 9 bright green); all readable, no clipping or overlap.
- panel-terminal-t3-dark: matches sidebar-right (centred title, icon/label/letter rows; fewer rows: documented); tab strip with a closable "Terminal 1" and "+": expected.
- panel-terminal-t3-light: expected.
- app-frame, panel-diff, panel-files (light, dark): changed-as-expected (drawer placeholder text; the strip's "+" only; pixel diff confined to the strip row).
- Phase 6 spot-check (panel-diff, panel-files, file-finder, light and dark): nothing unexpected.

### Review (Opus 5.5)
`main` had not moved (nothing to merge). Read the diff, `alacritty_terminal` 0.26.0's reply sites and
`portable-pty` 0.9's Windows child, and t3code's `ThreadTerminalDrawer.tsx` / `keybindings.ts`.

**Security (fixed):**
1. **Bracketed paste could be closed early.** `encode_paste` removed `ESC[201~` once, so `ESC[20ESC[201~1~` closed up
   into a fresh end marker and the rest ran as typed commands. Now every ESC (and the 8-bit CSI U+009B) is removed
   from bracketed text; test with the nested form.
2. **OSC 52 write was allowed.** Printed output (a `cat`ed file) could replace the clipboard with a command the user
   then pastes elsewhere. Now `Osc52::Disabled` (read was already refused); the clipboard plumbing is gone.
3. **OSC 8 targets were opened.** A hyperlink's target can differ from its text (`https://good.example` leading
   elsewhere). OSC 8 is no longer followed; only addresses visible in the text open (http(s) only, as before).
4. **The tree kill could hit an unrelated process.** `taskkill /T` walks by parent id only; Windows keeps a dead
   parent's id in its children, so a process whose parent id is a recycled number now held by the shell would be
   killed. New `hermes_core::terminal::kill_descendants`: a ToolHelp snapshot, walked from the shell's pid (safe:
   the killer holds the shell's handle, so the id is never reused, even after exit), a child counted only when created
   after its parent, each opened before it is judged and killed through that handle. `kill` = descendants, shell,
   descendants again (started in between); also runs after the shell exited (orphans are found through the held id).
   Faster than spawning `taskkill` (quit runs on the UI thread). `git.rs` keeps its `taskkill` (back to private).
   Dependency: `windows` (workspace, 0.62) for `hermes_core` with `Win32_System_Diagnostics_ToolHelp`; one line in
   `Cargo.lock`.

**Checked, no change:** replies are fixed numeric formats (DSR, DA, DECRQM, CSI 14/18 t, OSC 4/10/11 colours, kitty
mode); `alacritty_terminal` has no title report (`CSI 21 t`) - test `printed_text_cannot_type_into_the_shell` feeds a
hostile title plus every query. File links go to the Files tab, whose `fs::locate` refuses `\\host\share` and
`\\?\UNC` lexically before touching the disk (CP6.2). `cx.open_url` only for `http(s)://`. Environment: neither the
core nor the native app calls `set_var` outside tests; provider keys live in the keyring, so the shell gets exactly the
user's own environment (as a terminal beside the app would); scrubbing it would change the Tauri terminal too - left.
`taskkill` (git) is called by its System32 path with an argument vector.

**Not done, on purpose:** **no job object.** KILL_ON_JOB_CLOSE would also end, when the terminal closes, a GUI program
whose launcher has exited (`code .`, `start notepad`): Windows Terminal leaves those running, and so does the walk
above. The cost stays: an app crash leaves a program started in a console of its own (`Start-Process
-WindowStyle Hidden`); console-attached programs end with the pseudo-console. **`Ctrl+J` stays reserved:** t3code's
default `{ key: "mod+j", command: "terminal.toggle" }` has no `when`, so it also works in a focused terminal; a bare LF
cannot be typed (documented in the decision above).

**Visual check items:** (a) light palette fixed: t3code's bright green `#14ce14`, bright yellow `#b5ba00`, bright white
`#a5a5a5` (about 2:1) are now `#0e9a0e`, `#8f8a00`, `#8c8c8c`; test `the_light_palette_is_readable_on_the_light_canvas`
(all 16 at least 3:1 on the light canvas). (b) No scrollbar in the scrolled state: left; the "N lines below" pill and
Shift+PageUp cover it (Phase 8 polish). (c) Toolbar over a program's top-right border: as t3code, accepted. (d) Chooser
with 3 rows: accepted (documented). (e) Compared with `ThreadTerminalDrawer.tsx`: font 12 px (`DEFAULT_TERMINAL_FONT_SIZE`)
and the toolbar pill (`right-2 top-2`) match; the list's rows were 22 px / 11 px text / small radius, t3code's are
`h-6 rounded-md text-xs pr-2 pl-1.5`: now 24 px, 12 px, `radius::MD`, 6 / 8 px padding (header stays `h-[22px]`).
Remaining deviation, documented in `group.rs`: a close button at the row's end (t3code swaps the icon on hover).
Baselines `terminal-t3-{light,dark}` re-accepted after looking: light = the palette and the list; dark = the list rows
only (pixel diff confined to the list).

**Emulator / renderer, assessed:** the lag policy (reset + replay of the 1 MiB history) can start mid-sequence and lose
the alternate-screen switch if it is older than the history: the user may see a garbled screen until the program
redraws (or Ctrl+L); it needs more than 1,024 x 16 KiB of unread output, i.e. the UI frozen for long; accepted and
noted in "For the reviewer" 3. Resize: emulator first, the shell after 100 ms, sizes clamped to at least 2 x 1. Copy
trims trailing blanks per line and joins wrapped lines (Alacritty's). `Ctrl+C` with a selection copies even while a
program runs (as Windows Terminal). Blink only on DECSCUSR, its task dropped on blur. A hidden terminal parses but does
not draw. Memory: 10,000 lines x 120 columns is ~29 MB a terminal; with the 16-shell cap about 460 MB at worst, left.
AltGr: `Ctrl+Alt+<key>` with a character is text in the terminal; the global `Ctrl+Alt+B` vs AltGr+B is the Phase 2
item, unchanged. Views do not call the core: `terminal/session.rs` holds the `hermes_core::terminal` adapter behind
the `Pty` seam, spawned from `model/backend.rs`; spawn and close run in `background_spawn`.

**Tests made deterministic:** `real_tests` slept 300 ms after `less` quit. Now `Term::sync` resends a marker command
until its computed output shows (a program that just quit may read the first ones). The `less` part now asserts the
pages: the first page holds `row 1` and not `row 25` or the shell's lines, the next one `row 30` without `row 1`, the
shell's screen comes back after `q`. The whole `terminal` module ran 5 times in a row (49 passed each time; the real
test runs beside the others).

**Licences / lock:** `alacritty_terminal`'s Windows tree is MIT / Apache-2.0 / Unlicense / Zlib (none copyleft).
`cargo update --workspace --dry-run` locks 0 packages: the hand-edited lock is what Cargo would keep; its diff is
additions only.

**Final runs (review, `CARGO_TARGET_DIR=target/claude`):** `cargo test -p hermes-native --features snapshots`: 630
passed, 2 failed: the two `terminal-t3` baselines (re-accepted, above) and
`thread::composer::real_tests::a_model_and_a_mode_picked_during_a_turn_apply_from_the_next_turn_on_a_real_core`
("the footer showed the old model"), which passes alone (10 / 10) and in `check.ps1`: a Phase 5 test that is timing
sensitive under the full suite's load, not touched here (left for Phase 5's owner). `scripts/native/check.ps1`: "native
check: all ok". `cargo test -p hermes_core`: 150 passed. `cargo test -p hermes-desktop` (with `dist-webapp` copied in
from the main checkout and removed after): 130 + 68 passed; `crates/desktop` is untouched, so its RPC table and
terminal socket are as before (the socket now ends shells through `kill_descendants`). `cargo build --workspace
--locked`: ok. `scripts/perf/native-thread-check.ps1`: ok. `scripts/clean-test-temp.ps1`: 20 folders (4.8 MB) removed,
then none. No process under `target\claude`, no child of one, no `PING.EXE` / `less.exe`, no orphaned `-NoLogo` shell.
