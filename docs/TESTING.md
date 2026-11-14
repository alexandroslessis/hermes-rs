# Testing

Every feature ships with three layers of evidence, in this order of preference: unit tests in the
module, a runtime turn test when the agent loop is involved, and a live check script against a
running instance of the app when the page calls it. Then the user gets a short checklist for what
only a human can see.

Always build into `target/claude` (`$env:CARGO_TARGET_DIR = "target/claude"`, from the repo root
or as an absolute path). The user's own build locks `target/debug`.

## 1. Unit tests (in the module)

- A `#[cfg(test)] mod tests { use super::*; … }` at the bottom of the module you changed.
- Test the plain functions the gateway calls (that's why logic lives outside `gateway.rs`): give
  them a `tempfile::tempdir()` as the data folder or the working folder, call them, assert on the
  returned `Value` / file contents.
- Name tests as sentences describing behaviour: `a_disabled_toolset_is_not_offered`,
  `branching_keeps_tool_calls`, `credential_files_are_refused`.
- Cover: the happy path, the refusal paths (bad input, missing file, path outside the folder), and
  every decision the module doc mentions (each "unlike Hermes, …" deserves a test).
- Git-touching tests make their own repo in a tempdir and set `user.name`/`user.email` and the
  branch name explicitly (the machine's global git config sets `init.defaultBranch=master`).
- Run: `cargo test -p hermes-desktop <filter>` (or the crate you changed).

## 2. Runtime turn tests (`crates/runtime/tests/turns.rs`)

For anything that changes what happens in a turn (tools offered, guards, delegation, steering,
compression, side questions). The harness is a `wiremock` server that plays a scripted
OpenAI-compatible model:

```rust
#[tokio::test]
async fn switched_off_tools_are_not_offered_and_refused() {   // abridged from turns.rs
    let h = harness(vec![
        tool_reply(&[("t1", "terminal", json!({"command": "echo hi"}))]),
        text_reply("I couldn't run it."),
    ])
    .await;
    h.runtime.set_disabled_tools(["terminal".to_string()]);
    let session = session_in(&h);
    let events = run_turn(&h, &session, "run echo hi").await;
    assert_eq!(finished_status(&events), TurnStatus::Completed);
    let requests = h.requests.lock().clone();
    // What the model was offered, and the tool result it saw on the next request.
    assert!(!requests[0]["tools"].to_string().contains("\"terminal\""));
    assert!(requests[1].to_string().contains("turned off by the user"));
}
```

Read the existing tests near the area you change before writing a new one; the helpers
(`session_in`, `finished_status`, `last_text`, `sent_messages`) are defined in the same file.

- `harness(replies)` → `Harness { runtime, provider, requests, data, work }`; each reply is
  consumed by one chat-completions request, in order (`text_reply`, `tool_reply`, or `sse(...)`
  with `delta(...)` chunks for odd cases). Run out of replies and the mock answers 500.
- `run_turn(&h, session, text)` returns all `RuntimeEvent`s up to `TurnFinished`.
- `h.requests` holds every request body: assert on what the model was sent (`sent_messages`,
  `request["tools"]`). `last_text(&h, session)` reads the stored reply.
- Run: `cargo test -p hermes_runtime --test turns`. The harness runs `balanced` unless
  `HERMES_TEST_PROFILE=requests|tokens` says otherwise (the whole suite passes under each; a test that
  depends on one profile's caps or prompt wording names it with `harness_profile`). The profile also
  decides the system prompt's wording (stored with the session at creation), so read a session's prompt
  through `h.runtime.store().session(id)`.
- Pre-injection tests (`s6_*`, `a_skill_is_attached_once_...`, `resuming_and_rewinding_...`) use the bundled
  `arxiv` skill (`ARXIV_ASK`) under `harness_profile(.., Requests)`; helpers `user_contents(request)` (the user
  messages of a request as text) and `system_of(request)`. The whole suite also passes under
  `HERMES_TEST_PROFILE=requests`, where messages the index matches get a `<context>` block.
- Review scheduling tests (Phase 7 CP7.3, after the `// Phase 7 CP7.3` banner): `review_budget(profile,
  mode, idle)` / `idle_once(idle)` set a sub-second idle time on the knob directly (the config refuses
  under 3 s), `scheduled_harness` holds the `review` replies back with a delay, `wait_for` polls
  `request_count` or `h.runtime.review_schedule_status(id)`. Tests that run the evidence review by hand
  or count requests around corrections use `quiet_budget()` (`agentic`): under `balanced` two
  corrections now start an agentic review on their own. The scheduled tests wait up to 60 s: with the
  whole suite running, the first index refresh and skill git commits of a fresh data folder are slow.
  To make an agentic review fail at once, script a broken stream (`"data: {not json\n\ndata: [DONE]\n\n"`):
  a `REFUSE_400` makes it keep retrying across the following turns. An app restart is a second
  `runtime_with_budget(h.data.path(), ..)` dropped mid-review (`an_exit_mid_review_...`,
  `reviews_cut_off_twice_...`).
- Curator v2 tests (Phase 8 CP8.3, after the `// Phase 8, CP8.3` banner): `curator_harness(replies,
  per_cluster_budget(n))` scripts the `curator` replies, `two_clusters` / `deploy_pair` make the library
  (`procedure_skill` builds skills whose shared steps make them near-duplicates), `run_curator_now` runs
  the curator with the mock as its model, and `h.runtime.staging().list()` is where proposals appear.
  Clustering, the contract and `deliver` are unit tests in `runtime/src/curator/tests.rs`
  (`bundled_skills_do_not_cluster` reads `assets/skills`; `hermes_full_skill_set_is_reported` reads
  `../hermes-agent` when it is there and never fails). Tests of the agent-loop curator pin `Balanced`.
- Memory v2 tests (Phase 9 CP9.3, after the `// ---- memory retrieval, dedupe, hygiene` banner):
  `archive_scenario(profile)` is turn 1 writing an archive entry through the tool and turn 2 asking
  about it (3 requests under every profile; only `requests` attaches it, in the user message);
  `memory_budget(profile)` is the profile's knobs with no review of its own. The pieces are unit
  tests: `hermes_memory` `retrieval_tests.rs` (dedupe, confirmation, generation, batched hits,
  hygiene candidates, `demote_core`), `hermes_tools` `index_memory_tests.rs` (retrieval goldens on a
  fixture, a 240-entry archive that small talk and vague requests leave alone, two repositories'
  isolation incl. the filter-before-limit case, freshness across stores,
  skill boost, schema bump), `runtime/src/memory_hygiene.rs` and `review_once/tests.rs` (supersedes),
  `desktop/src/learning.rs` (link edges, dangling links, no project memory). The curator turns test
  backdates `memory.db` rows with `rusqlite` (a dev-dependency of the runtime only for that).
- `run_script` tests (Phase 10 CP10.1): the analysis is unit-tested in `crates/tools/src/script_guard.rs`
  (one list per smuggling construct and language, continuation joins, benign scripts that must pass);
  approval and execution in `crates/tools/src/script.rs` (every mode, taint, Yolo warning, persistence,
  outside paths, remembered approvals ignored; real PowerShell/uv/Git Bash runs that skip themselves
  when the interpreter is missing: output, exit code, stderr, size limit, timeout kill, stopping the
  turn, dropping the call, scratch cleanup, secret variables not inherited). `turns.rs` has a
  `// ---- Phase 10, CP10.1` banner: `script_call(id, language, code)` builds the model's reply,
  `tool_result_texts` reads the results back, and `one_run_script_call_does_the_work_of_three_tool_calls`
  holds the request counts (4 main requests with three terminal calls, 2 with one script).
  The tests use PowerShell (always present on Windows); no live check: the page calls nothing new.
- Context pack, multi-path reads and batching guidance (Phase 10 CP10.2): `crates/runtime/src/context_pack_tests.rs`
  builds real git repositories in temp folders (skipped when `git` is missing): sections, cache per HEAD +
  status (a sentinel put into the cache file shows through; a commit or a status change replaces it; corrupt,
  oversized and wrong-key entries are recomputed; hand-edited entries cannot forge tags), a 6,000-file tree
  with a 3,000-file folder (size, lines and time bounded), git missing, a process that sleeps or floods
  (`run_limited`), hostile README / commit subject, links and junctions not followed, empty repository,
  detached HEAD, a linked worktree, a repository's `core.fsmonitor` and `.gitattributes` clean filter not
  run (each with a control: plain `git status` runs it), a process that exits leaving a program holding its
  output pipe, stale temporary cache files swept, the cap under oversized parts.
  `turns.rs` has a `// ---- Phase 10, CP10.2` banner: the pack in the first message only (not in the system
  prompt, stored, replayed byte for byte after a reopen and a rewind, not tainting), none outside a
  repository or under `balanced` / `tokens` / the knob off, in front of the attached skills, cached across
  sessions, a hostile README, S1 in 2 requests (one script) and 3 (multi-path or parallel reads) against the
  baseline's 5, taint per path of a multi-path read, the guidance lines in each profile's frozen prompt.
  Two Phase 8 tests (`a_repositorys_skill_is_attached...`, `a_hostile_project_skill...`) run with
  `context_pack: false`: their repositories are fixtures and the pack's tree would name the skill folders.
  Live check: `scripts/perf/context-pack-check.mjs`.
- Known flaky under heavy load: `an_exhausted_budget_ends_with_a_toolless_summary` (timing).
  Re-run it alone before suspecting your change.

## 3. Live checks against your own instance (`scripts/perf/*-check.mjs`)

The page talks JSON-RPC and REST; a check script talks the same protocol to a real, running
`hermes-desktop` so the whole path (server, gateway, runtime, store, side files) is exercised.

### The instance

`scripts/perf/perf.ps1` starts a **separate** copy of the app: data in `target/perf/home`
(`HERMES_RS_HOME`), port **47810** (`HERMES_DESKTOP_PORT`), its own WebView2 profile
(`target/perf/wv2`), DevTools on **9223**, wire logging on, provider
`http://127.0.0.1:47900/v1` (model `mock`).

```powershell
$env:CARGO_TARGET_DIR = "target/claude"
cargo build -p hermes-desktop                       # cargo test does NOT rebuild the exe
$env:HERMES_PERF_EXE = "$PWD\target\claude\debug\hermes-desktop.exe"
powershell -File scripts/perf/perf.ps1 -Do start
tools/node/node.exe scripts/perf/<feature>-check.mjs 47810
powershell -File scripts/perf/perf.ps1 -Do stop     # stops only that exe's process tree
```

- The provider on :47900 is either `mock_llm.py` (a generic streaming model, for perf) or —
  usually for checks — **the check script itself**, which embeds a tiny provider tailored to the
  feature. Stop `mock_llm.py` first if it's running; the port must be free.
- The instance's data persists between runs (`target/perf/home`). Checks must not assume an
  empty store: use unique titles/names (`Date.now()`) and find "their" session by id.
- Logs: `target/perf/app.out` (stdout, wire log of every frame: `→`/`←`) and `app.log`.
- To test the importer, set `HERMES_HOME` to a home made by `scripts/perf/make-hermes-db.py`
  before `-Do start`.

### Writing a check script

Copy the skeleton of `toolsets-check.mjs` (provider + socket + rest helpers) and keep its shape:

```js
// <What it checks> against a running hermes-desktop (the perf instance, provider
// http://127.0.0.1:47900/v1): this script is that provider (stop mock_llm.py first). <steps>.
//   node scripts/perf/<feature>-check.mjs [port]
import http from 'node:http'

const port = process.argv[2] ?? '47810'
const base = `http://127.0.0.1:${port}`
let failed = 0
const check = (label, ok, detail) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}…`); if (!ok) failed++ }
// rest(method, path, body) → {status, body};  rpc(method, params) over ws://…/api/ws
// with subprotocol 'hermes-gateway-ticket.<anything>';  events[] collects 'event' notifications
// …the provider: GET → model list; non-stream POST → a title; stream POST → scripted SSE…
// …steps: each followed by check('what the user would see', condition, detail)…
console.log(failed ? `${failed} FAILED` : 'all ok')
process.exit(failed ? 1 : 0)
```

Rules:
- Check what the page relies on: response shapes (field names exactly as the contract), status
  codes, events and their order, and the side effects (a file written, a setting kept after
  re-reading it, the next turn's request to the provider).
- Wait for events with a polling helper and a timeout, never fixed sleeps alone.
- Leave the instance as you found it (switch settings back, delete what you made where possible).
- Add a sentence for the new script to `scripts/perf/README.md`.

### Page checks without screenshots

Policy: never screenshot or drive the **user's** app. For your own instance, read the page as
text/DOM through DevTools:

- `tools/node/node.exe scripts/perf/cdp.mjs "<js expression>"` — e.g.
  `"location.hash='#/settings'"`, then
  `"document.querySelector('[data-tour=\"nav-config:memory\"]')?.offsetParent !== null"` to check
  a section is visible, or `"document.body.innerText.includes('Star map')"`.
- `tools/node/node.exe scripts/perf/hover.mjs "<selector|aria-label|text>" [click]` — hover or
  click and get the boxes of popovers/tooltips that appear (positioning bugs).
- The page routes by hash: navigate with `location.hash = '#/…'`, not `pushState`.
- **A patch is checked by building it, not by `git apply --check` alone.** In a worktree:
  `target/hermes-src` may be a junction to the main checkout's (gitignored) and
  `build-hermes-webapp.ps1` starts by deleting that folder, so first remove the junction with
  `cmd /c rmdir <worktree>\target\hermes-src` (never a recursive delete through it); `tools/` is
  gitignored too, so link Node in with `cmd /c mklink /J <worktree>\tools <main>\tools` (remove it
  the same way afterwards). Then run the script (its `npm ci` needs the network the first time) and
  type-check: `node <src>\.build\webapp-workspace\node_modules\typescript\bin\tsc -p . --noEmit` in
  `<src>\.build\webapp-workspace\apps\desktop` (Hermes's own test files have ~28 errors at the
  pinned commit; look for the files your patch touches). To rebuild after editing one patch
  without a fresh export: re-apply it in `target/hermes-src` (with `GIT_CEILING_DIRECTORIES` set to
  `target\`, see PITFALLS) and run `node scripts/build-webapp.mjs` in its `apps\desktop`.
- After editing a patch, `hide.css` or `plugin.js`: rebuild the webapp (patches), then always
  the exe (`cargo build`: it recopies the webapp next to the exe, and `hide.css` / `plugin.js`
  are `include_str!`'d), then restart the instance.

## 4. The user's checklist

After anything visible, end your report with a numbered list the user can follow in their own
app in two minutes: where to click, what to type, what they should see. Include one negative case
("switch Terminal off, ask for `dir`: the reply says the tool is switched off"). Say which build to
run (they must rebuild/restart their app; your `target/claude` build isn't theirs).

## Before you commit

```powershell
cargo build --workspace                       # the frozen GPUI app still compiles
cargo test -p hermes-desktop
cargo test -p <every crate you touched>
cargo test -p hermes_runtime --test turns     # if the runtime changed
# live: the feature's check script, plus any check whose area you touched (all print "all ok")
```

Report results honestly in the commit body's **Checks:** paragraph: which tests and scripts ran
and passed, and anything you could not run.
