# Performance runs

Measures hermes-desktop as a separate instance: its own data folder, port 47810 and WebView2
profile (`target/perf`), with WebView2's DevTools protocol on port 9223 for the page's own timings.
A mock model streams at a fixed rate, so no real model or GPU is in the numbers. Your app, its data
and its browser cache are never touched.

```powershell
cargo build -p hermes-desktop --release
uv run --no-project python scripts/perf/mock_llm.py --tps 80 --tokens 1500   # leave running
powershell -File scripts/perf/perf.ps1 -Do start -Cold    # ms to the server, to the page's first call
node scripts/perf/load.mjs cold 3                          # page load: composer shown, long tasks
node scripts/perf/load.mjs warm 3
powershell -File scripts/perf/perf.ps1 -Do mem             # memory of the process tree
powershell -File scripts/perf/perf.ps1 -Do cpu -Seconds 10 # CPU over an interval (idle)
node scripts/perf/stream.mjs 24                            # one reply: frames, main thread, long tasks
                                                           #   (run perf.ps1 -Do cpu alongside)
node scripts/perf/cdp.mjs --metrics                        # JS heap, DOM nodes
node scripts/perf/switch.mjs '#/<session id>' 4            # opening a session (after 10 replies)
powershell -File scripts/perf/perf.ps1 -Do stop
```

Use the Node in `tools/node`. `stream.mjs` types into the composer; `switch.mjs` expects a session
whose last prompt is "Reply number 10, please." (send ten with `stream.mjs 4 "Reply number N, please."`,
the mock at `--tps 2000`).

## Baseline, 2026-11-04 (release build, Windows 11, Hermes `c419cf7c`)

| | |
|---|---|
| Server up after launch | 0.18 s |
| Page's first call after launch | 0.95 s (WebView2 starting its processes, navigating) |
| Composer shown after navigation | 115 ms warm, 250 ms cold (110 JS files, 6.8 MB) |
| Streaming 1,500 tokens at 80 tok/s | 60 fps; page main thread 11% busy; backend ~1% of a core, app 31% |
| Idle after a reply | ~1% of a core |
| Memory, fresh, one chat open | ~370 MB private (renderer ~210 MB, JS heap ~60 MB; backend ~18 MB) |
| Per long markdown reply | ~+10 MB, ~+1,000 DOM nodes (off-screen turns skip layout) |
| Opening a session of ~15k tokens | ~240 ms, one ~80 ms long task |

## Startup: backend built alongside the window (2026-11-04)

`perf.ps1 -Do start`, median of 5 warm launches; the first-run line is one launch with a fresh data
folder and WebView2 profile.

| Launch → | Before | After |
|---|---|---|
| server up | 130 ms | 77 ms |
| navigation starts | 390 ms | 283 ms |
| first paint | 850 ms | 756 ms |
| first run: server up / first paint | 1.9 s / 3.0 s | 0.77 s / 1.5 s |

The backend is built on the async runtime while Tauri starts WebView2 (`crates/desktop/src/main.rs`),
and the skills folder's git bookkeeping (`git status`, ~80 ms each launch; the first commit of ~300
files on a first run) runs on a background thread (`SkillStore::seed_bundled_in_background`). The
backend is now ready before WebView2 navigates; the rest of startup is WebView2's.

## Protocol smoke test

With the mock and an instance running (`perf.ps1 -Do start`; `HERMES_PERF_EXE` picks a debug
build), `node scripts/perf/smoke.mjs 47810 <a folder with a file in it>` drives a session the way
the page does (the mock answers "please write file NAME" with a `write_file` call) and checks
rollback, `/undo`, the built-in commands, export, compress, the curator, memory, search and logs.
`rollback-check.mjs` is the checkpoint round trip on its own; `stop-check.mjs` checks Stop on a
stalled provider, a long command and a huge output; `endpoints-check.mjs` checks Settings ▸
Providers ▸ Custom endpoints (validate, save with a key, the picker, activate, delete).
`attach-check.mjs` checks composer attachments (image upload and read-back, images reaching the
model, held and detached images, file upload and `file.attach`); it is the provider on :47900
itself, so stop `mock_llm.py` first.
`files-check.mjs` checks the side panel's file routes (read, save, mkdir, git root, a ticketed
download, a ranged media stream, the credential guard).
`git-check.mjs` checks the review pane's git routes in a throwaway repository (list, diffs,
stage, commit context, commit, branches, worktrees, revert).
`terminal-check.mjs` checks the side panel's terminal (open, run, reattach with replay, close, exit).
`subagents-check.mjs` checks live subagents (events, list, tail, steer, stop) with its own provider
on :47900 (stop `mock_llm.py` first).
`coding-commands-check.mjs` checks `/plan`, `/init`, `/learn` and `/review` (its reviewer subagent) with
its own provider on :47900.
`side-and-branch-check.mjs` checks `/branch` (all three routes, retries, the parent link), `/btw` and `/bg`
with its own provider on :47900.
`path-policy-check.mjs` checks the file tools' path policy (a model asking to write into the app's data, skills
 and memories folders is refused, naming the right tool, and nothing is written) with its own provider
 on :47900.
`toolsets-check.mjs` checks Capabilities ▸ Tools (list, switch off and on, the next turn's tools, a refused
call) with its own provider on :47900.
`tool-stream-check.mjs` checks the terminal's live output against the page (what the terminal sends
as `tool.progress` frames reaches it; a command that prints in two bursts must produce relay frames
before the `tool.complete` answer carries the result, patches/0017) and that a stop mid-command
keeps the output (the interrupted turn's `tool.complete` carries the result it had printed).
`memory-check.mjs` checks the Star map's routes (graph, node detail, edit, delete, a stale id) and that
Settings ▸ Memory reaches the store; it restores the perf instance's memory files after.
`artifacts-check.mjs` checks the Artifacts view through the page (a written file named in a reply
and a link are listed).
`archive-check.mjs` checks archived sessions (archive from PATCH and the shortcut, the `archived=`
lists, the project tree, unarchive, deleting an archived one, the auto-archive defaults).
`workspace-check.mjs` checks moving a session between projects (the canonical `workspace` mutation: the
receipt's cwd/branch/repo-root, session.info and the list carrying the identity, a non-repo folder
replacing it with nulls, the bare-home move filing the session into Home, the unknown-folder refusal,
the busy refusal while a turn runs) with its
own provider on :47900.
`profiles-check.mjs` checks the profiles window's SOUL.md editor (read, save, read back, a never-written
one as missing, another profile name refused like Hermes); it restores the SOUL.md it found.
`provider-error-check.mjs` checks the provider-error card's contract (its own provider on :47900 answers a
429 daily limit): the turn's `message.complete` carries status "error", the provider's text and the
descriptor — layer/code, a quota limit not retryable.
`file-tree-open-check.mjs` checks the file trees' "Open in default app" route (patches/0015) and the
"Reveal in File Explorer" route (patches/0016): a missing file, a relative path, an empty path and the
`file:` form answer 400 with a readable detail — the open itself is exercised by `artifacts-check.mjs`
and the `open_file.rs` unit tests.
`import-check.mjs` checks the Hermes importer (list, preview, import, `/import-hermes`, continuing an
imported chat) against a home built by `make-hermes-db.py` with Hermes's own SessionDB; start the
instance with `HERMES_HOME` set to it.

`aux-models-check.mjs` checks Settings ▸ Model ▸ Auxiliary (the four jobs, refused assignments,
a title and a compression model with its own and an inherited effort reaching their requests,
reset).

`keyboard-nav-check.mjs` checks arrow-key navigation through the page (DevTools, patches/0018 and
0019): the file tree's ↑/↓ move the visible selection and ←/→ fold the selected folder, and the
Settings rail's rows walk with ↑/↓ (wrap-around; arrows only move focus, the hash never changes).
It reloads the page first — arborist's focus cursor drifts under synthetic keydowns with no real
DOM focus, which real usage never produces.

`cdp.mjs '<expression>'` evaluates JavaScript in the instance's page (DevTools port 9223) and
prints the result; `hover.mjs '<selector|label|text>' [click]` moves the mouse onto an element and
lists the popovers, tooltips and menus that appear, with their boxes. Together they check UI
without screenshots (see `docs/TESTING.md`).

## Bundle analysis

```powershell
# Why does a library load before the first paint? (shortest chain of static imports)
node scripts/perf/import-chains.mjs target/hermes-src/.build/webapp-workspace <scratch dir> '@xterm/' '/katex/'
# What the startup JavaScript is made of, by package and app folder
node scripts/perf/sourcemap-build.mjs target/hermes-src/.build/webapp-workspace <scratch dir>
node scripts/perf/startup-bytes.mjs <scratch dir> group 30
```

The workspace is prepared by `scripts/build-hermes-webapp.ps1` (with the patches applied).

Lazy-loading xterm, CodeMirror, KaTeX and the brand icons (tried 2026-11-04, 10 Hermes files):
startup JavaScript 6.5 -> 5.3 MB, but a fresh start's memory only -6 MB (~2%) and the composer
~10 ms sooner. V8 compiles functions when first called and WebView2 caches compiled code, so code
that loads but never runs costs little; not worth a 10-file patch to carry across Hermes updates.

