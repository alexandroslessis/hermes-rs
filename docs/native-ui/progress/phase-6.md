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
