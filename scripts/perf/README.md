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

