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
