# Phase 13 progress

## CP13a — `Arc<Message>` conversations
Worktree: C:\Users\alexandros\repos\hermes-rs-p13   Branch: backend/p13-perf   Model: Sonnet 5.5 (subagent)
Cut from `main` at e16fbe2 (Phases 0-12 merged). Reviewed (Opus) and committed on the branch. Phase 13 is not done (13b-13f remain).
Done:
- **The type** (`crates/llm/src/conversation.rs`): `Conversation(Vec<Arc<Message>>)`. `Clone` is now a shallow copy
  (pointers), so `OpenSession::snapshot()` (`session.rs`, was line 159, now `snapshot`), a review's view, `HistoryReplaced`
  events and `clone()` share the messages with the live conversation. API: `messages() -> &[Arc<Message>]`,
  `messages_mut() -> &mut Vec<Arc<Message>>`, `pop`/`remove` return `Arc<Message>`, `from_arcs`, `to_vec()` (deep copy for the
  page-bound functions), `arcs(iter)` (free function: plain messages to shared ones), `iter()` yields `&Arc<Message>`,
  `into_iter()` yields `Message` (moves what nobody else holds, copies a shared one). `new`/`new_unvalidated`/`push`/`extend`
  keep taking plain `Message`s. `agent_visible_messages` / `filtered_messages` return `Vec<Arc<Message>>`.
- **Copy-on-write at every mutation site** (each through `Arc::make_mut`, so a message is copied only while another holder
  has it): `Conversation::push` (a streamed chunk merging into the last message, and the "empty message carries inference
  metadata" case), `TurnRuntime::update_message` (`session.rs`), `ConversationEffect::ensure_message_ids` (`operation.rs`),
  `compress::undo`, the repair passes of `fix_conversation`, `drop_foreign_signed_thinking`, `messages_for_provider`.
  `drop_trailing_errors` / `rewind` only pop/truncate the list (no message edited).
- **The request path** (`crates/agent/src/inference.rs`): before, one request built the agent-visible projection FOUR times
  (`applies` -> `should_infer` x2 builds, `infer` -> `should_infer` x2 + `messages_for_provider`), each a deep copy of every
  message (`agent_visible_content` = `clone`), then `finalize_request` copied it again (`agent_visible_messages`, `fix_conversation`'s
  shadow map twice, one `clone` of every signed thinking block in `dedupe_signed_thinking`, `merge` through `fixed.messages().clone()`):
  about ten deep copies of the whole history per request. Now:
  - `messages_for_provider` returns `Vec<Arc<Message>>`; a message is copied only if something is taken out of it (a part for the
    user only: new `Message::has_content_hidden_from_agent` decides; a tool request nothing answered).
  - `request_base(conversation, turn)` (replaces `should_infer`) builds the projection ONCE per call (with empties kept) and derives
    the non-empty list and the "is the model next" decision from it; `infer` uses its result as the request base instead of calling
    `messages_for_provider` again. `applies` (called by the machine before `infer`) still builds it once more, as pointer clones
    plus a per-message check (no allocation of messages): two cheap passes per request instead of four deep copies.
  - `finalize_request` / the fix passes work on `Vec<Arc<Message>>` and copy only the messages they change; `dedupe_signed_thinking`
    compares signed blocks by reference in a first pass (Goose cloned every signed block into a `seen` vec) and copies only a
    message that loses a block.
  - `Provider::stream` / `complete` take `&[Arc<Message>]` (trait in `base.rs`, `OpenAiCompatibleProvider`, `CompactingProvider`,
    the two test mocks). The OpenAI formatter (`format_messages`, `create_request*`) is generic over `Borrow<Message>`, so the
    request's one real copy is the JSON the wire needs. The fallback path's `base` is a `Vec<Arc<Message>>` (clone = pointers).
- **Store**: `Store::replace_messages` is generic over `Borrow<Message>` (it serialises each message; schema untouched).
- **Callers moved to `&[Arc<Message>]`**: `operation.rs` (`messages_since_kickoff`, `last_effective_role`, `assistant_turn_count`,
  `ends_turn`), `inference.rs` helpers, `machine.rs::interrupted_response`, `runtime`: `signals` (`TurnBase::before`,
  `rewind_signal`, `final_answer`), `preinject` (`already_attached`, `last_assistant_text`), `changes::session_files`,
  `review` (`finish_turn`, `summarize`), `review_once` (`used_skills`, `topic`, `evidence_scope`, `gather`), `compress`
  (`plan`, `serialize`, `tool_calls_by_id`; `estimate_messages` is generic over `Borrow<Message>`; `compact` builds its
  output as `Vec<Arc<Message>>`, sharing every untouched message), `review_schedule::View.messages`, `FallbackGate::estimate`.
  Requests built from one fresh message (titles, `/btw`, the approval judge, compression summary, curator / review repair loops)
  wrap it in `Arc::new`.
- **Page-bound copies kept on purpose** (a user's click, not per request): `Runtime::messages`, `rewind`, `undo`, `usage`,
  `insights`, `add_command_output` return `Conversation::to_vec()` (deep copy) because the desktop crate's transcript functions
  and `branch`/`hermes_import` take `Vec<Message>` / `&[Message]`. The frozen GPUI app (`crates/app`) needed one line (`chat.rs`: `.map(shown_user_text)` over
  the messages became a closure, since the items are `&Arc<Message>`); otherwise it only uses `Conversation::new_unvalidated`,
  `messages()[i]`, `.len()`.
Files: crates/llm/{src/conversation.rs,src/conversation/message.rs,src/base.rs,src/openai_compatible.rs,src/formats/openai.rs,
  examples/chat.rs,tests/{openai_compatible_stream,request_observer}.rs}, crates/agent/{src/inference.rs,src/operation.rs,
  src/machine.rs,src/tool.rs,src/fallback.rs,src/compaction/{model,provider}.rs}, crates/store/src/lib.rs, crates/runtime/src/{session,
  lib,ops,compress,changes,signals,signals/tests,preinject,review,review_once,review_once/tests,review_schedule,delegate,fallback,
  curator/propose}.rs, docs/{ARCHITECTURE,PITFALLS}.md, PORTING.md, docs/backend/progress/phase-13.md
Remaining deep copies on the request / turn path, and why (the "no remaining deep copies" claim, by code review: grep of
`.clone()` over messages in `crates/agent/src` and `crates/runtime/src`):
- `TurnRuntime::apply_effects` -> `self.append(message.clone())` (`session.rs`): one copy of each message when the finished
  response is stored (the effect list is `&mut [HermesEffect]`); per message once, not per step over the history.
- `InferenceRunner::emit_message`: `accumulator.push(message.clone())` + `AgentEvent::Message(clone)` per streamed chunk (a delta).
- `ops.rs` repair / hide paths (`hidden(last)`, `last.clone()` to rewrite one tool call, the empty-response placeholder): one message.
- `additional_messages` (turn context, a few hundred bytes) are wrapped in a fresh `Arc` per request.
- `compress.rs`: stubbed copies of the messages a compaction rewrites (rare, by design).
- `Runtime::messages/rewind/...` return a deep `to_vec()` for the page (above).
- The provider JSON (`format_messages_with_options`) copies text into `serde_json::Value`: the wire's own copy.
- `Message::agent_visible_content` / `user_visible_content` themselves still copy (used by the shortcut's slow branch and the page).
Tests (CARGO_TARGET_DIR=target/claude): `hermes_llm` 488 lib + 5 (7 new in `conversation.rs`: a clone shares and keeps what it saw, a
  merged chunk touches only the live conversation, in-place edit of an unshared message, a valid conversation is repaired with
  zero copies, a repair copies only what it changes, merging leaves the originals, `has_content_hidden_from_agent` agrees with
  `agent_visible_content` incl. a tool result); `hermes_agent` 45 (3 new in `inference.rs`: the projection equals the pre-13a
  copying one value for value and shares everything that needed no change, `request_base`, foreign signed thinking dropped from a
  copy only); `hermes_runtime` lib 213 (2 new in `session.rs`: a snapshot shares the messages and does not see later pushes or edits;
  dropping trailing errors leaves a snapshot whole); `hermes-desktop` lib 183 + acp 68; `hermes_store`/`tools`/`memory`/`prompts` unchanged and
  green; `turns` 256 passed / 7 ignored under the default, `HERMES_TEST_PROFILE=requests` and `tokens` (the first requests run had
  `s4_requests_two_corrections_make_exactly_one_review_at_idle` fail on a timing assert under load; it passed alone and in the
  second full requests run); `cargo build --workspace` OK (placeholder `target/hermes-src/apps/desktop/dist-webapp` for the
  desktop crate, deleted afterwards). No live (`*-check.mjs`) check: nothing the page calls changed shape.
Byte-identical requests: I added a temporary hook to `turns.rs` (not kept) that wrote every request body the mock provider received
  (about 980 per profile) to a file, ran the whole suite at HEAD (e16fbe2) and on this tree under the default, `requests` and
  `tokens` profiles, and compared the multisets of lines after normalising what differs between any two runs (temp folder names,
  uuids, session / call / message ids, timestamps, durations, random hex). `tokens` and `requests`: zero differing requests (979 / 978
  vs 979 / 978). Default: 1 of 979 differs, in a random hex in an attachment file name that the normaliser missed (the line is
  otherwise equal). So no request byte changed.
Deviations from the plan:
- `Provider::stream` / `complete` signatures changed (the plan only named `snapshot()` and `messages_for_provider`): without it the
  request would have been copied back into plain messages at the boundary. The OpenAI formatter and `Store::replace_messages` are
  generic over `Borrow<Message>` so plain `Vec<Message>` callers (tests, the importer) still compile.
- `applies` and `infer` each build the (shared) projection: not "once per request" strictly, but pointer-level; caching it across the
  two trait calls would need a field on the runner for no measurable gain without a perf run.
- No allocation-count micro-benchmark (the user's disk-deletion / no-perf-checks rule); the claim rests on the review above and
  the `Arc::ptr_eq` tests.
For the reviewer:
- **Every mutation goes through `make_mut`** (focus): grep `messages_mut()` and `Arc::make_mut` in `crates/`. The only
  `messages_mut()` users now pop / truncate / retain / iterate with `make_mut` (`session.rs` `drop_trailing_errors`,
  `update_message`; `lib.rs` `rewind_as`; `compress.rs` `undo`; `operation.rs` `ensure_message_ids`). Anything that assigns
  through an `Arc` will not compile (no `DerefMut`), which is the safety net.
- **Repair passes rewritten** (`conversation.rs`, Goose code): each pass now checks "needs work" read-only and `make_mut`s only then.
  Check the pairing of the check with the old unconditional code, and that issue strings and their order are unchanged:
  `has_mergeable_text` (adjacent equal-audience text pair is exactly when the old fold merged; chains merge into the first block's
  audience, so the pairwise test is equivalent), the trim test, the empty-tool-result test, `fix_tool_calling` (removal only when
  something is to be removed), `merge_consecutive` (`make_mut(last)` + `try_unwrap`/clone of the merged message's content),
  `dedupe_signed_thinking` (two passes; one issue per message that loses a block, in message order, as before).
- **`has_content_hidden_from_agent` must stay in step with `filter_for_audience`** (`message.rs`): Text / Image by their own audience
  annotation, tool results by each content block's audience (`content_audience`), everything else never filtered. A unit test checks
  agreement on a few shapes; a new block type that gets filtered must be added to it.
- `request_base` replaces `should_infer`: the "non-empty last" decision is taken from the keep-empty projection exactly as the old
  two calls did (`last` of the keep-empty list empty -> no request; then the last non-empty message's effective role).
- Prompt caching: request bytes unchanged by the comparison above; no past message or the system prompt is rewritten (a shared
  message is only ever replaced by a modified COPY in the request list, never in the conversation).
- `Conversation: Serialize/Deserialize` derive uses serde's `rc` feature (already enabled workspace-wide); nothing serialises a
  `Conversation` today.
- `fmt`: `cargo fmt` is NOT clean at HEAD for several crates (e.g. `api_client.rs`, `retry.rs`, `budget.rs`, the `inference.rs` loop
  body); I formatted my changes and then removed the formatting-only noise in files I had not meant to touch (3-way merge against a
  formatted HEAD), so the diff is only the change. The reformatting of the unformatted `inference.rs` loop was left out.
- Not touched: `crates/desktop` (it still sees `Vec<Message>` from the runtime), the GPUI app apart from that line, the store schema, protocol shapes.

### Review (Opus, 2026-11-15)
Checked the whole diff against the plan's review focus (every mutation through `make_mut`; no deep copies left on the
hot path) and the implementer's list. No correctness defect found; one test made robust, one equivalence test added.
- **Repair passes = Goose's originals:** holds. Read pass by pass against HEAD: `has_mergeable_text` is true exactly
  when the old fold would join two blocks (it only ever joins an adjacent equal-audience pair and keeps the first
  block's audience, so chains reduce to the pairwise test); the trim test, the empty-tool-result test, the
  `fix_tool_calling` removals and `drop_foreign_signed_thinking` guard the old unconditional loops with the same
  predicate; `dedupe_signed_thinking` still compares blocks by value (`Vec<&Block>::contains` derefs), one issue per
  message in message order; `merge_consecutive` is the same extend. **Added** `conversation/pre13a_repair.rs` (test
  only: HEAD's `fix_conversation` and `merge_consecutive` over plain messages, copied verbatim) and
  `copy_on_write_repair_matches_the_copying_passes`: 4,000 generated conversations (>10,000 messages) mixing
  audiences (none / empty / user / assistant / both) on text, images and tool-result parts, empty and whitespace
  text, tool calls answered, orphaned and misplaced, empty / error / image results, signed, unsigned and redacted
  thinking with duplicates, agent-only / user-only / turn-context messages. It asserts the same messages and the same
  issues in the same order as the old code, the input conversation unchanged, `has_content_hidden_from_agent ==
  (agent_visible_content() != message)` for every message, `agent_visible_messages` and both merge modes equal to the
  old ones, and that every issue kind (15) was exercised. Passes.
- **Copy-on-write:** holds. `Arc<Message>` has no `DerefMut`, so every in-place change must be `make_mut` (compile
  check); grep of `messages_mut()` / `make_mut` matches the implementer's list. `ensure_message_ids` now assigns ids
  on the replacement conversation through `make_mut` (same result as before). `Message` has no interior mutability,
  so sharing is observably the same as the old deep copies. `TurnRuntime::append` now keeps an `Arc` clone for the
  store write (it was a deep clone per streamed chunk) and drops it before the next chunk.
- **Remaining copies:** the implementer's list is complete for the turn path; two more, both off it: the Goose
  compaction summarizer (`compaction/{model,provider}.rs`) copies the messages into plain ones, only on a context
  overflow. Mind `conversation.to_vec()` (deep) vs `conversation.messages().to_vec()` (pointers): added to PITFALLS.
- **Byte-identical requests:** not re-run against HEAD (it would need a second full build of HEAD plus the request
  hook); covered instead by the equivalence test above (repair, projection, merge), the implementer's
  `the_request_projection_matches_the_copying_one...` test for `messages_for_provider`, `request_base` deriving the old
  two `should_infer` projections from one, and the formatter changing only `&[Message]` to `&[M: Borrow<Message>]`.
- Signatures consistent; `sessions.db` schema untouched (`replace_messages` only made generic); no protocol shape
  changed, so no live check. GPUI app builds.
- **Flake:** `s4_requests_two_corrections_make_exactly_one_review_at_idle` passed 5/5 alone. It is timing-sensitive by
  design: the 1.5 s idle timer is armed at each turn's end and must not fire before the next turn starts, a gap a
  loaded machine can stretch. Idle raised to 4 s (the final quiet wait to 5.5 s); same assertions, ~6 s longer. Noted
  in PITFALLS.
- Tests after the review (CARGO_TARGET_DIR=target/claude): `hermes_llm` 489 lib + 5, `hermes_agent` 45, `hermes_store`,
  `hermes_tools` (329), `hermes_memory`, `hermes_prompts` green; `hermes_runtime` lib 213; `hermes-desktop` lib 183 +
  acp 68; `turns` 256 passed / 7 ignored under the default, `HERMES_TEST_PROFILE=requests` and `tokens`;
  `cargo build --workspace` OK (placeholder `target/hermes-src` in the worktree, deleted afterwards). No perf checks
  (user's rule).

**User checklist** (no visible change; one sanity pass in the rebuilt app).
1. Rebuild and start the app (`cargo build -p hermes-desktop`, your usual run).
2. Open an existing long session: it shows the same history as before.
3. Send a message that makes the agent run a tool or two (e.g. "list the files here"): the reply streams, the tool card shows, and
   after the answer the session list and the history still match what you saw.
4. Click edit on an earlier message and resend: the conversation rewinds to that point and continues (rewind uses the shared list).
5. Run `/compress` on a long session: the summary notice appears and the next message works.
