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
