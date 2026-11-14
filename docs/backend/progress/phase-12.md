# Phase 12 progress

## CP12.1 — The provider fallback chain
Worktree: C:\Users\alexandros\repos\hermes-rs-p12   Branch: backend/p12-fallback   Model: Sonnet 5.5 (subagent)
Cut from `main` at eeb2940 (Phases 0-11 merged). Reviewed (Opus) and committed on the branch; Phase 12 done.
Done:
- **Config** (`crates/desktop/src/fallback.rs`): the chain is the config record's `fallback_providers`
  (`desktop-config.json`), `[{provider, model}]` in the order tried: the key Hermes's own Settings ▸ Model ▸
  Fallbacks sub-page edits, so there is a natural slot and no new page code (`rest.rs`: default `[]` and a
  schema description; `hide.css`: the sub-page is no longer hidden). Global, not per profile or per session:
  the existing layering has neither (a session stores its provider and model; there are no profiles in the
  config record). Read at every turn in `Gateway::turn_options`, so a change applies from the next message.
  Validation drops unusable entries with a `tracing::warn!` each, never a failed turn: not an object (a
  `"provider/model"` string is accepted), a missing provider or model, a provider that is not configured or
  has no key, a model absent from a fixed model list (`discover_models` off), the primary itself, repeats, and
  everything past `MAX_FALLBACKS` = 3. Keys never come from the chain: the entry names a configured provider,
  whose endpoint and key are used; the page's extra routing keys (`base_url`, `api_key`...) are ignored.
- **Triggers** (`crates/llm/src/fallback.rs`, `classify`, one place): daily-quota 429 (the "N of N" body,
  "usage limit reached", "free-models-per-day" / "per day", "daily limit", or a Retry-After of an hour or
  more), our governor at 0 for the provider (no request sent), 5xx and no answer (timeout, refused
  connection) after the provider layer's retries, model not found (404 or 400 with the provider's own words:
  `model_not_found`, "not a valid model", "does not exist", "no endpoints found", "model ... not found"; the
  URL the provider layer prefixes to a 404 is ignored, so a host called `models.*` is not a missing model).
  Never: 401 / 403, 400, 422, 413 and "maximum context" (compression), 402, per-minute 429 (Retry-After of
  seconds), stream decode errors, a plain 404. Unit tests use real OpenRouter / OpenAI / Ollama bodies and
  Retry-After values (OpenRouter's daily 800-of-800 body with `retry_after_seconds: 19919`, the free-tier
  `free-models-per-day` body, Groq's tokens-per-day, per-minute limits with 2 / 8 / 30 s).
  **Retry rule changes** (`retry.rs::should_retry`): a daily-quota 429 (the wider wordings, a Retry-After of an
  hour or more) and a missing model are no longer retried (a 404 model-not-found cost `http_retries` extra
  requests; one existing test used "model not found" as its example of a retried 400, now "upstream hiccup").
- **The switch, in `InferenceRunner::open_stream`** (`crates/agent/src/{inference,fallback}.rs`): the same
  request (same messages and tools, signed reasoning of another provider dropped as for any provider change)
  is re-issued to the next target. Decisions:
  - *Stickiness:* the runner is built per turn, so the position (`ChainRun.current`) holds for the rest of the
    TURN (no flapping mid-turn, one notice) and the next turn starts at the primary. The primary is skipped
    without a request only when it is known out of requests until its reset: its configured daily limit is used
    up (governor), or it answered a daily-quota 429 (`Ledger::mark_exhausted`, in memory, honoured by
    `Governor::check`, so by `begin_turn` too). An outage or a missing model is not remembered: one failed
    request (plus its retries) per turn while the primary is down.
  - *Mid-stream rule:* a request moves only before anything of its response was delivered: an HTTP error
    before the stream, or an error as the stream's FIRST item (the runner peeks it). After the first chunk the
    behaviour is exactly what it was (what arrived stays, the error follows): never re-asked on another
    provider, nothing duplicated, no half message invented. I did not change the existing "partial then error"
    behaviour (discarding the partial would be a separate behaviour change; see the doubts).
  - *Event shape:* `AgentEvent::Status { kind: "fallback", text }` -> `status.update`, the one kind the page
    keeps as a line (the taint warnings already use it; Hermes uses it for "switched to fallback model"). The
    text names who could not serve and why, `switched to B (model) for the rest of this turn`, that the
    conversation is now sent to B, that B's prompt cache starts empty, and that the next message tries A
    again. ACP shows it as a thought line.
  - *Every provider keeps its limits:* the gate asks `Governor::check(provider, Main)` for each position
    before a request goes there; a refused fallback is skipped, the walk continues, and a chain that runs out
    ends with the FIRST provider's error plus `Fallback chain: a: ...; b: ...`. `begin_turn` refuses at 0 only
    when no provider of the chain may take the turn. The ledger counts a request on the provider that served
    it (every client is labelled with its provider id): a moved turn shows one request on each.
  - *Mid-turn tool results are not re-run:* only the model call moves (a test makes a tool call on the
    fallback and checks the file and the request count).
  - *Who uses the chain:* only the main turn. Review, curator, titles, the approval judge and `delegate_task`
    children keep their own providers (behaviour unchanged); compression summaries use the fallback only while
    the turn runs on it. ACP shares the gateway, so it gets the chain.
- **Context length, before switching:** each target has a `fit_limit` (the compression threshold of its
  window; an unknown window is not checked: documented default). The runner estimates the request
  (`FallbackGate::estimate`, chars / 4) and, if it does not fit, asks for compression once per target and turn
  (`Pending` + an empty `applied`; the machine loops; `AutoCompress` reads `FallbackShared` and compresses for
  that window, writing the summary on the fallback's provider when the session's own provider is the one out of
  requests, and judging size by the estimate of the whole request instead of the primary's last reported
  usage); then the runner re-checks. Still too big: the target is skipped, never sent an over-limit request.
Files: crates/llm/{src/fallback.rs,src/lib.rs,src/retry.rs}, crates/agent/{src/fallback.rs,src/inference.rs,
  src/lib.rs}, crates/runtime/{src/fallback.rs,src/lib.rs,src/compress.rs,src/delegate.rs,src/governor.rs,
  src/ledger.rs,tests/turns.rs,examples/agent.rs}, crates/desktop/{src/fallback.rs,src/lib.rs,src/gateway.rs,
  src/rest.rs,plugins/hermes-rs/hide.css}, crates/app/src/{chat,state}.rs (a field in two struct literals; the
  frozen app still builds), scripts/perf/{fallback-check.mjs,README.md}, GAPS.md, PORTING.md, docs/{ARCHITECTURE,
  PITFALLS,TESTING,GAPS-PLAYBOOK}.md
Tests (CARGO_TARGET_DIR=target/claude, all green): `hermes_llm` lib 480 (+16 fallback / retry tests: real provider bodies,
  Retry-After, never-on-4xx), `hermes_tools` / `memory` / `store` / `prompts` unchanged; `hermes_runtime` lib 211 (4 new: gate, cap, fit limit,
  quota mark), `turns` 255 passed / 7 ignored, run under the default, `balanced`, `requests` and `tokens` profiles (the first three before the last
  wording change of the notice, then the default again: 255 each time; 17 new Phase 12 scenarios); `hermes-desktop` lib 183 (7 new: chain
  validation) + acp 68; `cargo build --workspace` OK (a placeholder `target/hermes-src` for the desktop crate, deleted afterwards). Live:
  `scripts/perf/fallback-check.mjs` against a perf instance built from this worktree (port 47810, data in `target/perf`): all checks passed;
  the instance was stopped by its exe path.
Request counts: unchanged when `fallback` is empty or the primary works (`nothing_changes_when_the_primary_works`
  compares request count, the user message and the tools with and without a chain; the whole suite passes
  unchanged under each profile). With a failure: daily 429 -> a 1, b 1; 503 with `http_retries` 1 -> a 2, b 1;
  400 / 401 / 403 / 422 / per-minute 429 / plain 404 -> a 1, b 0; missing model -> a 1, b 1; our limit at 0 -> a 0,
  b 1.
Deviations from the plan:
- The config key is Hermes's `fallback_providers` in `desktop-config.json`, not a new `fallback` key: it is the
  one the page's own sub-page edits.
- No per-profile or per-session override (the config layering has neither).
- A 402 (credits exhausted) does not switch: it is not in the plan's trigger list and is the user's to top up;
  easy to add to `classify` if wanted.
- The mid-stream rule is "never switch, keep today's partial + error" rather than "discard the partial".
- `should_retry` changed for quota 429s and missing models (above); an existing test's example text changed.
For the reviewer:
- **No fallback on ordinary 4xx** (the review focus): `classify` is the only decider (`crates/llm/src/
  fallback.rs`); `ordinary_client_errors_never_move_to_the_fallback` asserts b gets ZERO requests for 400, 401,
  403, 422, a per-minute 429 and a plain 404, and the llm tests cover real bodies. Check `is_model_not_found`:
  it is the loosest rule ("model" + "not found" / "does not exist" in the provider's own words); a 400 whose
  message contains both would switch.
- **No half-streamed duplicate** (focus): `open_stream` peeks the first item; after it, the existing loop is
  untouched. Test: `a_response_that_breaks_half_way_is_never_repeated_on_the_fallback` (b zero requests, the
  text once in the events and once in the store). Look for any path where `delivered` content could precede a
  switch: `settle`/`pick` run only before `Opened::Stream` is returned.
- **Context check before switching** (focus): `pick` checks the gate and the fit BEFORE returning `Use`;
  `a_fallback_with_a_smaller_window_is_compressed_for_before_it_is_asked` (the summary request goes to b with
  purpose `compress`, the real request is under the window) and `a_fallback_the_request_cannot_fit_is_skipped_...`.
  The estimate is chars / 4 over the request; the limit is the compression threshold, not the full window.
  The system prompt and the 18 tool schemas alone are about 12k tokens, so a window under ~16k can never be
  fit (always skipped), as it could not be used by the agent anyway.
- The loop between the runner and `AutoCompress` is bounded by `compress_asked` (once per target and turn); an
  `AutoCompress` that decides not to compress (failed earlier, `compacted_at`) just means the target is skipped.
- `Ledger::mark_exhausted` is in memory only and lasts until the provider's next reset. After a daily 429 with a
  chain, removing the chain makes the next turn refused ("used up (the provider said so)") until the reset or a
  restart. A user who tops up credits mid-day must restart the app.
- Page: the Fallbacks sub-page is unhidden but I could not open it (no `target/hermes-src` webapp in the
  worktree). It writes `fallback_providers` through `/api/config` like the other fields; the live check writes
  the key through `config.set`. If the page's editor misbehaves, re-add the `nav-config:model:fallbacks` line to
  `hide.css` (the key still works from the file).
- The notice is a `status.update` `fallback` line (transient: it is not stored with the session).
- Not done / decided against: chain for review/curator/aux calls; reasoning-effort params on a fallback; a
  vision check (the entry's `supports_vision` comes from the model facts when known); a "health" memory for
  outages (a 5 minute cool-down would stop one failed request per turn: simple and deterministic was asked).
- Frozen GPUI app: two struct literals got `fallback: Vec::new()`; `cargo build --workspace` passes.

## Review (Opus, 2026-11-14)
Checked against the plan's three review points and the rest of the diff.
- **No fallback on ordinary 4xx:** holds. `classify` is the only decider; 400 / 401 / 403 / 402 / 413 / 422 /
  per-minute 429 / plain 404 map to `None` (unit tests on real bodies, `ordinary_client_errors_never_move_to_the_fallback`
  with zero requests to b, the live check for 401 and 400). **Fixed:** `is_model_not_found` matched any message
  containing "model" and "not found" anywhere, so an ordinary 400 such as "tool call id ... not found ... the model"
  would have switched provider (and, through `should_retry`, stopped being retried). Now "model" must be followed by
  "not found" / "does not exist" within 120 characters in the same sentence (no ". " or newline between); every real
  wording in the tests still matches; new test `a_400_that_mentions_a_model_and_something_not_found_does_not_switch`.
  `should_retry`: ordinary 429 / 5xx / network / other 4xx retry exactly as before (the retry tests are unchanged
  apart from the example text); only quota 429s (wider markers, a Retry-After of an hour, which is the clamp) and a
  missing model stop retrying. The new markers ("per day", "daily limit") also hide the error card's Retry button
  for such 429s (`transcript.rs` shares `is_usage_limit`), which is right for a daily quota.
- **No half-streamed duplicate:** holds; the switch is decided only in `open_stream` before `Opened::Stream`, and the
  stream loop after it is unchanged. The existing test used a decode error, which `classify` never switches on, so
  it did not prove the rule. **Strengthened:** it now also breaks half way with an in-stream `{"error":...}` frame
  (a ServerError that WOULD switch had it come first): still b = 0 requests, text once. New test
  `an_in_stream_server_error_before_any_chunk_moves_to_the_fallback` covers the peek itself (200 + error as the first
  frame -> b answers, no error stored). **Fixed:** an empty stream (first item `None`) was re-chained and polled again
  after it ended; not every stream is fused, so it now returns `stream::empty()`.
- **Context check before switching:** holds (`pick` checks gate and fit before `Use`; the `Compress` loop is bounded by
  `compress_asked`; compression is the only rewrite; the system prompt is untouched). **Fixed:** with an auxiliary
  compression model configured, `Summarizer::bound_to` left the session-model backup behind the auxiliary on the
  primary (the provider that just failed); now that backup is rebound to the target. Docs adjusted (ARCHITECTURE,
  the compress.rs field doc).
- Ledger on the serving provider, each target's own governor, no keys from the chain, validation (unknown provider,
  primary, repeats, cap): correct and tested. `turn_options` does its config/settings reads synchronously as it did
  before this phase (no new blocking pattern); no `sessions.db` change.
- **Fallbacks page unhide: kept.** Read from Hermes's source at `HERMES_COMMIT`: the sub-page renders
  `FallbackModelsField`, which calls only `GET /api/model/options` (served; `slug` = our provider id) and the
  ordinary config autosave. Live: `GET /api/config` returns `fallback_providers: []`, `/api/config/schema` lists it,
  `PUT /api/config` round-trips an entry with an extra `base_url` key. Not opened in a rebuilt page (no webapp build);
  PITFALLS says how to re-hide it if the editor misbehaves.
- The notice reuses `Status{kind:"fallback"}`; protocol shapes unchanged. The four new `fallback.rs` files formatted
  with rustfmt.
- Tests after the fixes (CARGO_TARGET_DIR=target/claude): `turns` 256 passed / 7 ignored under the default (=
  balanced), `HERMES_TEST_PROFILE=balanced`, `requests` and `tokens`; `hermes_runtime` lib 211; `hermes_llm` lib 481;
  `hermes_agent` 42; `hermes-desktop` lib 183 + acp 68; `cargo build --workspace` OK (placeholder `target/hermes-src`,
  deleted after). Live: `fallback-check.mjs` against this worktree's debug build on 47810 (data `target/perf`): all
  checks passed; instance stopped with `perf.ps1 -Do stop`.
- Left as documented decisions: 402 does not switch; the partial-then-error behaviour is kept (not discarded); outages
  are not remembered across turns; the exhausted mark is in memory (a restart clears it); a first frame that carries
  nothing visible (a usage-only chunk) followed by an error counts as delivered and does not switch (conservative).

**User checklist** (Phase 12). Needs a build of the app (`target\debug\hermes-desktop.exe`) and a second provider.
1. Open Settings ▸ Providers and make sure you have two providers set up, say your usual one and a second one
   (a local Ollama / LM Studio server needs no key; a hosted one needs its key). Note the second provider's name.
2. Open Settings ▸ Model ▸ Fallbacks. Click "Add fallback", pick the second provider and one of its models. (If
   the page does not show Fallbacks, add this to `%APPDATA%\hermes-rs\desktop-config.json`, with your own ids,
   and restart the app: `"fallback_providers": [{"provider": "my-second-provider", "model": "its-model"}]`.
   Provider ids are the ones in `%APPDATA%\hermes-rs\config.json` under `providers`.)
3. Simulate running out: Settings ▸ Providers, open your main provider and set "Daily request limit" to 3 (or, in
   `desktop-config.json`, `"requests": {"limits": {"<main provider id>": {"daily_limit": 3}}}`). Send a few short
   messages until the main provider's 3 requests are used (the status bar shows "Requests 3/3").
4. Send one more message. Expected: the answer comes from the second provider, and a grey line appears in the
   chat: "<main> has used up its daily request limit: switched to <second> (<model>) for the rest of this turn.
   Your conversation is now sent to <second>, and its prompt cache starts empty there...".
5. About the cache: the second provider has not seen your conversation, so this first reply reads all of it
   again (more input tokens, maybe a little slower); the following requests of the same turn are cached there.
   The next message tries your main provider first again (here it is still used up, so it goes straight to the
   second one without spending a request).
6. Check where your conversation went: the line names the provider each time it moves. A wrong API key on your
   main provider never moves it: you see the key error instead (that is on purpose).
7. Remove the fallback: Settings ▸ Model ▸ Fallbacks, click the X on the row (or set `"fallback_providers": []`),
   and set the daily limit on the main provider back to empty. Restart the app if the main provider still says
   "used up" (it remembers a quota answer until its reset time or a restart). With no fallback a used-up
   provider is refused as before ("used up ... resets at ...").
