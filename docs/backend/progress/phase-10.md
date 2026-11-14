# Phase 10 progress

## CP10.1 — `run_script`: one script instead of several tool calls, approved as a whole
Worktree: C:\Users\alexandros\repos\hermes-rs-p10   Branch: backend/p10-roundtrips   Model: Sonnet 5.5 (subagent)
Cut from `main` at d893837 (Phases 0-9 merged). Uncommitted.
Done:
- **`run_script` tool** (`crates/tools/src/script.rs`, schema `prompts/tools/run_script.json`): args
  `{language: powershell|python|bash, code, timeout}`. The script is written to
  `<scratch>/scripts/<random>/script.<ext>` (PowerShell with a byte-order mark, line ends unified) and run
  from there with the session's working folder as cwd: the terminal's scrubbed environment
  (`shell::command`), stdin closed, no console, in a job object so the process tree dies on timeout, on the
  turn being stopped, when the call future is dropped and when the script ends. The script goes in as a
  file, never as a command-line string; PowerShell's command line is our own 3-line driver
  (`-EncodedCommand`, only the random path in it: UTF-8 output, no progress records, exit code as the
  terminal computes it). Output: stdout and stderr read up to 8 MB / 1 MB, stdout trimmed like the terminal
  (`shell::limit_output`: head and tail, whole text saved), stderr clipped and returned as its own field,
  secrets masked (`redact`, the stronger pass when the script reads the environment or a secret file).
  `timeout` default 60 s, cap 600 s; `code` at most 64 KB, no NUL. Python is `uv run --no-project
  --no-python-downloads --quiet python -X utf8 <file>` (never `python` on PATH, no downloads, no PEP 723
  inline-metadata installs); bash is Git for Windows' `bash.exe`; a missing uv or bash is a plain error
  before anyone is asked. The scratch folder is removed after the run (retried while Windows holds a
  killed process's handle; a `Drop` guard covers a dropped future).
- **Approval** (`SessionTools::check_script` in `script.rs`, analysis in `script_guard.rs`): the whole script
  goes through the terminal's `hardline` and `dangerous` over the text, every physical line and the text
  with continuations undone (backticks, carets, backslash-newline; NFKC), plus per-language lists of
  **dynamic-execution constructs** (PowerShell, Python, bash: see PITFALLS "run_script"), plus the
  terminal's `command_flag` (persistence, app data, skill scripts) over the text, plus *obvious* path
  literals outside the working folder (`path_policy::script_outside_path`). Hardline anywhere: blocked in
  every mode. A dynamic construct: asks in every mode but Yolo, **never smart-approved, never remembered**.
  Persistence: asks even in Yolo. Tainted + flagged: always asks (never smart-approved); Yolo runs and warns.
  The card shows `run_script (<language>)` and the whole script, once/deny only; "session"/"always" are
  honoured as "once", and remembered terminal approvals are not consulted. A script that needs a decision
  and is longer than 16,000 characters is refused (the user must see what they approve).
- Wiring: the tool belongs to the `terminal` toolset (`desktop/src/toolsets.rs`), so it is offered where
  `terminal` is and switched off with it; the review and curator whitelists never allow it; subagents get it
  like `terminal`; `checkpoint_before` snapshots the folder when the script may change files
  (`script_guard::may_change_files`); `taint_after_call` taints after a script that fetches from the network
  or names an outside path; `guards.rs` / `compress.rs` treat it like `terminal`; ACP kind `execute`, a
  title, stderr in the card.
- No system prompt text, no first-turn context pack, no multi-path `read_file`, no todo guidance
  (CP10.2).
Files: `crates/tools/src/{script.rs,script_guard.rs}` (new), `crates/tools/src/{lib.rs,path_policy.rs,
shell.rs}`, `prompts/tools/{run_script.json,README.md}`, `crates/prompts/src/tools.rs`,
`crates/desktop/src/{toolsets.rs,acp/tools.rs}`, `crates/runtime/src/{guards.rs,compress.rs}`,
`crates/runtime/tests/turns.rs`, `PORTING.md`, `GAPS.md`, `docs/{ARCHITECTURE,PITFALLS,TESTING}.md`.
Tests (all with `CARGO_TARGET_DIR=target/claude`, 2026-11-13):
- `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm`: all ok
  (hermes_tools 306 tests, 47 of them `script*`).
- `cargo test -p hermes_runtime --lib`: 180 ok. `--test turns`: 213 ok under the default profile and under
  `HERMES_TEST_PROFILE=balanced`, `requests` and `tokens` (8 new tests).
- `cargo test -p hermes-desktop`: 176 + 68 ok. The worktree has no `target/hermes-src` (the webapp build
  needs npm), so I made an empty `target/hermes-src/apps/desktop/dist-webapp` directory for the build
  script and removed it afterwards: the desktop tests do not read the webapp.
- `cargo build --workspace`: ok.
- Real interpreters on this machine: Windows PowerShell 5.1, uv 0.9.26 (finds a Python), Git Bash: the
  execution tests ran against all three (they skip themselves when an interpreter is absent).
- No live check against the perf instance (nothing the page calls changed); the desktop UI was not driven.
Request counts: unchanged for every existing scenario (the full `turns` suite passes under the default,
`balanced`, `requests` and `tokens` profiles). New: three reads as three `terminal` calls = 4 main
requests; the same reads in one `run_script` = 2 (`one_run_script_call_does_the_work_of_three_tool_calls`).
Deviations from the plan:
- *Remembered approvals are ignored for every script*, not only dynamic ones (the brief says dynamic
  scripts are never remembered; I extended it to all, because the key a terminal approval is remembered by
  is the pattern's description and a script mixes several). Stricter, one click more.
- *Outside path literals flag reads too* (a script's reads and writes cannot be told apart): in
  AcceptEdits and Auto a script naming `C:\Windows\...` asks, where `read_file` of the same path does not.
- *No live output relay and no background mode* (the plan did not ask for them).
- *No live check script*: the page calls nothing new (the tool's rows render like any tool).
- Hardline is enforced even when a session has no approval store (the terminal's check returns before it
  in that case); only test sessions have none.
For the reviewer (security is the focus):
1. **Residual risks** (all written in PITFALLS "run_script"; the guard is an approval aid, not a sandbox,
   Phase 15 is the boundary): (a) *write, then run*: a script writing a file with an extension not on
   the list and running it by a plain command (`Set-Content x.dat ...; python x.dat`, `node x.mjs`);
   (b) *programs that run programs* (`git -c alias.x='!..'`, `cargo build` build scripts, `make`, `tar
   --to-command`, `rsync -e`...); (c) Python's dynamism: the list is names, an unlisted stdlib/third-party
   module that evaluates strings gets through; (d) exfiltration over the network is not an approval trigger
   (only a taint afterwards); (e) in Auto the reviewer sees the text, and `<# #>` comments and Python
   docstrings are not stripped like `#` comments (only for scripts flagged by a dangerous pattern alone);
   (f) a module or `.psm1` written earlier and imported runs unread; (g) scripts run with the user's
   rights in the working folder.
2. Please read the rule tables in `script_guard.rs` with an attacker's eyes: PowerShell `&` / `.`
   handling (`&\s*["'][^"'\n]*[$+(]` lets `& "C:\tool.exe"` through; `& .\x.ps1` and `& 'x.ps1'` are not
   flagged), bash command-position variable rule, whether `\b(?:eval|source|exec|xargs|trap|parallel)\b`
   (any occurrence, also as text) is too broad, `CMD_POS` anchors, the `(?!\s*=)` guard on `cmd` / `sh`
   as variable names.
3. `check_script` vs `check_command`: compare the mode matrix (Manual/Plan ask every script, AcceptEdits and
   Auto ask when flagged, Yolo only for persistence). `store.asks()` is not consulted (only through
   `mode()`); with the approval store absent the script runs after the hardline check.
4. Over-long scripts: refused only when a decision is needed; the 16,000-character bound is my choice. A
   64 KB benign script costs seconds in a debug build (see PITFALLS "Checking cost"); the analysis is on
   `spawn_blocking`. A regex that errors counts as a match (asks).
5. The per-line scan calls `approval::dangerous` for every distinct line: fine for typical scripts, linear
   in script size; check for catastrophic patterns with a hostile 64 KB input if you want (the existing
   `dangerous` patterns have `.*` under dotall).
6. Cleanup of the scratch folder when the whole app crashes is not handled (nothing sweeps
   `scratch/<session>/scripts`); a killed process may hold the folder for a moment (retries cover it).
7. `$LASTEXITCODE` leaks: a script whose last native command failed but which carried on reports that
   exit code (the terminal's rule, kept for consistency); a terminating error is exit code 1.
8. Python needs uv's own interpreter discovery to find a Python; on a machine with none, the result is
   uv's error text on stderr and a nonzero exit (not our "uv was not found" message).
9. `taint_after_call("run_script")` taints on any outside-path literal in the script, also when the path was
   only mentioned (conservative; the script was asked about anyway in the asking modes).
10. The new tool changes the frozen tool list once on upgrade (PITFALLS).

### Review (Opus 5.5)
Main had not moved (no merge). Reviewed as an attacker: a prompt-injected model trying to get a
command run unread. `/code-review` not available in the subagent; manual review plus an adversarial
corpus of **~200 scripts** (`crates/tools/src/script_guard_corpus.rs`, kept as regression tests):
every route of dynamic execution in each language, obfuscation (continuations, concatenation, case,
tabs, CR/CRLF/U+2028, zero-width/bidi, full-width, en dashes), every terminal-guard pattern wrapped
in a script (top level, after `;`, in `if (..) {}`, `then ..; fi`, a function, after `&&`), padded lines.
The first run of the corpus missed **56** attacks; all are caught now.

Found and fixed:
1. **`run_script` bypassed the terminal's approval inside blocks.** The terminal's command-position
   patterns (hardline `rm -rf /`, `shutdown`, `python -c`) did not see a command after `{`, `(`,
   `then`, `do`: `if ($true) { shutdown /s /t 0 }` ran unasked in AcceptEdits/Auto, `then rm -rf /`
   only asked. Each line is now also split, quote-aware, into the commands inside its blocks.
2. **PowerShell en/em dashes** (`Remove-Item x –Recurse –Force` deletes recursively, verified) and
   curly quotes are folded; `\v \f NEL U+2028/9` count as line breaks.
3. **Missing constructs**: the `function:` drive (string -> function, verified), `.InvokeReturnAsIs()`,
   `[wmiclass]`/`Win32_Process`, `Import-Module`/`using module`, `ii`, LOLBins (`explorer`, `msbuild`,
   `installutil`, `regasm`, `cmstp`, `runas`...), shells by any path or behind `env`/`command`/`nohup`,
   registry writes (`Set-ItemProperty HKCU:`, `[Microsoft.Win32.Registry]`); `ri -r -fo`, `-Recu`,
   `| Remove-Item`, `[IO.Directory]::Delete`; Python `shutil.rmtree` (deleting the project ran
   unasked), `f = exec`, `o.execv` on an alias, `from os import *`, `nt`, `posix`, `_ctypes`; bash
   `/bin/sh x`, `env bash x`, `command $c`, `alias`, `awk '{print | "sh"}'`, `sed s/../../e`; `git -c
   alias.`/`core.pager`, `git config alias.`, `tar --to-command`, `rsync -e`, `ProxyCommand`; `.env`.
4. **Persistence and credentials by name** (`$PROFILE`, `.bashrc`, startup folder, `.git/hooks`,
   `core.hooksPath`, `.ssh`, `authorized_keys`, `.aws`, `.npmrc`...) now ask **even in Yolo**
   (`Analysis::persistence`), as the terminal does for `Persistence` paths; before only a literal
   resolvable path did (`Path.home() / '.ssh' / 'authorized_keys'` passed).
5. **Write, then run within one script** (`printf ... > x; ./x`, `Set-Content x.dat; python x.dat`,
   `iwr -OutFile a.exe; .\a.exe`): a cheap rule (a file write + a run by path or an interpreter on a
   file, PowerShell/bash) now flags it. Across two calls it remains residual.
6. **Backtracking limit**: with `fancy_regex`, a benign 64 KB script hit the limit on the settings
   rule and asked (an error counts as a match). Rules now use the linear `regex` engine unless they
   need look-around. Padded lines (60 KB) are still caught; worst case measured 1-12 s per 60 KB
   line in debug, on `spawn_blocking`.
7. **Approval card**: the card showed raw text, so an RLO/ESC/backspace could reorder or hide the
   script's tail; control, bidi and invisible characters are now shown as `\u{XXXX}`, and a raw
   control character flags the script.
8. **TOCTOU / self-modification**: the script file is written, then held read-only with share-read
   only until the run ends, read back and compared with what was checked; a junction as the run or
   `scripts` folder is refused. A parallel script could otherwise swap the file after approval, and
   bash (which reads as it goes) could append to its own file. File work moved to `spawn_blocking`
   (it was on the async worker, rule 7); `Drop` removes on a thread; leftovers older than an hour
   are swept.
9. **Python ran a repository's `.venv\Scripts\python.exe`** (verified: `uv run --no-project` picks
   it). Now `uv python find --system --no-project --no-config --no-python-downloads` names the
   interpreter (from the scratch folder) and it runs directly. PEP 723 metadata was already ignored
   (verified: no install).
10. PowerShell by full path under `%SystemRoot%`; `uv.exe` only from absolute PATH entries; bash
    without `BASH_ENV`/`ENV`/`BASH_FUNC_*`; `timeout` accepts int/float/string, junk -> default.

Verified as claimed: once/deny only, "session"/"always" honoured as once, remembered terminal
approvals ignored, dynamic and persistence never reach the smart reviewer, tainted+flagged always
ask (Yolo warns), >16,000-char flagged scripts refused (now counted on the escaped text), env
scrub (test), stdin closed, job-object kill on timeout/stop/drop (tests), output caps (8 MB/1 MB,
the rest read and dropped), code limit in bytes, NUL refused; registration in the `terminal`
toolset, curator whitelist excludes it, ACP kind `execute`.

| Construct | Result |
|---|---|
| `iex`/`Invoke-Expression`, `& $x`, `&(...)`, `. $x`, aliases, `$ExecutionContext`, `[scriptblock]`, `function:` drive, `.Invoke*()` | caught (dynamic: always asks, never smart/remembered) |
| `-EncodedCommand`, `FromBase64String`, `[Text.Encoding]..GetString`, `Add-Type`, reflection, modules | caught (dynamic) |
| `Start-Process`/`start`/`ii`/`Invoke-Item`, COM (`WScript.Shell`, `Shell.Application`), WMI/CIM `Win32_Process`, `[Diagnostics.Process]`, LOLBins, `schtasks`, `certutil`, `bitsadmin` | caught (dynamic / persistence) |
| `reg add ..\Run`, `Set-ItemProperty HKCU:`, `$PROFILE`, startup folder, `.bashrc`, `.git/hooks`, `crontab`, `.ssh`/`id_rsa`/`.aws` | caught (persistence: asks even in Yolo) |
| Python `eval`/`exec`/`compile` (also aliased), `__import__`, `importlib`, `getattr`, dunders, `subprocess`, `os.system`/`exec*`/`spawn*` (also aliased), `from os import *`, `nt`, `ctypes`, `pty`, `pickle`, `pip`, decoders, `.pth`/`sitecustomize` writes, `rmtree` | caught |
| bash `eval`, `$()`, backticks, `bash -c`, `/bin/sh x`, `source`, `exec`, `xargs`, `<()`, here-doc to sh/python, `printf | sh`, `$c`/`command $c`, `alias`, `awk system`/`| "sh"`, `sed e`, `perl -e`, `find -exec` | caught |
| Every terminal-flagged command inside a script, any position/block, padded lines, continuations, CR/CRLF/U+2028, zero-width/bidi, full-width, en dashes, case, tabs | caught (dangerous/hardline as in the terminal) |
| Write then run in one script (`> x; ./x`, `Set-Content x.dat; python x.dat`) | caught (dynamic) |
| Write in one call, run in the next (`python x.dat`) | **passes (residual)**; same as `write_file` + `terminal` |
| Programs that run repository code (`npm run`, `cargo build`, `make`, `pytest`) | **passes (residual)**; as in the terminal |
| A Python name not on the list that evaluates strings; a module the model wrote earlier, imported | **passes (residual)** |
| Network exfiltration of project files (`requests.post`, `Invoke-RestMethod -Body`) | **passes (residual)**; taints afterwards; `.env`/credential names ask |
| Processes started through a service (WMI, Task Scheduler, COM server) outliving the job | constructs asked about; once approved they escape the job (documented) |
| Prompt-injected text in a PowerShell block comment / docstring reaching the Auto reviewer | only for scripts flagged by a dangerous pattern alone (as for commands) |

Tests (`CARGO_TARGET_DIR=target/claude`, 2026-11-13): `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm` all ok (hermes_tools 319, incl. the corpus, padding, TOCTOU lock, junction, venv, card escaping, timeout parsing tests); `hermes_runtime --lib` 180 ok; `--test turns` 213 ok under the default, `balanced`, `requests` and `tokens` profiles (request counts unchanged); `hermes-desktop` 176 + 68 ok (empty `target/hermes-src` placeholder, removed after); `cargo build --workspace` ok. Not done: live check (nothing the page calls changed), UI not driven.

## CP10.2 — Batching guidance, the first-turn context pack, multi-path `read_file`, todo wording
Worktree: C:\Users\alexandros\repos\hermes-rs-p10   Branch: backend/p10-roundtrips   Model: Sonnet 5.5 (subagent)
On top of CP10.1 (09e66c0). Uncommitted.
Done:
- **Batching guidance** (step 2): `BATCHING_GUIDANCE` (`prompts/src/text.rs`) is one sentence after Hermes's
  parallel-calls text in the frozen stable tier, in every profile (only when `run_script` is offered): issue
  independent reads, searches or commands as parallel tool calls in one response, or do them in one
  `run_script` that prints only what you need; `read_file` takes several files (`paths`). It names the CP10.1
  tool and the CP10.2 schema; it has no number in it.
- **todo wording** (step 5): the `requests` budget guidance (`BUDGET_GUIDANCE_REQUESTS`) ends with "Update the
  todo list only at phase boundaries (when a phase of the plan is finished or the plan changes), not after
  every step." Chosen at session start with the profile's wording and frozen (the Phase 3 pattern);
  `balanced` and `tokens` do not have it; `todo_list` itself is unchanged.
- **Context pack** (step 3, `crates/runtime/src/context_pack.rs`, wiring in `Runtime::begin_turn`, `Block::with_pack` /
  `pack_only` / `escape_tags` in `preinject.rs`): new knob `BudgetKnobs::context_pack` (`requests` true,
  `balanced` / `tokens` false; override `budget.context_pack` true / false / "" with validation, `OVERRIDE_KEYS`
  22 -> 23). On the first user message of a session (no user message in the conversation yet, not an
  app-written prompt) whose folder is in a git repository (`SessionTools::project_root`, else
  `project_skills::find_root`: a worktree counts) a `<repo-context>` element goes at the front of the `<context>`
  block (or a block of its own): header (repository, working folder, branch), 5 commit subjects, `git status
  --short` (25 lines), the project file names and sizes, the README head (2,000 characters), a depth-3
  `.gitignore`-aware tree (300 lines, 40 entries a folder). At most 14,000 characters; cut order: the tree's
  end, then the README's tail, every cut marked. Stored with the message (resume, rewind and branches replay it
  byte for byte), never in the system prompt, hidden from the page by the existing `preinject` note / strip
  (a test pins `user_text`, `typed_text`), once per session. Cached per repository root + HEAD + a hash of the
  status text in `<data>/cache/context-pack/` (32 files, 64 KB each, atomic writes, corrupt / oversized /
  wrong-key entries recomputed); branch and status always fresh. `git` runs with a 4 s limit per call (8 s in
  all, 12 s for the whole thing, `spawn_blocking`), output capped at 256 KB, `core.fsmonitor=false`, no optional
  locks; any failure omits that part (no `git`: tree and README only). Repository text: control / bidi characters
  replaced, the block's and the element's tags escaped (also over cached content), README past
  `scan_context`, read only as a plain file inside the repository, links and junctions neither read nor
  entered. **No taint** (decision and residual risk in PITFALLS).
- **Multi-path / grouped** (step 4): `read_file` takes `paths` (<= 20, 160,000 characters in all, per-file
  entries and errors, duplicates once, `path` + `paths` both read; the single-`path` shape, results and errors
  byte-identical, pinned by a test); each file is masked by its own name and taints per file actually read
  (inside the tool, skipped for a review's own reads); `search_files` takes `group_by_file` (opt-in; per-file text in
  grep's `N:` / `N-` format with merged context; the Phase 3 context factor applies; the flat shape is the
  default). Schemas changed (`path` no longer required): a one-time cache miss. `compress.rs` and ACP titles
  know a `paths` call.
Files: `crates/runtime/src/{context_pack.rs,context_pack_tests.rs,preinject.rs,budget.rs,lib.rs,compress.rs}`,
`crates/runtime/Cargo.toml` (`ignore`, already in the lock file), `crates/runtime/tests/turns.rs` (9 new tests; two
Phase 8 tests run with the knob off), `crates/tools/src/{files.rs,search.rs,lib.rs}`,
`crates/prompts/src/{system.rs,text.rs}`, `prompts/tools/{read_file,search_files}.json` + `README.md`,
`crates/desktop/src/acp/tools.rs`, `scripts/perf/{context-pack-check.mjs,README.md}`, `PORTING.md`, `GAPS.md`,
`docs/{ARCHITECTURE,PITFALLS,TESTING}.md`.
Tests (all with `CARGO_TARGET_DIR=target/claude`, 2026-11-13): `cargo test -p hermes_tools -p hermes_memory -p hermes_store -p hermes_prompts -p hermes_llm` all ok (hermes_tools 325, hermes_llm 468, hermes_memory 101, hermes_prompts 24); `hermes_runtime --lib` 199 ok (19 new in `context_pack`); `--test turns` 221 ok under the default, `balanced`, `requests` and `tokens` profiles (9 new); `hermes-desktop` 176 + 68 ok (empty `target/hermes-src` placeholder, removed after); `cargo build --workspace` ok. Live check `scripts/perf/context-pack-check.mjs` against my own instance (port 47810, data in target/perf): all 36 checks passed; instance stopped. Not done: UI not driven.
Request counts: unchanged for every existing scenario (S1 stays 5 in `baseline`); new: S1 under `requests` is
**2** main requests with one `run_script` (reads three files, edits one), **3** with one multi-path `read_file`
then the edit, **3** with three parallel `read_file` calls then the edit; "give me an overview of this repository"
in a fresh repository session is **1** main request (the pack answers what it would have listed).
Deviations from the plan:
- *AGENTS.md / CLAUDE.md heads are not in the pack*: the system prompt already loads the first of
  `.hermes.md` / `AGENTS.md` / `CLAUDE.md` into its project context, so the pack lists their names and sizes and
  says so; only the README head is quoted (PITFALLS).
- *A parallel-calls text already existed* (Hermes's `PARALLEL_TOOL_CALL_GUIDANCE`); step 2 adds one sentence
  after it (the script and `paths`) instead of a second paragraph of the same advice.
- *The cache key includes a status fingerprint* (not HEAD alone): the tree and README can change without a
  commit; the cost is one recompute when the status changes.
- *No taint* for the pack (the brief asked to decide): README / commit text is untrusted but scanned, escaped and
  labelled; residual risk in PITFALLS.
- Two Phase 8 tests (`a_repositorys_skill_is_attached_in_its_own_sessions_and_never_in_another`,
  `a_hostile_project_skill_is_never_loaded_or_attached`) now run with `context_pack: false`: their fixture
  repositories are directories with a `.git` folder, the pack's tree names the skill folders they assert
  absent, and repo-b's first message was asserted to be exactly what was typed.
For the reviewer:
1. **Taint decision.** Read the README / subject path in `context_pack.rs` and PITFALLS: a prompt injection in
   the first 2,000 README characters or in the last 5 commit subjects reaches the model without a taint (only
   `scan_context`'s patterns and the escaping stand in the way). Is "no taint" right, or should a non-empty
   README head / subject taint (one line in `begin_turn`)?
2. **`git` in a hostile repository.** `core.fsmonitor=false` is the one known "status runs a repository
   program" route and a test covers it with a real config; check for others (`core.sshCommand` is not used by
   these commands, `diff.external` / textconv are not used by `status --short`, a `.gitattributes` filter does
   not run for `status`... but `core.untrackedCache`, `status.*` config, `include.path`?). The `safe.directory`
   check is relied on (a foreign-owner repository makes git refuse). `git` is found on PATH as any process would.
3. **Cache.** The key is root + HEAD + status text; the status is read before the cache lookup on every first
   message (a `status` on a very large repository costs up to the 4 s limit, then the git parts are left out,
   nothing cached). A file added inside an already-untracked folder is missed by the cached tree until the next
   status change (documented). `<data>/cache/context-pack/` is not cleaned by anything else and not part of
   the shared-store schema (rule 6).
4. **Size.** 14,000 characters (about 3,500 tokens) once per session is the price in every first message in a
   repository under `requests`; `budget.context_pack: false` removes it. Check the cut order in `assemble`
   (tree end first, then the README tail; the git parts are never cut) and `fit_lines` marks.
5. **The walk.** `filter_entry` counts entries per folder (first 40 kept, a rejected folder is not entered);
   `ignore` still reads and sorts a huge folder's names (100k names is fast, but it is O(n) on the first
   message). `.gitignore` is honoured with `require_git(false)` (so a nested non-git folder's `.gitignore`
   counts) and `parents(false)`; global gitignore off. Hidden files are listed except `.git`.
6. **`read_file` `paths`.** `path` is no longer `required` in the schema (a model that sends neither gets the
   old "missing field `path`" error). The `files` result has no top-level `content`: anything reading
   `output.value["content"]` for `read_file` (hints, signals, delegation summaries, the page's tool row) sees
   none for a multi call; I handled `compress.rs` and the ACP title, the page renders the raw JSON. Taint is
   applied inside the tool (not in `taint_after_call`), which is also run by subagents' tools.
7. **`begin_turn` condition.** "No user message in the conversation" is read from `open.conversation`
   (hidden messages count); a session imported or branched with messages but whose first user message has no
   pack gets none (it is not a first message). ACP sessions get the pack too.
8. The new schema text is a one-time cache miss (the tool list is part of the prefix), as is the
   new system-prompt sentence for sessions created after the upgrade (existing sessions keep their stored prompt).
9. Wall-time: the pack tests create real git repositories and a 6,000-file tree (a few seconds each); the
   whole `context_pack` test module runs in about 10 s.

**User checklist (Phase 10, CP10.1 + CP10.2)** — after building and starting your own copy of the app
(Settings ▸ Model ▸ Budget profile = `requests`, the default):
1. Start a **new chat whose folder is a git repository** (for example this project's folder) and send
   "give me an overview of this repository". The agent should answer straight away from what it already has
   (branch, recent commits, changed files, file tree, README) without running tools first; most models do,
   a model may still look at one or two files. Note the request gauge in the status bar before and after:
   it should go up by one (plus one more per tool round if the model chose to look anyway).
2. Look at your message in the chat: it shows only what you typed, not the repository overview. Open another
   chat and come back to this one: still only your words. Send a second message: the agent does not get a
   second overview (nothing for you to see; the answer just doesn't restate the tree).
3. Ask for something that needs several files, e.g. "read Cargo.toml, README.md and AGENTS.md and summarise
   how they fit". The transcript should show the reads together in one step (several tool rows at once, one
   `read_file` row naming several files, or one script row) rather than one read per answer.
4. Set the approval mode to **Accept edits** (in **Manual** every script asks, by design; in **Auto** an
   approval model may decide instead of you). Ask
   "count the lines of every .rs file and show the five biggest". If the agent uses a script, it runs without
   asking. Then make a throwaway folder `scratch-test` with a file in it inside the repository and ask
   "delete the scratch-test folder and everything in it with a PowerShell script (run_script)": a
   **`run_script (powershell)`** approval card appears with the whole script; press **Deny** and check that
   `scratch-test` is still there (delete it yourself afterwards).
5. Start a new chat in a folder that is **not** a git repository (e.g. an empty folder on the Desktop), or
   switch Settings ▸ Model ▸ Budget profile to `balanced` or `tokens` and start a new chat in the repository:
   ask the same question; the agent now has to look around with tools first (more than one request).
   Switch the profile back to `requests` afterwards.
6. To switch the overview off while keeping `requests`: quit the app, open `%APPDATA%\hermes-rs\desktop-config.json`
   in a text editor, and inside `"budget": { ... }` add `"context_pack": false` (comma-separated from the
   other keys); start the app again. New chats no longer get it. Remove the line to go back. There is no
   Settings switch for it.
7. A repository you did not write: the overview is presented to the agent as data, not instructions, and the
   usual approvals still apply; to be safe, switch the overview off for such repositories (step 6), as for
   their `AGENTS.md` (see PITFALLS "The first-turn context pack").


### Review (Opus 5.5)
Main had not moved (no merge). `/code-review` not available in the subagent; manual review, then each
claim tried: hostile git configs with a control run of plain git, a 200,002-file repository, the real
`turns` harness for byte identity, the multi-path read through the write guard, the review and the hints.

Found and fixed:
1. **`git status` ran a hostile repository's program through a `.gitattributes` filter.** Only
   `core.fsmonitor` was switched off. Verified with git 2.52: `* filter=evil` plus `filter.evil.clean` in
   `.git/config` runs the command when `status` meets a tracked file whose stat data changed (a touched
   file). Now every call sets `GIT_ATTR_SOURCE` to the empty tree (no attributes, so no filter; git 2.42+,
   older git ignores it), `-c color.ui=false`, and drops every inherited `GIT_*` variable (`GIT_DIR`,
   `GIT_CONFIG_*`, `GIT_EXTERNAL_DIFF`...). Test with a control (`a_repositorys_clean_filter_is_not_run`).
   Enumerated the rest (PITFALLS): hooks do not run for `status`/`log`/`rev-parse` and nothing is written
   (`--no-optional-locks`, so no `post-index-change`); `diff.external`/textconv are `diff`-only;
   `core.sshCommand`/credential helpers network-only; `gpg.program` not reached (`--no-show-signature`,
   `%s`); `include.path` only adds config. System/global config kept (the user's `safe.directory` still
   refuses foreign repositories). Side effect, documented: a stat-dirty Git LFS file shows as modified.
   Context: the page's coding rail already runs a plain `git status` in the same folder.
2. **A hang that outlived the timeout.** `run_limited` joined the stdout reader after the process exited;
   a program git started that inherited the pipe kept it open, and the join had no limit (the blocking
   thread leaked forever; the turn itself was saved by the 12 s outer timeout). Now waited for within the
   limit only. Test: `cmd /c start /B powershell Start-Sleep` returns at the limit.
3. **Multi-path reads were invisible to three consumers** that read only `args.path`: the background
   review's read-before-patch guard (a skill read via `paths` was refused as unread), the subdirectory
   `AGENTS.md` hints (no hint for a file read via `paths`), the delegation summary's `files_read`. All
   three now take each path; ACP gives one location per path. Tests for the review and the hints, and
   that a `paths` read satisfies the session's write guard per file (and an unread file still refuses).
4. Cache: two writers of the same key in one process shared a temporary name (now numbered); a crash's
   temporary files were never swept (now: older than an hour, on the next write; test).
5. Tree: the user's global excludes (`core.excludesFile`) are now honoured, as `git status` does.
6. Small: a literal newline inside a string in `search.rs`; the dead-code warning on
   `script_guard::Analysis::flagged` (test-only now); mojibake (`â€”`) and CRLF in this note.

Verified as claimed: the pack only in the first user message's `<context>` block, never the system
prompt; stored and replayed byte for byte after a reopen and a rewind (turns harness); no second pack;
page/resume/rewind/session search show the typed text; `balanced`/`tokens` add nothing (the knob defaults
off, `inject` and the index path unchanged when it is); in front of retrieved skills in one block; cache
key root + HEAD + status (a commit, branch switch or status change is a new key; corrupt, oversized,
wrong-key entries recomputed; 32 files, 64 KB; atomic rename); 14,000-character cap with tree-then-README
cut order and marks; tags escaped, also over cached text; README via plain-file + inside-root checks
(symlink not read, directory skipped, binary skipped, 8 KB read); junctions not entered; empty repo,
detached HEAD, worktree, subfolder cwd, no git; all git in `spawn_blocking` with per-call and overall
limits; `read_file` single-path results and errors byte-identical (pinned), `paths` bounded (20 files,
160,000 characters), per-file masking and taint; `group_by_file` opt-in, default unchanged. A bare
repository is not found as a root (no `.git` entry): no pack, by design.

**Huge repository** (200,002 files: 150 folders x 1,000 plus a flat folder of 50,000, committed, debug
build): first pack 2.8-3.4 s cold (git status + the walk, which reads the 50,000-name folder once), 0.36-0.44 s
from the cache; the walk alone 0.75 s; the tree fell back to depth 2 (87 lines) and said so; the pack 1,717
characters. Bounded by the 4 s git limit, 20,000 visited entries and the 8 s / 12 s deadlines.

**Taint decision: no taint, confirmed** (reasoning written into PITFALLS): taint gates smart approval,
outside writes and memory/skill staging; a README exists in practically every repository, so tainting on
it would stage self-improvement and disable smart approval in nearly every coding session under the
default profile, emptying the signal. The README comes from the same authors as `AGENTS.md`, which the
system prompt already loads as instructions after the same scan without tainting; the pack labels its text
as data. Project skills taint because they are procedures the model is told to follow, with scripts.
The pack is also unconditional on the first message (a "hi" pays ~3,500 tokens once, then cached): kept,
as the plan says, documented in PHASES.

**Phase 8 tests with `context_pack: false`:** not a pack bug. The fixtures are folders with an empty
`.git` directory (git refuses them, so the pack has the tree only); the tests assert that repository B's
first message is exactly what was typed and that a hostile skill's folder name appears nowhere. The pack's
tree listing `.agents/skills/<name>/` names is correct behaviour (names only, escaped).

Request counts (unchanged from the implementer's): S1 under `requests` 2 with one `run_script`, 3 with
`paths` or parallel reads (baseline 5); "overview of this repository" 1. These counts are what the mock is
scripted to send; what the tests prove is that the batched calls work and that the pack carries what an
overview needs, not that a real model batches (written into the Baselines).

User checklist above rewritten for a non-expert (where the gauge is, which approval mode, a throwaway
folder for the deny test, the config file's real name and place, no Settings switch).

Tests (`CARGO_TARGET_DIR=target/claude`, 2026-11-14): `cargo test -p hermes_tools -p hermes_memory -p
hermes_store -p hermes_prompts -p hermes_llm` all ok (hermes_tools 327); `hermes_runtime --lib` 200 ok;
`--test turns` 221 ok under the default, `balanced`, `requests` and `tokens`; `hermes-desktop` 176 + 68 ok
(empty `target/hermes-src` placeholder, removed after); `cargo build --workspace` ok. Live check
`context-pack-check.mjs` against my own instance (port 47810): all 32 checks passed; instance stopped.
(`perf.ps1 -Do stop` looks only under the main checkout's `target\claude`; the worktree's instance was
stopped by its path.) UI not driven.
