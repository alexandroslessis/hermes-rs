# Testing

Every feature ships with three layers of evidence, in this order of preference: unit tests in the
module, a runtime turn test when the agent loop is involved, and a live check script against a
running instance of the app when the page calls it. Then the user gets a short checklist for what
only a human can see.

Always build into `target/claude` (`$env:CARGO_TARGET_DIR = "target/claude"`, from the repo root
or as an absolute path). The user's own build locks `target/debug`.

## 1. Unit tests (in the module)

- A `#[cfg(test)] mod tests { use super::*; … }` at the bottom of the module you changed.
- Test the plain functions the gateway calls (that's why logic lives outside `gateway/`): give
  them a `hermes_testdir::tempdir()` (section 5) as the data folder or the working folder, call them, assert on the
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
  `core/src/learning.rs` and `desktop/src/learning.rs` (the graph JSON; link edges, dangling links, no project memory). The curator turns test
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
- Short tool schemas (Phase 11 CP11.1): `hermes_prompts` `tools.rs` tests (byte budgets per schema and for the
  whole block, the shape snapshot `crates/prompts/tests/fixtures/tool_shapes.json`, the phrases a call depends
  on, the pointer sentence), `hermes_tools` `skills.rs` `the_tools_guide_is_bundled_and_documents_every_parameter...`
  (every property name of the four schemas appears in the skill), `turns.rs`
  `the_long_tool_schemas_are_short_in_every_profile` (bytes of the request's `tools` in each profile, one
  request). The whole `turns` suite passes unchanged under each profile: request counts did not move.
- Deferred tools (Phase 11 CP11.2): `hermes_prompts` `deferred.rs` (block, strip, size caps, the skill_manage rewrites), `hermes_runtime`
  `deferred.rs` (`loaded_in`: order, direct calls, unanswered calls, too many names, truncation), and in `turns.rs` (after the `CP11.2` banner):
  `other_profiles_send_every_tool_unchanged` (18 tools, same list every request, schema text, size pinned), `tokens_sends_the_core_tools_and_tool_search_only`
  (the list, the byte saving against a threshold computed from the schema files, the prompt, no `skill_manage` anywhere), the flow
  `tool_search_loads_a_tool_for_the_next_request_and_costs_one_round_trip` (3 requests vs 2), the direct call, unknown / already loaded / too many
  names / query, restart + rewind, compaction, switched-off tools, MCP tools past N (a `Many` mock server), the review with `skill_manage`, a subagent, a
  profile change mid-session; and (CP11.2 review) `a_compaction_or_a_model_switch_keeps_the_kind_of_tool_list`, `a_copied_deferred_block_in_a_context_file_changes_nothing`,
  `the_core_mcp_tools_are_the_ones_the_session_started_with`, `the_context_breakdown_counts_the_tools_a_deferred_session_sends`. Live:
  `scripts/perf/deferred-tools-check.mjs`. Under `HERMES_TEST_PROFILE=tokens` the suite keeps `tokens`' other knobs but **not** its deferred list
  (`test_budget(None)` sets `tool_schemas: All`: the other tests call deferred tools directly); the deferral is covered by the tests that name `Tokens`.
- Provider fallback (Phase 12 CP12.1): classification and config validation are unit tests (`crates/llm/src/fallback.rs`: real OpenRouter /
  OpenAI / Ollama bodies and Retry-After values; `crates/core/src/fallback.rs`; `crates/runtime/src/fallback.rs`: the gate, the cap, the fit
  limit). `turns.rs` (after the `Phase 12` banner) runs two or three mock providers, `duo(a_handler, b_handler, http_retries)`, each recording
  (purpose, body) per request: the daily 429 -> b with the same messages and tools, the notice and the ledger on both; the next turn skips a known
  used-up primary; 5xx after the retries (counts 2 and 1), the next turn re-probes; a tool call after the switch stays on b and runs once; a
  missing model (404 and 400 wordings); 400 / 401 / 403 / 422 / per-minute 429 / plain 404 never reach b (zero requests); our limit at 0 goes
  to b with no request to a, b's own limit refuses the turn, a used-up b is skipped for c; a chain that runs out shows the first error plus
  `Fallback chain:`; an auth error on b is shown; a stream that breaks half way (bad JSON, or an in-stream server error that would switch had it
  come first) is not re-asked and nothing is duplicated; an in-stream error as the FIRST frame does move to b; a working primary
  makes the same requests as without a chain; a smaller window compresses first (the summary request goes to b) and the real request fits; a
  fallback nothing can make fit is skipped and never sent the request; a session resumes on the primary after a restart.
  Live: `scripts/perf/fallback-check.mjs`.
- The store's writer (Phase 13 CP13b): `crates/store/src/writer/tests.rs` (order, a waiting write after queued ones,
  read-after-write from both connections and another thread, another `Store` on the file after a flush, batches, the
  256 cap, superseded upserts, failures per session and reported once, a refused id, a commit that fails, a write
  that ends the batch's transaction (nothing run twice), a write that panics, drop and `shutdown` draining, a write
  during another thread's shutdown landing after the queue, a delete keeping the writer's own failure, many threads, and a killed process: the test binary runs itself with
  `HERMES_STORE_CRASH_DB` set, flushes, queues more and exits without destructors). To fill a batch on purpose a
  test holds `store.writer.write.lock()` while it queues. `turns.rs` (after the `CP13b` banner): a finished turn is
  readable from another connection, and a write the database refuses (a test trigger) fails the turn with the reason.
- No blocking work on an async worker (Phase 13 CP13c): `crates/tools/src/blocking_tests.rs`. One-thread
  (`current_thread`) runtime, a timer task ticking every 20 ms beside the tool call, a test-only hook
  (`slow::set(session_id, delay)`, honoured by `test_pause` inside each blocking closure) making the tool's
  disk/database work take 300 ms; the longest tick gap must stay under 150 ms (generous: a blocked worker shows
  300 ms or more) and the call must have taken at least the delay (the hook really ran in the tool). Covers
  `memory`, a staged `memory` write, `session_search` (discover and browse), `skills_list`, `skill_manage`,
  `skill_view`; `a_blocked_worker_is_seen_by_the_timer` is the control. It measures time: if it fails only on
  a very loaded machine, re-run alone before suspecting the code. The same file checks
  `off_worker_unless_stopped` (the tool follow-ups' hop): raced against Stop as `runtime/agent_loop.rs` races a
  check, it keeps the result where a plain `spawn_blocking` hop loses it, and without Stop it leaves the worker free.
- The session actor (Phase 13 CP13d): `crates/runtime/src/session_actor/tests.rs` (unit tests over a real actor
  thread: order from one and many senders, a barrier, answers from a panicking handler and a stopped actor,
  commands queued when its thread ends, re-entrancy refused, the thread ending with its last handle, the registry,
  a waiting call on a one-thread runtime; the run slot (`RunGuard` claim / drop, stale releases, a panicking turn's
  task), Stop's priority path while the actor is held up, the review pause (`IdleEpoch` / `ConfirmIdle`), the
  review slot's rules and the scheduler pair raced 300 times; the page's queue order, `only_if_running`, live,
  model pick, effort, revision, images, close vs delete; from the review: a pick after the turn ended drops the
  one it left, a dead actor is replaced and its turn stopped, a losing load gets its copy back).
  `session::testing::open_session` makes an `OpenSession`. The `turns` suite and the desktop / ACP tests run
  unchanged on top.
- The agent loop (Phase 13 CP13e): `crates/runtime/src/agent_loop_tests.rs` asks each check of `ops.rs` at its
  retry / continuation boundary under the `balanced`, `requests` and `tokens` knobs and checks the `Next` it returns
  (truncated tool call with the growing output cap, invented tool names, malformed JSON, cut-off text and its
  stitching, the empty-reply ladder, the iteration budget's summary), plus Stop's "interrupted" answers; its
  `whole_loop` module runs `AgentLoop` itself over `session::testing::open_session` with a counting test provider
  and test tool providers (pass order: unanswered calls before a steer, a spent budget after pending calls; Stop
  during the deferred lookup; an error after Stop is a Stop, one without Stop skips finalize). Whole turns:
  `turns.rs`, unchanged. **Golden trace** for a change to the loop's plumbing: a throw-away patch that appends every
  provider request body (`ApiRequestBuilder::response_post`), every `AgentEvent` (`Emitter::emit`, minus
  `ToolProgress`) and every queued store write (`Store::queue`) to a file named after the current thread (a test's
  name; all `turns` tests are `current_thread`, so their spawned tasks log there too) when an env var names a
  folder; run the suite under each profile before and after, normalise ids, timestamps, temp names, ports and
  durations, and compare per test. Run the "before" twice: the tests whose order differs between two identical runs
  are the noise floor (concurrent tool calls, background reviews). CP13e's patch and comparer are described in
  `docs/backend/progress/phase-13.md`.
- The typed gateway dispatch (Phase 13 CP13f): `crates/desktop/src/gateway/rpc.rs` tests (every served name parses
  from any shape of params, a name that is not exactly served is `MethodNotFound` with the old text and no `data`, the `SERVED`
  list is the enum, the page's payloads land in the typed fields, a wrong-typed field reads as absent, errors keep codes,
  texts and `data`) and `gateway/rpc_table_tests.rs`: every method over a spread of params that need no session
  (absent, `{}`, a string, a number, a number / string / null in every field, plus payloads shaped like the page's), the
  code and text of every error and the shape of every success, compared with `tests/fixtures/rpc_table.txt` (900 lines,
  recorded on the code **before** CP13f); a second test sends the unserved names through `Gateway::call`. After changing
  a handler's answer on purpose: `HERMES_RPC_TABLE_BLESS=1 cargo test -p hermes-desktop --lib rpc_table` and read the
  fixture's diff. Live: `scripts/perf/rpc-dispatch-check.mjs` (all 80 methods over the socket). **Golden comparison**
  for a change to the dispatch itself (method in `docs/backend/progress/phase-13.md`, CP13f): a throw-away per-method
  gateway test over ~25 params per method (2,181 answers, sessions included), and a throw-away line in `handle` logging
  every request and reply frame to a file named by an env var while the live checks run; normalise ids, times, temp names
  and compare before and after. The table test above is the part that stayed.
- ACP tests must not count on a background job being late: `/retry` and `/undo` are learning signals, so under
  `balanced` a review may follow the turn (the desktop app ignores the test config's `background_review: false`),
  with the turn's tools and system prompt; filter its requests out (its last message is the review prompt). A test
  that stops a turn waits for the turn's request to reach the mock, not a fixed delay.
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

## 5. Temp folders and leaks

Tests make their folders with `hermes_testdir::tempdir()` (crate `crates/testdir`), not
`tempfile::tempdir()`, in `hermes_tools`, `hermes_runtime` and `hermes-desktop`; a fixture struct
declares the `TestDir` field last. Why, and the numbers: docs/PITFALLS.md, "Test folders that do not
go away" (a test run used to leave ~1.6 GB in `%TEMP%`; 300 GB over a few hundred runs).

After a round of test runs, sweep what is left (small database files a thread holds until the test
process exits):

```powershell
powershell -NoProfile -File scripts/clean-test-temp.ps1            # dry run: count and size
powershell -NoProfile -File scripts/clean-test-temp.ps1 -Delete    # remove
```

Do not redirect `TEMP` for a test run (the taint test treats paths under it as scratch). A line
`hermes_testdir: could not remove ...` in test output is a leak: fix the field order or the owner of
the open file.

## 6. Native UI (`crates/native`)

The native UI ([docs/native-ui/](native-ui/README.md)) has no protocol, so no `*-check.mjs`. Its
checks are three kinds, cheapest first, all run by one script:

```powershell
powershell -NoProfile -File scripts/native/check.ps1           # build (--features dev), tests, snapshots
powershell -NoProfile -File scripts/native/check.ps1 -Update   # accept the rendered snapshots
```

It builds into `target/claude`, sets `HERMES_RS_HOME=target/native-test`, opens no window, and
prints only failures, the `*.diff.png` / `*.actual.png` paths to look at, and a summary line.

**1. Layout checks (text; every change).** Name the elements a check needs with `.probe()` (any
element with an id; Kit's own controls such as `Button` are named by their id already;
`.probe_text(text)` on a one-line label also records the width its text needs). A test opens the
view in a headless window and asserts on the element dump of its last frame
(`crates/native/src/testing.rs`):

```rust
let mut cx = headless_app();                     // real DirectWrite text, Kit's assets
let window = open_window(&mut cx, size(px(800.), px(600.)), |_, cx| cx.new(|_| MyView));
let layout = layout(&mut cx, window.into());
let mut check = layout.check();
check.same_row("save", "cancel").same_height(&["save", "cancel"])
     .inside("save/caption", "save").no_overflow("toolbar")
     .background("save", theme.button_hover).text_color("save/caption", theme.foreground)
     .state("save", Some(true), None, Some(false));          // hovered, focused, disabled
check.assert();                                  // every failure at once, plus the dump
```

Names are an id or the last ids of a path (`"toolbar/save"`) and must match one element.
`Layout::of(window)` works inside `#[gpui_kit::test]` too, but `TestAppContext`'s text system is
GPUI's fixed-width fake: use `headless_app()` whenever a check depends on text width. Gallery
stories get there with `gallery::open_story(cx, story, state, theme)` (it applies the state's
hover, press or focus). The same dump comes from a running instance: build with
`--features dev`, then `--dump <file>` (written half a second after start) or F12 (writes
`layout-dump.txt` in `HERMES_RS_HOME`). One line per element, indented under its parent:

```
# window 160x60  pointer 65,28
story  0,0 130x56 bg=#ffffffff fg=#0a0a0aff hovered
  button  12,12 106x32 bg=#e5e5e580 hovered label="Save changes"
    label/caption  23,19 84x17.5 fg=#0a0a0aff text_w=83.4 hovered
```

`bg` is the element's own solid background (its computed style, else the topmost painted quad of
exactly its bounds, which is how a Kit button's is read), `fg` the text colour its content gets
(probed elements only), `hovered` the pointer over its hitbox. `cargo test -p hermes-native --lib
the_dump -- --nocapture` prints this one.

**2. Snapshots (images, no tokens unless something changed).** Each story registered in
`crates/native/src/gallery/mod.rs` (`register_stories!`) is rendered off-screen in each of its
states, in two themes (`Story::themes`: the CP0.2 and CP2.1 stories keep GPUI Kit's light and dark,
`-light.png` / `-dark.png`; every design-layer and frame story is rendered under t3code's own
themes, `-t3-light.png` / `-t3-dark.png`), cropped to the story's element, states side by side, and
compared with `crates/native/snapshots/<story>-<theme>.png` (a pixel counts as changed when a
channel moves by more than 8). On a difference the test writes `<name>.actual.png` and
`<name>.diff.png` (changed pixels red over the faded baseline) to `target/claude/snapshots/` and
fails; a story with no baseline writes only `.actual.png`. Images are at scale 2 (the test
window's). Run alone: `cargo test -p hermes-native --features snapshots`.

To accept: look at (or have a subagent look at) the `.actual.png`, then `check.ps1 -Update` (or
`UPDATE_SNAPSHOTS=1` on the cargo command) and commit the changed files in
`crates/native/snapshots/` with the change that caused them; the reviewer sees them in the diff.

**3. A subagent reviews the images.** Images never enter the orchestrating session. When
`check.ps1` lists diff or new images, a Haiku subagent opens only those (`.diff.png` first, then the
`.actual.png` beside it) and writes one verdict per image under the checkpoint in the progress note:
"expected: <what changed and why>" or "unexpected: <what is wrong, where>" (e.g. "state 3: focus
ring missing", "dark: label clipped on the right"). The reviewer reads verdicts, and opens an image
only for an "unexpected" one. Full-window captures of our own instance are for the end of a phase.

**The frame and the design layer (CP2.2).** `workspace_tests.rs` opens the whole `Workspace` in a
headless window at three sizes (640x480, 1100x700, 1920x1080) and presses real shortcuts
(`window.press("ctrl-b", cx)`, `window.input("tog", cx)` after `keymap::bind` and `palette::bind`),
drags the borders (`window.drag(from, to, cx)` on the `*-handle` elements) and reads sizes back from
the dump; the sizing rules (`workspace::frame_layout`), `restore_window`, `ui_state` (`native-ui.json`
round trip, malformed files), the palette matcher, the keymap table and the toast model are plain
unit tests. Each design component's states are asserted in `gallery/design.rs` and `gallery/frame.rs`
through `each_state(story, ...)`. An element a test names needs `.probe()` (a bare `div().id(..)`
is not in the dump). What cannot be tested headlessly: the Windows non-client hit test (snap
layouts flyout, caption drag, double-click to maximise), DPI changes, real window position
restore: they are on the user's checklist (`progress/phase-2.md`).

**The sidebar (CP3.1).** `sidebar/tests.rs` opens a `Sidebar` over a `FixtureBackend` (fake data that
records every `Op` and can be told to fail or to emit `CoreEvent`s) in a headless window: layout (row
and header heights, titles under the folder's name, status icons and times on one right edge, the
tile of a pinned row, nothing overflowing), then real key presses and clicks (arrows, Enter, F2, Delete
and its question, Ctrl+F, drag onto a header) and the `Op` that reaches the backend. `CoreBackend` is
tested against a real `Core` on a `hermes_testdir` folder (a tokio runtime of the test's own, calls from
the test thread). Run the app on fake data with `hermes-native.exe --fixture` (no lock, no core). Names in
the dump of a row's parts carry the thread id (`title-t1`, `status-icon-t1`), because a context menu wraps
the row and puts its own id in the path; `no_overflow` ignores the rows inside a scrolling `threads` list.
`edits_through_the_model_survive_a_restart` drives `AppModel` over a real core, then opens a second
core on the same folder and checks the store. To run the app on many threads without the user's
data, seed a synthetic folder (empty, under `target\native-test`):
`$env:HERMES_NATIVE_SEED = "$PWD\target\native-test\seed"; cargo test -p hermes-native
seed_synthetic -- --ignored --nocapture` (three projects, 50 threads with emoji / newline / long / untitled titles, pins,
archives, no-project ones), then `$env:HERMES_RS_HOME` to that folder.

**The thread view (CP4.1).** `thread/tests.rs` opens a `ThreadView` over a `FixtureBackend` (histories
from `model/fixture.rs`, any length with `fixture::long_history(n)`) in a headless window and feeds the
model `CoreEvent`s directly (`ThreadModel::on_event`, or `on_event_at` with a time): layout (bubble and
reply against the content lane, the code block's header), the virtual list (painted `row:<key>` elements
of a 2,000-row thread), older pages through real wheel events (`ScrollEvent` on `timeline`), pinning and
the `jump-to-latest` button, the parse counter (`markdown::probe`: paragraphs containing `PARSEPROBE`),
copy buttons, the dev prompt, the workspace wiring, a timing printed as `stream-20: ... stream-2000: ...`
(`cargo test -p hermes-native --lib two_thousand -- --nocapture`). Mouse selection is tested under
`#[gpui_kit::test]` (`TestAppContext`): under `headless_app()` a drag selects nothing (PITFALLS). A real
turn is kept in `crates/native/fixtures/thread-session.json`: `the_recorded_session_is_what_the_real_core_sends`
runs a real `Core` against a `wiremock` provider and fails when the core's events change;
`UPDATE_FIXTURES=1` rewrites the file. The gallery's `thread` and `thread-live` stories replay it. To try a
turn by hand: `hermes-native --fixture --dev` (the strip under the thread replays the recording), or the real
core on a synthetic folder with `--dev --dev-provider http://127.0.0.1:47900/v1` and `scripts/perf/mock_llm.py`.

**The header (CP4.3).** `thread/header_tests.rs` renders a `Workspace` over a `FixtureBackend` with a thread open
and checks the header in the title bar at 640, 1100 and 1920 px with the sidebar open and collapsed (one row
with the toggles and caption buttons, the drag zone keeps 24 px, the title and its menu keep 40 px) with a
300-character title, project, branch and model and a folder 30 levels deep (`no_overflow("thread-header")`; the
title shows an ellipsis); the chips that leave when room is short; renaming (the test helpers always say
"one click": `double_click` dispatches `click_count: 2` events itself); the toggles and keymap-built tooltips;
every menu entry's backend operation (`FixtureBackend::ops` / `thread_ops` / `exports`); the "git is read in the
background, at open and at a turn's end, never per frame" counters (`header_reads`); `git_state` on real repos made
with `git init` in a `hermes_testdir` folder (clean, dirty, detached, no repo, missing folder); and
`CoreBackend::header_info` / `export` on a real core (key masked in the file). The pure parts (git mapping, menu
state, rename rule) are unit tests in `thread/header_data.rs`. Hand-only: the save dialog, the dropdown's look, a
right click, the real tooltips.

**The composer (CP5.1).** `thread/composer/tests.rs` renders the card headless over a `FixtureBackend` (which records
every `ComposerOp` and plays the core's queue): Enter, Shift+Enter, white space, an IME composition's Enter (marked
text), undo, a failed send (text kept, toast, no row waited for), the queue from the core's list (also what another
front end queued), remove, send now, edit, Ctrl+Enter steer, Stop, `QueuedFailed` giving the text back, the three
menus, escape and outside clicks, the new-thread screen's picks and its create-then-send, drafts reported; layout at
640/1100/1920 (card = lane width, centred, footer on one row, a 300-character model name cut), the input growing to its
maximum and scrolling, the queue above the card, the menus inside the window. `wiring_tests.rs`: the app model makes the
composer and hands it the core's events, the new thread is created, opened and sent to with its picks, the workspace keeps
drafts per thread in `native-ui.json` (restored, saved, emptied = removed, stale ones of unlisted threads pruned) and lays
the card out with the sidebar open or collapsed and the right panel open. `real_tests.rs`: a real `Core` on a
`hermes_testdir` folder with a wiremock provider whose first reply is held: send, queue two, remove one, the queued
one runs and shows its row when its turn starts, Stop, a model/effort/mode picked mid-turn (the model applies after the
turn), a new thread started from the new-thread screen. Snapshots `composer`, `composer-menus`, `composer-new`. Hand-only:
IME with a real IME, Win+. emoji, paste, the feel of the menus.

**Phase 4 Done list, and what covers it.** *A recorded fixture session of every event kind replays in the
gallery:* `thread::recording` (the recorder test and the replay round trip) and the stories `thread`, `thread-live`
and `thread-tools-*`. *Timeline layout tests:* `thread/tests.rs`, `thread/tests/tools.rs`. *Snapshots of every
card kind:* `thread-tools-{command,edit,lookup,agent,group,footer}`, `thread-*`, `thread-header`. *A live turn
against the mock provider streams correctly:* the test `the_recorded_session_is_what_the_real_core_sends` (a real
`Core` + a wiremock provider) and the live check `scripts/perf/native-thread-check.ps1`: it seeds a synthetic
folder (`target
ative-live`, the sidebar's seed test), starts `mock_llm.py` on :47901 and our build with
`--dev --dev-provider <mock> --dev-send <prompt>`, with which the app opens the first thread, sends the prompt
through `ThreadModel::submit` (no pointer or keys), waits for the turn and prints `dev-send: reply ...` and, after
reading the thread back from the store, `dev-send: stored ...`; the check wants the mock's text in both and the
two equal. A window shows for a few seconds; the folder is deleted afterwards (`-Keep` keeps it). With
`--features dev`, `--dump <file>` is written when the turn has ended.

**The work log (CP4.2).** `thread/tests/tools.rs` drives the same thread view with tool events: runs of
calls fold into one group row (a reply, a reasoning row or a notice ends a run; the header counts while
a call runs), history pages join a run they cut, every card kind's layout (24 px lines aligned, a card
opens under its line and gives its height back), a 10,000-line command stays a 16-line virtual list
and is parsed once, live output touches only its own card and costs the same in a 2,000-row thread,
the footer's tree, Retry and Branch reaching `FixtureBackend::thread_ops` (and failing into a notice;
Retry on the newest turn only), `CoreBackend::retry` on a real core (a refused retry cuts nothing, a
retry cuts and resends in one submit: `the_core_backend_reads_a_stored_thread_a_page_at_a_time`),
and the actions a card sends (`OpenDiffTab`, `OpenScreen`). The pure rules have their own unit tests:
`thread/ansi.rs`, `diff.rs`, `tools.rs` (every card from its real result shape; the changed-files
tree), `work.rs` (group labels). **The live check for tool events is a core-level test, not a
script:** `thread::recording::tests::the_recorded_session_is_what_the_real_core_sends` runs a real
`Core` against a `wiremock` provider that makes a call of every tool kind (a command with coloured
output and one that exits 1, read, search, patch, write, memory, skill, a blocked web fetch, a
delegated task with a subagent, a failing read) and checks the events against
`crates/native/fixtures/thread-session.json` (`UPDATE_FIXTURES=1` rewrites it; times, ids and the
temporary folders are scrubbed so it is deterministic) and that the cards the live events build equal
the ones the stored transcript builds. The gallery's `thread-tools-*` stories replay slices of that
recording through the model.

**The gallery** (`hermes-native-gallery.exe`, `cargo run -p hermes-native --bin
hermes-native-gallery`): every story in every state, with t3code's default / GPUI Kit / Zed One and
light / dark switches, for a person to judge hover, press and focus by hand (the user's checklist).

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
