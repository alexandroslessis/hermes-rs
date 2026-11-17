# Phase 13 progress

## Phase 13 summary (done 2026-11-17, branch `backend/p13-perf`)
Six plumbing checkpoints, each reviewed and committed on its own, with no behaviour change by design (protocol shapes,
event order, RPC codes, ACP and the `sessions.db` schema unchanged; `turns` 258 passed / 7 ignored under default,
`requests` and `tokens` after every one):
- **CP13a** (20f3724): conversations are `Vec<Arc<Message>>`; snapshots share messages, edits go through `make_mut`.
- **CP13b** (74ac1f7): one writer thread owns `sessions.db`'s write connection; turn writes are queued, reads wait for
  the writes sent before them, a finished turn is flushed before `TurnFinished`.
- **CP13c** (8e3d12d): no store, file, git or process work left on an async worker (`Gateway::blocking`,
  `rest_blocking`, `off_worker_unless_stopped`).
- **CP13d** (acad3ca): one actor thread per session owns its control state (run slot, review slot, queue, live turns,
  pending route, effort, revisions, held images).
- **CP13e** (a110767): Goose's `StateMachine` replaced by `AgentLoop` in `crates/runtime/src/agent_loop.rs`.
- **Test-temp leak fix** (fce5fec): `hermes_testdir` for every test folder (the runs had filled 300 GB of `%TEMP%`).
- **CP13f** (the `backend p13 cp13f` commit): `gateway.rs` split into `gateway/` by area, dispatch over a typed
  `Request` enum with lenient params and a typed `RpcError`; answers proven unchanged by golden records.

**User checklist** (one sanity pass covering all six; nothing should look different):
1. Rebuild and start your app (`cargo build -p hermes-desktop`, your usual run). The sidebar lists your sessions and
   the project tree; open an existing long session: the same history as before.
2. Send "list the files here": the reply streams, the tool card shows, the title and the status bar's tokens appear.
3. Ask for something slow (a slow command) and, while it runs, send two more messages: they appear as queue cards;
   delete one, edit the other (the edit goes last); use Steer with a short note: the note runs first when the turn ends.
4. Start another long request and press Stop: it stops at once and the tool card keeps the output it had. Stop with
   nothing running does nothing.
5. While a reply streams, click another session and back: the history is the same as on screen. When it ends, close
   the app at once and start it again: the whole exchange is there.
6. Edit an earlier message and resend (rewind): it continues from there. Run `/compress` on a long session, then send a
   message: the summary notice shows and the reply works. `/usage`, `/status`, `/context`, `/save json` and `/rollback`
   answer as before.
7. Pick another model while a reply streams: it is used from the next message. Change the effort, switch sessions and
   back: the pick is kept. Settings: switch auto-archive and the budget profile: both are kept after a restart.
8. Move a session to another folder with the composer's folder picker: it works; while a turn runs it says the session
   is busy. Rename, archive and unarchive a session, delete another: each applies at once.
9. Ask the agent to remember something, to save a skill and to delegate a small task: memory and skill are saved, the
   subagent row runs and its summary comes back.
10. Negative: voice, pet and wake features stay hidden; nothing new appears.

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

## CP13b — store writer actor
Worktree: C:\Users\alexandros\repos\hermes-rs-p13   Branch: backend/p13-perf   Model: Opus 5.5 (subagent)
On top of 20f3724 (CP13a). Not committed (the reviewer commits). Phase 13 is not done (13c-13f remain).
Done:
- **The writer** (`crates/store/src/writer.rs`, new; tests in `writer/tests.rs`): one thread ("hermes-store-writer") owns
  the write `Connection` of `sessions.db`; every write is a job on one `std::sync::mpsc` channel, run in send order.
  Two kinds of job: **queued** (`Store::queue(StoreOp)`, new: `UpsertMessage`, `ReplaceMessages`, `RecordUsage`,
  `SetSystemPrompt`; returns at once) and **waiting** (every existing write method: it sends its write as a job and
  blocks until that job's batch is committed, returning the write's own result). So the synchronous API the gateway, the
  importer and the frozen GPUI app use is unchanged in signature and meaning, and it can never overtake a queued write.
- **Reads** (`Store::read()`, crate-private): a second connection, `query_only`, busy timeout 5 s. Every read method
  (sessions, messages, counts, usage, projects, search, insights: all of `lib.rs`, `projects.rs`, `search.rs`) first
  calls `Writer::settle`: if anything is queued it sends a `Flush` marker (the writer closes its batch at once) and waits
  until every job sent before the read is done. An in-memory store uses one connection for both (two `:memory:`
  connections would be two databases).
- **The turn path** (`crates/runtime/src/session.rs`): `TurnRuntime::append`, `update_message`, `drop_trailing_errors`,
  the `ReplaceConversation` effect, `OpenSession::apply_compaction` (prompt + messages) and `RecordUsage` now `queue`.
  `append` / `update_message` / `drop_trailing_errors` queue under the conversation's lock (queueing never blocks), so two
  writers to one session reach the store in the order they changed the conversation (before, the synchronous upsert ran
  after the lock was released, so two racing appends could land out of order).
- **Turn boundaries** (`crates/runtime/src/lib.rs`): `flush_turn` (`Store::flush_session` through `spawn_blocking`) +
  `after_save` before every `TurnFinished`: the normal end (right after the state machine and the event forwarder finish,
  before `running` is cleared), the early-out path (the user message could not be added / cancelled), `/compress`, and
  the panic guard of a crashed turn.
- **Exit**: `Store::shutdown()` (new; also `Drop`): closes the channel, the thread commits what is queued and ends, joined.
  `crates/desktop/src/main.rs` calls it on `RunEvent::Exit` (next to the ledger flush), `acp/mod.rs` when the editor
  closes stdin. Writes after shutdown run inline on the caller's thread (the old behaviour), so a late background write is
  not lost either.

Decisions (the policy):
- **Batching**: a batch starts with the first job and collects for `BATCH_WINDOW` = 5 ms, at most `MAX_BATCH` = 256 jobs;
  it closes early at a waiting write (committed at once), a `Flush` marker (a read or a flush is waiting) or the channel
  closing. Inside: `BEGIN IMMEDIATE` (the write lock up front, so no lock-upgrade deadlock with the GPUI app's writer),
  each job in `SAVEPOINT store_op` ... `RELEASE`, then `COMMIT`. A queued upsert of `(session, seq)` that a later upsert of
  the same row in the same batch replaces, with nothing between them that touches that session except upserts of other
  rows, is skipped (a streamed reply: only its last version per batch is serialised and written). Waiting upserts are
  never skipped.
- **Rollback semantics**: a job that fails is rolled back to its savepoint alone; the rest of the batch commits. A `BEGIN`
  that fails (busy past 5 s) fails every job of the batch with that error. A `COMMIT` that fails rolls the batch back and
  runs each job again in a transaction of its own (jobs are `Fn`), so one job the database refuses at commit time (a
  deferred constraint) never loses the others. A panic inside a job is caught (`catch_unwind`), the open transaction
  rolled back, every job of that batch reported failed, and the writer goes on; a transaction left open is rolled back
  before the next batch (`is_autocommit` check). Later queued writes of a session whose write failed still run (each
  independently); the turn is stopped by the reported error at its next write or at its end.
- **Errors are reported, not swallowed**: a waiting write gets its own `Result` (as before). A queued write that fails is
  logged at `error` (`session database: a queued write for <session> failed: ...`) and kept per session (at most 64; the
  rest counted) until someone takes it: the session's next `Store::queue` returns it as `Err` (an EARLIER write; this one
  is queued all the same), which stops the turn at its next effect as a synchronous error did; otherwise the turn-end
  `flush_session` returns it and `after_save` makes the status `TurnStatus::Failed("This turn was not fully saved to the
  session database: N write(s) to the session database failed (the first: <SQLite's reason>). What is shown may be
  missing after a restart.")`, appended to an existing failure reason. The page shows it as any failed turn (no new
  shape). Each failure is reported once, to one `queue` or flush; `flush_session(id)` takes only that session's,
  `flush()` all. A deleted session's leftover failures are dropped. A review's or a subagent's queued token usage is filed
  under the parent session (their `record.id`), so its failure reaches the parent's next turn or flush.
- **Nothing lost on exit or crash**: graceful exit drains (`shutdown`); a panic elsewhere in the app does not touch the
  writer (its own thread; its own panics are caught). A killed process (or `process::exit`, no destructors) loses only
  what was queued and not yet committed: at most the open batch, ~5 ms of writes. A turn boundary is never lost: the
  turn's writes are committed before `TurnFinished` goes out, so a turn the page shows as finished is on disk (WAL +
  `synchronous=NORMAL` as before: durable against a process kill; a power cut can lose the last commits, unchanged).
  Tested with a real killed process (below).
- **Never block the async workers**: `queue` is a channel send under two short mutexes. `flush*` blocks for at most one
  commit and is called from async code only through `spawn_blocking` (`flush_turn`, the ACP exit). Read methods may wait
  for one commit when writes are pending; they were already blocking SQLite calls on the async workers before this
  checkpoint (CP13c's subject), and the wait is no longer than the old lock contention with a write on the single
  connection.
- **Order**: one channel, one thread, jobs in send order; `seq` numbers are assigned under the queue's lock, so
  `done >= seq` means everything sent before is committed. Message rows are keyed by position as before; the ids and order
  the page sees are unchanged (`a_finished_turn_is_committed_before_the_page_hears_it_ended` compares them).
- Schema, protocol shapes, message ids: unchanged. The one connection-level difference: the read connection is
  `query_only` (a per-connection pragma).

Read-after-write: every `Store` read method waits for the writes sent before it (from any thread), so each point below is
covered by the store itself; the column says why the read needs it.

| Reader (where) | Store reads | Needs what a turn / write just did? | Covered by |
|---|---|---|---|
| `Runtime::reload_if_stale` (runtime `lib.rs`; before a turn, rewind, compress, `messages`) | `message_count`, `last_message_id`, `load_messages` | yes: compares the live conversation with the store; a stale read would "reload" an older history over it | read barrier |
| `Runtime::open` / `open_fresh`, session switch, `session.resume` / `session.history` (gateway 992) | `load_messages`, `session` | yes | read barrier |
| rewind / undo / retry / rollback (`rewind_as`, gateway `rewind_turn`) | `reload_if_stale`, then `replace_messages` (waiting write) | yes | barrier; the write is ordered after the turn's queue |
| delete (`Runtime::delete_session`, gateway `session.delete`) | `delete_session` (waiting write) | a cancelled turn's last chunks | ordered after them in the queue |
| `/usage`, `session.usage`, status bar (`usage_text`, gateway) | `usage_totals` | yes: the turn's last call | read barrier |
| `/insights`, `insights.get` (`commands::insights_report`) | `insights` | yes | read barrier |
| session list / sidebar / search (gateway `session.list`, `/api/sessions`, `/api/sessions/search`) | `list_sessions`, `message_counts`, `sessions_matching`, `session` | yes: titles, `updated_at`, counts right after a turn | read barrier |
| export (`session.export`, `export.rs`), `/branch`, `hermes_import` | `Runtime::messages` (live conversation after `reload_if_stale`), `session` | yes | read barrier |
| session info / title / model / workspace (gateway `session` reads, `workspace.rs`, `acp/server.rs`) | `session`, `message_count` | yes: `set_title` / `set_model` are waiting writes; a turn's `updated_at` | waiting write + barrier |
| projects (gateway, runtime `list_projects`) | `list_projects` | after `create/update/delete_project` (waiting writes) | waiting write + barrier |
| `session_search` tool, recall (`tools/src/recall.rs`) | `search`, `search_rows`, `search_row_session`, `session_by_title`, `list_sessions`, `session` | other sessions (the asking one is excluded); a `session_id` read of any | read barrier |
| review, review schedule (`review_once.rs`, `review_schedule.rs`) | `session`, through `Runtime` | the turn that just ended | turn-end flush + barrier |
| the GPUI app (another process), a second `Store` on the file, tests with `rusqlite` | anything | committed rows only | the turn-end flush; `flush()` elsewhere |
| exit (`main.rs` `RunEvent::Exit`, ACP stdin closed) | — | — | `shutdown` drains |

Files: crates/store/{Cargo.toml,src/lib.rs,src/projects.rs,src/search.rs,src/writer.rs (new),src/writer/tests.rs (new)},
  crates/runtime/{src/session.rs,src/lib.rs,tests/turns.rs}, crates/desktop/src/{main.rs,acp/mod.rs}, Cargo.lock (`tracing` for
  the store), docs/{ARCHITECTURE,PITFALLS,TESTING}.md, PORTING.md, docs/backend/progress/phase-13.md
Tests (CARGO_TARGET_DIR=target/claude, final tree):
- `hermes_store` 31 (12 old + 19 new in `writer/tests.rs`: send order with a replace in the middle; a waiting write
  never overtakes a queued one (incl. a delete after queued upserts); every read sees what was queued before it, on the
  other connection and from another thread, no flush; a second `Store` on the file sees the queue after `flush_session`;
  200 queued writes in few batches (< 50, typically 1-2); the 256 cap (writer held off with the connection's lock: >= 3
  batches); 30 chunks of one message written once (>= 28 skipped); `superseded` keeps an upsert across a replace, a
  closure of unknown scope, and a waiting upsert; a failure goes to its own session's flush, once, the batch's other writes
  saved; the next `queue` reports an earlier failure; 94 failures of one session never hide another's (per-session cap,
  all counted); id refused before queueing; a waiting write's own error leaves nothing behind; a COMMIT that fails (a
  deferred FK) is retried write by write (counter checked); a panicking write is reported and the writer goes on; drop
  drains 500 queued writes; writes after `shutdown` run inline; 4 threads x 100 writes keep each thread's order; **a killed
  process**: the test binary runs itself with `HERMES_STORE_CRASH_DB`, writes, flushes 100, queues 300 more and
  `process::exit(42)` (no destructors): the 100 are there, what landed of the rest is a prefix, `integrity_check` ok).
  The pragma test also checks the read connection is `query_only`.
- `hermes_llm`, `hermes_agent`, `hermes_tools`, `hermes_memory`, `hermes_prompts`: green, unchanged counts.
- `hermes_runtime --lib` 213. `turns` **258 passed / 7 ignored under the default, `HERMES_TEST_PROFILE=requests` and
  `tokens`** (256 + 2 new: `a_finished_turn_is_committed_before_the_page_hears_it_ended`: another `rusqlite`
  connection reads the turn's messages (same ids and order as `Runtime::messages`) and its usage row right after
  `TurnFinished`; `a_write_the_database_refuses_fails_the_turn_with_the_reason`: a test trigger refuses the usage row,
  the turn ends `Failed` naming SQLite's reason, the message and reply are saved, the next turn completes). No flake seen.
- `hermes-desktop` 183 + acp 68. `cargo build --workspace` OK (placeholder `target/hermes-src/.../dist-webapp` in the
  worktree, deleted afterwards).
- `cargo clippy -p hermes_store --tests`: nothing new (two redundant-clone hits in old tests exist at HEAD). rustfmt: the
  store crate formatted; my hunks in runtime/desktop are rustfmt-clean (those files have older unformatted code, left).
- Live, own instance (debug exe, port 47810, data `target/perf`; a `tools` junction to the main checkout's Node, removed
  afterwards): with `mock_llm.py`: `smoke.mjs` (export json/md/html, `/undo` rewind, rollback restore, compress, `/usage`,
  `/insights`, `insights.get`, `/api/sessions/search`, curator), `rollback-check.mjs`, `stop-check.mjs`; as their own
  provider: `side-and-branch-check.mjs`, `archive-check.mjs`, `preinject-check.mjs` (resume, rewind box, session search),
  `workspace-check.mjs`, `provider-error-check.mjs`: all passed; no `session database` error in the instance's log. The
  instance was stopped with `perf.ps1 -Do stop` (a kill). No perf/timing tools run (user's rule); no new check script.

Deviations from the plan:
- The plan says "a `flush()` awaitable for the places that need read-after-write". Instead every read waits for the
  writes sent before it (a barrier in the store), plus explicit `flush_session` at turn boundaries. Reason: ~60 read call
  sites in four crates, and one missed site (`reload_if_stale`) would silently rewind a live conversation; a barrier in the
  store cannot be forgotten by future code. The cost: a read during a streaming turn closes the open batch early.
- `flush` is blocking (the store crate has no tokio); the async wrapper is `flush_turn` in the runtime.
- Only the turn path queues; every other write still waits for its commit (unchanged API, needed by the GPUI app and by
  callers that use their write's result).

For the reviewer:
- **Read-after-write** (focus 1): every read method in `crates/store/src` goes through `self.read()`; there is no
  `self.read.lock()` outside `read()` and the pragma test. The table above lists the callers by area.
- **Nothing lost** (focus 2): `Drop`/`shutdown` drain (`dropping_the_store_commits_everything_queued`); `main.rs` Exit and
  ACP; the kill test; `flush_turn` before each of the four `TurnFinished` sends in runtime `lib.rs` (normal end, early-out,
  compress, panic guard). The GPUI app (frozen, not touched) relies on `Drop` and its turns' boundary flush; if its process
  exits without dropping the runtime, it loses at most the open ~5 ms batch of a turn that had not finished.
- **Errors** (focus 3): `queue` returns earlier failures (taken); `flush_session` / `after_save`; logged at `error`; the
  wording of the `Failed` status. Check you agree that a mid-turn `queue` consuming the error (the turn then fails with
  "N write(s) to the session database failed ..." rather than the `after_save` text) is acceptable.
- Lock order: `queue` mutex -> `state` mutex (settle); the writer takes only `state` and the connection, never `queue`.
  `queue` is called under the conversation's lock (no store lock is held while taking that lock). A writer thread that
  died outside the per-batch `catch_unwind` releases waiters through a drop guard and leaves a session-less failure.
- **Copy per chunk**: the queue holds an `Arc` of the streaming message until its batch commits, so the next chunk's
  `make_mut` copies it (PITFALLS updated). Net: one message clone per chunk on the turn's thread instead of HEAD's per-chunk
  JSON serialisation + upsert + FTS update + commit there. Not measured (no-perf rule).
- An in-memory store shares one connection between reader and writer (tests only); file stores have two.
- `superseded`'s rule (skip only a QUEUED upsert followed in the same batch by an upsert of the same row, with only
  other-row upserts of that session in between) and the commit-failure retry (writes are `Fn`; run twice only on the retry
  path, after the first run was rolled back).

### Review (Opus, 2026-11-15)
Read the whole diff against the plan's focus (read-after-write, nothing lost on exit or crash, errors reported) and
the list above. Four defects fixed, each with a test where a test can show it.
- **Fixed: a write whose error ends the whole transaction.** SQLite rolls a transaction back by itself on some errors
  (a full disk, an I/O error, an interrupt). The batch then went on: `ROLLBACK TO` failed silently, each following
  write's `SAVEPOINT` opened and its `RELEASE` committed a transaction of its own, the final `COMMIT` failed ("no
  transaction"), and the retry ran every write again: usage rows counted twice, a committed `create_session` answering
  "already exists" to its caller. Now `apply` checks `is_autocommit` after each write and, if the transaction is gone
  (nothing of the batch committed), reruns every write of the batch alone. Test
  `a_write_that_ends_the_transaction_never_runs_the_others_twice` (3 usage rows on the unfixed code, 2 now).
- **Fixed: deleting a session swallowed the writer's own failure.** `delete_session` cleared its leftovers with
  `take_failures(Some(id))`, which also takes the session-less "the writer stopped; queued writes were lost" entry,
  and dropped the result. New `Writer::discard_failures` removes only that session's. Test
  `deleting_a_session_keeps_a_failure_of_the_writer_itself` (fails on the unfixed code).
- **Fixed: a write during a shutdown could overtake the queue.** After `shutdown` closed the channel (on another
  thread: the exit hook) the writer was still draining, but a write refused by the closed channel ran inline at once,
  racing the thread's last batch for the connection (a late write landing before older queued ones of its session).
  A second `shutdown` (exit hook + `Drop`) also returned before the drain finished. Both now wait until the thread has
  stopped (`wait_stopped`; `stopped` is set at once when the thread could not be spawned). Test
  `a_write_during_a_shutdown_lands_after_the_queue` (a race on the unfixed code, which it passed by luck: it guards the
  contract rather than reproducing the bug).
- **Fixed: the early-out path cleared `running` before its flush**, so a next turn could start and have its writes'
  failures taken by this turn's `flush_session`. Now flushed first, as at the normal end, `/compress` and the panic
  guard.
- **Tightened** the kill test: what landed after the flush must be positions `0..n` with their own ids.
- **Coalescing (focus 1):** holds. Tried to build a counterexample: every write is tagged with the one session it
  touches (session writes by id; projects `None`, which stops the scan), and no table references another session
  (FKs go to `sessions(id)`; the FTS triggers stay within the row), so other rows' upserts and other sessions' writes
  between two upserts of `(S, seq)` cannot change what the second leaves. A replace, delete or title change in between
  stops the skip (tested). A skipped upsert whose replacement fails loses only the older version, and the failure is
  reported.
- **Order and turn boundaries:** one channel, one consumer; `seq` assigned under the queue lock; `done` moves only
  after a batch. All four `TurnFinished` sends flush first; cancel/stop ends through the normal path (`stop-check.mjs`);
  rewind / undo / rollback are waiting writes behind the queue; ACP uses the same turn path; no turn task is
  `abort()`ed. Subagents (`persist: false`) and reviews only queue usage, under the parent id.
- **Read-after-write (focus 2):** holds. Every read in `crates/store/src` goes through `read()`; nothing else opens
  `sessions.db` (`hermes_import.rs` reads Hermes's own `state.db`; `tools/index.rs` and `memory` use their own
  databases; only tests open it with `rusqlite`). `reload_if_stale` reads the conversation, then the (settled) store,
  so the store can only be ahead of its snapshot, never behind; that race predates CP13b. The barrier instead of the
  plan's explicit `flush()` per read point: **accepted**; a new read cannot forget it, and it costs an early-closed
  batch.
- **Deadlocks (focus 3):** none found. Lock order conversation -> queue -> state; the writer thread takes only state
  and the connection (never together) and no caller's lock; jobs are closures over owned data and never call the
  `Store`. `queue` under the conversation lock never blocks before shutdown (after it: an inline write, at exit only).
  An in-memory store's single connection would deadlock a caller holding a read guard while waiting for a write; no
  such caller exists (it would have self-deadlocked on the old single mutex too).
- **Errors (focus 4):** the mid-turn `queue` wording ("N write(s) to the session database failed (the first: ...)")
  as the turn's failure is acceptable: it names the database and SQLite's reason, and the turn stops as on a
  synchronous error. A dead writer thread: waiting callers get "the session database writer stopped", reads stop
  waiting (`stopped`), later writes run inline, the lost queue is one reported failure. No hang, no silent drop.
- **Schema / protocol / GPUI (focus 5):** schema untouched (`query_only` is per connection); ids and order checked by
  the turns test; GPUI app builds and gets the runtime's turn-end flush: only a turn still streaming when its process
  exits without dropping the runtime can lose its open batch (~5 ms).
- **Per-chunk copy / PITFALLS (focus 6):** accurate. PITFALLS gained: never `COMMIT`/`ROLLBACK` inside a write, and
  tag a write with the session it touches (coalescing relies on it).
- Tests after the review (CARGO_TARGET_DIR=target/claude): `hermes_store` 34 (31 + 3 new); `hermes_llm`,
  `hermes_agent`, `hermes_tools`, `hermes_memory`, `hermes_prompts` green, counts unchanged; `hermes_runtime --lib`
  213; `turns` **258 passed / 7 ignored under the default, `requests` and `tokens`**; `hermes-desktop` 183 + acp 68;
  `cargo build --workspace` OK (placeholder `target/hermes-src/apps/desktop/dist-webapp`, deleted afterwards). Live,
  own instance (debug, port 47810, `target/perf`): with `mock_llm.py` `smoke.mjs` (export, search, `/undo`, compress,
  `/usage`, `/insights`), `rollback-check.mjs`, `stop-check.mjs`; `archive-check.mjs`; `preinject-check.mjs` (resume,
  rewind box, session search) as its own provider: all passed; no `session database` line in the instance's log. No
  perf or timing checks (user's rule).

**User checklist** (no visible change; a sanity pass in the rebuilt app):
1. Rebuild and start the app (`cargo build -p hermes-desktop`, your usual run).
2. Send a message that makes the agent run a tool; while it streams, click another session and back: the history is the
   same as on screen.
3. After the reply ends, close the app at once (window X), start it again: the whole exchange is there.
4. Search the sidebar for a word from that reply: the session is found. Run `/usage`: the call is counted.
5. Edit an earlier message and resend (rewind): it continues from there; export the session (json/md): it matches.
6. Negative case: not triggerable by hand (a database write failure shows as the turn failing with "This turn was not
   fully saved to the session database: ..."; covered by the turns test).

## CP13c — `spawn_blocking` everywhere it's missing
Worktree: C:\Users\alexandros\repos\hermes-rs-p13   Branch: backend/p13-perf   Model: Sonnet 5.5 (subagent)
Not committed (an Opus reviewer commits). Phase 13 is not done (13d-13f remain).

Why now: CP13b made every `Store` read wait for the writer's pending commit, so a store read in async code can hold a
worker for a commit (before: only for a SELECT). The plan named `skills.rs:222` (git through `tokio::process::Command`:
that is already non-blocking, nothing to do there) and the `memory` / `session_search` / `skill_*` dispatch
(`tools/src/lib.rs`, which has moved). The audit found `memory` and `skill_*` already on `spawn_blocking`;
`session_search` was not, and neither were many others (below).

**Method.** A throw-away script (not committed) parsed every non-test `.rs` file under `crates/`, found each `async fn`
and `async {}` block, removed the spans inside `spawn_blocking(..)` / `block_in_place(..)`, and listed what was left that
matches `std::fs`, `File::`, `std::process`, `Command::new` (non-tokio), `thread::sleep`, `reqwest::blocking`,
`std::net`, `.exists()/.is_dir()/.is_file()/.metadata()/canonicalize/read_dir`, every `Store` method, and the
`SkillStore` / `MemoryStore` / checkpoint / staging methods; a second pass listed the sync helpers containing those
patterns and their callers in async code. Each hit was then read in context (the script has false positives:
`self.blocking(..)` calls, tokio's own `fs::read`, names like `.find` / `.take`). Also read by hand: the gateway's
`dispatch` / `mutate` / `rest` / `session_route` / turn event loop, the ACP server, `ToolProvider::call` in
`tools/src/lib.rs`, the runtime's turn task. The agent crate (`crates/agent`) had no hit. Not exhaustive for: a `std`
mutex guard held across an `.await` (spot-checked, none found) and sync helpers reached through trait objects.

**Inventory** (W = wrapped in `spawn_blocking` / the gateway's `blocking` helper, C = converted to tokio async,
L = left, with the reason):

| # | Where | What blocked | Decision |
|---|---|---|---|
| 1 | `tools/recall.rs` `session_search` (via `dispatch_unredacted`) | FTS search, `list_sessions`, `search_rows` | W: `Recall { session_id }` runs in `spawn_blocking`; a JoinError answers "Session search stopped: ..." |
| 2 | `tools/lib.rs` `skills_tool`, `stage_skills`, `review.rs` skill_manage: the provenance note | the first threat scan compiles its regexes (~0.5 s in a debug build); found by the new test | W: the note is made inside the closure (new `SourceNote::for_session`) |
| 3 | `tools/review.rs` skill_manage guard (`skills.find` walks the skills folders, `exists`, `canonicalize`) + `manage_with_source` (writes) | disk | W: `guard_skill_manage` is a free fn run in `spawn_blocking` over a copy of the read set; the write in `spawn_blocking` |
| 4 | `tools/review.rs` `mark_read` (`canonicalize`), `viewed_path` (`skills.find`) | disk | W (both async now) |
| 5 | `tools/lib.rs` `checkpoint_before` (git snapshot before `write_file` / `patch` / destructive terminal / `run_script`) | `git` child processes, seconds in a big repo | W: still awaited before the change; `hints` and `turn_checkpoints` are `Arc<Mutex<..>>` so the lock is taken inside the closure |
| 6 | `tools/lib.rs` `call`: `checkpoints.record_write` | hash + checkpoint store | W |
| 7 | `tools/lib.rs` `call`: `hints.check` (reads AGENTS.md / CLAUDE.md of a newly visited folder) | disk | W |
| 8 | `tools/files.rs` `write_file`, `patch`: `lint::delta` | runs `python -m py_compile` / `node --check` and waits | W (`lint_off_worker`) |
| 9 | `tools/shell.rs` `limit_output` (terminal, `run_script`) | writes the full output when over the cap | W only when over the cap (`limit_output_off_worker`; a normal output costs nothing) |
| 10 | `tools/web.rs` `extract`: `truncate_with_footer` -> `store_full_text` | writes the page under `cache/web` | W only for a page over the limit (`truncate_off_worker`; the two fns are free fns over `cache_dir`) |
| 11 | `tools/lib.rs` `check_command`: `ApprovalStore::allow_always` | saves the always-allowed file | W |
| 12 | `tools/vision.rs` `load_source`, local path | `metadata` + `read` of an image up to 50 MB | C: `tokio::fs` |
| 13 | `tools/process.rs` `spawn`, `shell.rs` `run`: `create_dir_all(scratch)` per call | mkdir | C: `tokio::fs::create_dir_all` |
| 14 | `runtime/lib.rs` `delete_session`: `store.delete_session` + `remove_dir_all(scratch)` | a write that waits for its commit + a tree delete | W: one closure, order unchanged (store, scratch, taint file); the store's error is still returned |
| 15 | `runtime/ops.rs` `spill_oversized` (an over-limit tool result is saved) | file write | W only over the limit (`spill_oversized_off_worker`) |
| 16 | `runtime/lib.rs` turn task: `user_message` (`references::expand` reads `@file` references) | disk | W |
| 17 | `runtime/lib.rs` turn task and `review_schedule.rs` `review_once`: `index::project_name` | stats up the folder tree for `.git` | W (the review's moved into its existing closure) |
| 18 | `desktop/gateway.rs` `dispatch`: `projects.tree/list/create/update/delete`, `session.active_list`, `session.title`, `session.rewind`, `rollback.*`, `session.export`, `session.branch*`, `file.attach`, `image.attach` / `image.attach_bytes`, `insights.get` (all sessions + counts), `session.create` (builds the system prompt: skills, memory, files; store writes), `session.resume` / `session.history` (whole conversation), `prompt.submit` (+ its model-facts row), `complete.path`, `config.set model`, `commands.catalog`, `complete.slash`, `slash.exec` (`coding_command`, `commands::exec`), `command.dispatch`, `session.delete` (archive file) | SQLite, files, skills folder, git | W through the new `Gateway::blocking(\|g\| ..)` |
| 19 | `gateway.rs` `mutate` (rename, model, rewind, archive), `builtin_command` (`/usage`, `/insights`, `/diff`), `status_text`, `context_text`, `window_for`, `context_breakdown`, `compress_session` | store reads, whole conversation, git | W (these take `self: &Arc<Self>` now) |
| 20 | `gateway.rs` the turn's event loop (one task per turn): the heartbeat's `session_info` + `usage_snapshot` (every 5 s), `Usage` events (`note_generation` writes a file, `usage_snapshot` reads the store), `TurnFinished` (`messages()` loads the conversation, a session row, `usage_snapshot`), the end (`apply_pending_route` + `run_queued`, which reads settings and submits the next prompt), `title_session`'s `set_title` | store + file on the turn's own task | W |
| 21 | `gateway.rs` `rest`: `session_list`, `stored_messages`, `session_route` (GET/PATCH: store + archive file; DELETE: the runtime's, then the archive file), the final `self.rest.handle(..)` (all generic routes: config, env, ...), `/api/logs` | store, config files | W; `/api/logs` C (`tokio::fs`) |
| 22 | `llm/canonical/registry.rs` `fetch_remote_catalog`: etag file read | tiny read; the function is called nowhere | L: dead code, Phase 14 removes the canonical registry |
| 23 | `Settings::load(data_dir)` at the top of every `dispatch` and in `model_facts`, `context_length`, `turn_options`, `known_context`, `rest`; `rest.config_value` / `set_config_value` (read / write `desktop-config.json`) | one small JSON file, read per request | L: short; a cache is a design change (CP13d/f, when `dispatch` is restructured). Flagged below |
| 24 | `gateway.rs` single-row store reads left inline: `turn_options`, `session_info` (turn start, `route_session`, `title_session`), `session_provider` (`prompt.btw`, `prompt.background`), `model_options`, `model_facts`, `provider_for`, `file_path` | one keyed SELECT (waits for at most the open batch's commit) | L: short; the multi-row, writing and file ones are in 18-21 |
| 25 | `Path::is_dir/exists/is_file` single stats: `files.rs` (`write_file`, `read_file`), `search.rs`, `shell.rs` (`workdir`), `script.rs` | one `stat` | L: short (a network drive is the only slow case) |
| 26 | `runtime/curator/per_cluster.rs` `propose::remember` (the curator's "seen" file, once per job) | tiny file | L: a background pass, a few writes of a few hundred bytes |
| 27 | `runtime/ledger.rs`, `signals.rs`, the store writer | own threads | L: already off the workers |
| 28 | `desktop/acp/*` | `spawn_blocking` already around every store and file read; its calls into the gateway (`session.create` ...) go through `dispatch`, now off the workers | L (nothing to do) |
| 29 | `tools/skills.rs`, `skill_manage.rs`, `memory`, `search.rs`, `script.rs`, `files.rs` reads, `curator.rs`, `staging.rs`, `index.rs` | already `spawn_blocking` / `tokio::fs` | L: already correct |

Counts: wrapped: rows 1-11 (11 tool-side), 14-17 (4 runtime sites), 18-21 (the gateway: ~60 arms and helpers); converted
to tokio async: 3 (rows 12, 13, 21's logs); left with a reason: 8 (rows 22-29, the last two are "already correct").
Error-path behaviour only: a failed thread in the gateway answers an RPC error ("The request stopped: ..."); `complete.path`
used to answer an empty list when its thread died, now an error; `spill_oversized` answers "not spilled" (the full text goes
to the model) if its thread died.

**The test** (`crates/tools/src/blocking_tests.rs`, `slow` hook in `tools/lib.rs`; see TESTING.md and PITFALLS.md). It
found row 2: a skill write stalled the worker ~0.5 s in a debug build (the first threat scan compiles its patterns; in
release it is shorter, but it is the same work on the same worker).

Files: crates/tools/src/{lib,recall,review,files,shell,script,process,vision,web,provenance,blocking_tests}.rs,
crates/runtime/src/{lib,ops,review_schedule}.rs, crates/desktop/src/gateway.rs, crates/llm/src/canonical/registry.rs (not
changed: dead code), docs/{ARCHITECTURE,PITFALLS,TESTING}.md, docs/backend/progress/phase-13.md

Tests (CARGO_TARGET_DIR=target/claude):
- `hermes_agent` 45, `hermes_llm` 489 (+7 ignored) + 5 integration, `hermes_memory` 101, `hermes_prompts` 37, `hermes_store` 34,
  `hermes_tools` **335** (329 + the 6 new `blocking_tests`): green. `hermes_runtime --lib` 213.
- `turns` **258 passed / 7 ignored under the default, `HERMES_TEST_PROFILE=requests` and `tokens`** (counts as CP13b).
  A first full run failed `stopping_mid_command_keeps_the_output_it_had_printed` in all three profiles (the chat got
  "Tool call was interrupted before completing" instead of the printed output). Cause: the machine races the step against
  Stop with `biased` priority for the call, so a call that still has an `.await` to get through when Stop fires is
  dropped; my first version of `SessionTools::call` ended with two `spawn_blocking` hops (hints, `record_write`). Fixed:
  once `emit.cancel_token().is_cancelled()`, the follow-ups (and `limit_output_off_worker`, `spill_oversized_off_worker`)
  run inline as before (PITFALLS). Full suite re-run green after the fix, all three profiles.
- `hermes-desktop` 183 + acp 68. `cargo build --workspace` OK (placeholder `target/hermes-src/apps/desktop/dist-webapp`
  in the worktree and a `tools` junction to the main checkout's Node: both removed afterwards).
- Live, own instance (debug exe, port 47810, data `target/perf`): as their own provider `staging-check` (memory staging),
  `coding-commands-check` (`/review` subagent, `/plan`...), `attach-check` (`file.attach`, `image.attach*`; on a fresh
  instance: after other checks it fails "model does not support vision" because the instance caches the model's facts once
  asked, not a regression), `preinject-check` (skills attached, session search, resume, rewind), `review-check`
  (skill_manage by the review), `side-and-branch-check`, `workspace-check`, `memory-check` (Star map: skills and memory
  through the stores), `toolsets-check`, `archive-check`: all passed. With `mock_llm.py`: `smoke.mjs` (`session.create`,
  rollback, `/status`, `/context`, `/usage`, `/diff`, `/tools`, `/memory`, `/insights`, `insights.get`, exports, `/undo`,
  `session.compress`, curator), `rollback-check.mjs`, `stop-check.mjs`: all passed. No `The request stopped` / panic /
  session-database line in the instance's log. No new check script (nothing new for the page); no perf or timing checks
  (the user's rule); the one timing-sensitive test is `blocking_tests`. Cleanup slip: stopping `mock_llm.py` I
  killed every process whose command line named it (3), which may have included another session's mock; nothing else outside
  `target\claude` was stopped.
- Caution for future live runs: `smoke.mjs` run with a working folder inside a repository makes `rollback.restore` act on
  that repository's checkpoint (here the worktree itself; it restored the same state, nothing was lost). Use a folder
  outside any repository, as `rollback-check.mjs` documents.


For the reviewer:
- **Order.** `checkpoint_before` is awaited before the call it guards. The gateway's `blocking` hops keep per-request
  order (each request was already its own task); `prompt.submit` runs `submit` on a blocking thread, then the receipt
  goes out and `release_admission` runs as before (`handle`). `run_queued` (the next queued prompt) now runs on the
  blocking pool after `apply_pending_route`, in that order. The turn's `TurnFinished` handler awaits the store reads
  before sending `message.complete`; the order of the events the page sees is the same.
- **Locks.** No `std` / `parking_lot` guard crosses an `.await` or enters a closure; `hints` and `turn_checkpoints` became
  `Arc<Mutex<..>>` and are locked inside the closure (nothing else used them). The conversation lock is taken only inside
  the runtime's own sync methods that the closures call, as before.
- **`session.create`, `session.resume`, `prompt.submit` on the blocking pool**: they call `Runtime::create_session`,
  `messages`, `send` (conversation lock, `TurnLock::acquire`). `submit` `tokio::spawn`s the turn's task from inside the
  closure (a runtime handle exists on blocking threads). Please check you agree.
- The timer test is wall-clock based (300 ms delay, 150 ms tolerance).
- **Stop** (found by `turns`): after Stop, a tool call must not wait (see Tests above and PITFALLS). The follow-ups after a
  tool's own work check the cancel token and run inline. Still open, rare: a Stop landing exactly while `lint_off_worker`
  or `truncate_off_worker` runs loses that call's result (the step is dropped at the hop). Say if you want those inline-on-Stop
  too.
- **Not audited**: `Mutex` guards held across `.await` (spot check only). The frozen GPUI app (`crates/app`) is untouched
  and calls the sync runtime from its own executor.
- Doubts: (a) row 23, `Settings::load` per request, is the largest remaining blocking-ish call on the workers, but each
  read is a few hundred bytes; (b) row 24 leaves single-row store reads in async handlers, each of which can wait one
  commit during a streaming turn (CP13b's barrier); (c) the new `gateway.rs` hunks are not rustfmt-formatted (the file has
  older unformatted code; I did not reformat it; `cargo fmt -p hermes_tools` was run once by mistake and undone, so the
  tools files are as they were apart from my hunks).

**User checklist** (no visible change; the page should feel the same):
1. Rebuild and start the app. Open the sidebar, switch between sessions and projects: the lists and histories show as before.
2. Ask the agent to write a Python file with a syntax error: the result carries the lint warning as before; during a long
   tool call the Stop button and the sidebar stay responsive.
3. `/usage`, `/insights`, `/status`, `/context`, `/compress`, `/save json` and `/rollback` each answer as before.
4. Rename a session, archive and unarchive it, delete another: each applies at once.
5. Ask the agent to remember something and to save a skill (`memory`, `skill_manage`): both are saved; Settings ▸ Skills lists it.

### Review (Opus, 2026-11-15)
Read the whole diff, then made an audit of my own (not the implementer's script): every `async fn`, `async {}` and
spawned task in `crates/desktop/src` (gateway, ACP, server, REST), `crates/runtime/src` and `crates/tools/src`, for
`std::fs`, `std::process`, `Store`/`rusqlite`, `SkillStore`/`MemoryStore`, checkpoints/git, `thread::sleep`, lazily
compiled regexes, and the sync helpers they call (`session_info`, `usage_snapshot`, `end_live`, `enqueue`,
`Runtime::messages` / `open`). Findings:
- **Fixed: the turn's own task still read the store on the worker** at four points the inventory missed:
  `session.info` at the turn's start (right after the prompt was queued, so it waited for that commit), `end_live`
  (twice: the event stream closing, and after `message.complete`), `enqueue` for an undelivered steer, and
  `title_session`'s `session.info` after the title. Each `session_info` is two store reads (the row, the usage
  totals). Now through `Gateway::blocking`, awaited in place, so the page sees the same events in the same order
  (`end_live_off_worker` still marks the session settled if its thread dies).
- **Fixed: a failed thread in a REST route answered Hermes's 404.** `rest()` mapped a `JoinError` with `.ok()` to
  `None`, which is "No such API endpoint": the page reads it as "feature absent" (rule 5: a wrong answer is worse than
  none). New `Gateway::rest_blocking` answers 500 `{"detail": "The request stopped: ..."}`; used for
  `/api/profiles/sessions`, `/api/sessions/{id}/messages`, `/api/sessions/{id}` (GET/PATCH, and the DELETE lookup)
  and the generic routes.
- **Fixed: Stop could still lose a stopped command's output** (focus 3). The implementer's work-around checked
  `is_cancelled()` once, before each follow-up hop; a Stop landing while the hop ran (hints, `record_write`,
  `limit_output_off_worker`, `spill_oversized_off_worker`) still dropped the call. New
  `hermes_tools::off_worker_unless_stopped(cancel, work)`: on the blocking pool, but once Stop fires the future
  completes in that same poll: work not yet started runs inline, work already running is waited for through a std
  channel (no `.await`; the same wait the pre-Phase-13 inline call had). Cancel stays immediate for everything else.
  `lint_off_worker` / `truncate_off_worker` are left plain on purpose: `write_file`/`patch`/`web_extract` already
  had real `.await`s (`tokio::fs`, the network) where Stop drops them, as at HEAD; only the terminal and `run_script`
  are built to answer after Stop, and their follow-ups are all shielded now. Tests:
  `stop_during_a_follow_up_keeps_the_calls_result` (raced like `agent/machine.rs`; its control shows a plain hop
  losing the result) and `a_follow_up_without_stop_does_not_block_the_worker`.
- Doc fix: `shell.rs`'s `limit_output` doc line had been moved onto the new wrapper.
- **Rows left, re-checked against CP13b's read barrier**: `Settings::load` / `config_value` (row 23) read small JSON
  files, never the store: unaffected by CP13b, left. Single-row store reads still inline (row 24): `model_options`
  (`model.options`), `session_provider` (`/btw`, background prompt), `config.set` effort / `mutate` / compress /
  queue edits' `session.info`, `file_path` (file serving), `Runtime::side_question`'s `messages` (`/btw`) — each a
  keyed SELECT behind at most one open batch (≤5 ms plus a WAL commit), on a user action, not per turn or per
  chunk: accepted. Every per-turn and periodic path (start, heartbeat, usage, finish, end, title, queued next
  prompt) is now off the workers. `Runtime::review_once` opens the session inline but only tests call it.
  Lazily compiled regexes: the threat scan was the only slow one found (row 2); the redaction patterns are small.
- **Wrapping correctness (focus 2)**: no `std`/`parking_lot` guard crosses an `.await` or enters a closure
  (`self.read.lock().clone()` is a temporary dropped at the end of its `let`); `checkpoint_before` is awaited before
  the call it guards (a Stop during it drops the call before the change; the detached snapshot finishes and holds
  `turn_checkpoints` only inside its thread); `JoinError`s become error values everywhere (RPC `-32000` "The request
  stopped: ...", tool errors); `-32601` and every other shape unchanged. `submit` / `session.create` /
  `session.resume` on the blocking pool: `spawn_blocking` threads run inside the runtime's context, so
  `tokio::spawn` works there in the app, in ACP (same `dispatch`) and in the `current_thread` test runtimes; the
  gateway uses no task-locals and no tracing spans (`instrument`/`info_span` absent), so nothing is lost by the hop.
  The receipt-before-events order holds (`starts` is filled inside `submit`, released by `handle` after the reply).
- **Timer test (focus 4)**: `slow` is `#[cfg(test)]` in `hermes_tools` only (absent from every other crate and from
  release/debug builds); keyed by session id, each test its own id. Mutation check: making `session_search` run
  inline fails `session_search_does_not_block_the_worker` ("stalled for 315 ms"). Ran `blocking_tests` 10 times alone
  and 3 times inside the full `hermes_tools` suite (parallel tests as load): all green. `test_pause` is the first line
  inside each blocking closure (memory, staged memory, staged skills, skills tools, session search).
- **Process hygiene (focus 6)**: the worktree's `git status` lists only the CP13c files. **My own slip**: one of my
  PowerShell commands used `[IO.File]::ReadAllText` with a relative path, which .NET resolved against the main
  checkout, and wrote back five of my gateway edits there (`crates/desktop/src/gateway.rs` in
  `C:\Users\alexandros\repos\hermes-rs`); found by `git status` there within minutes, it was only my hunks, restored
  with `git checkout -- crates/desktop/src/gateway.rs`; the main checkout is clean again (nothing built from it).
  Of the implementer's report: its `mock_llm.py` kills by name cannot be undone; nothing else outside the worktree
  was found changed.
- Tests after the review (CARGO_TARGET_DIR=target/claude): `hermes_store` 34, `hermes_llm` 489 (+7 ignored) + 5,
  `hermes_agent` 45, `hermes_memory` 101, `hermes_prompts` 37, `hermes_tools` **337** (335 + 2 new);
  `hermes_runtime --lib` 213; `turns` **258 passed / 7 ignored under the default, `requests` and `tokens`**;
  `hermes-desktop` 183 + acp 68; `cargo build --workspace` OK (placeholder `target/hermes-src/apps/desktop/dist-webapp`,
  deleted afterwards); `cargo clippy -W clippy::await_holding_lock` on tools/runtime/desktop: no hit.
- Live, own instance (debug, port 47810, `target/perf`; the main checkout's `tools/node/node.exe` run in place; working
  folders in my scratch folder, outside any repository): with `mock_llm.py` (started and stopped by its PID)
  `smoke.mjs`, `rollback-check.mjs`, `stop-check.mjs` (both cases: stalled reply, slow command); as their own provider
  `staging-check`, `memory-check`, `preinject-check` (skills, session search, resume, rewind), `review-check`,
  `attach-check` (on a fresh instance: after other checks it fails "model does not support vision" because the
  instance caches the model's facts, as the implementer noted), `side-and-branch-check`, `archive-check`,
  `workspace-check`: all passed. No "The request stopped", panic or session-database line in the instance's log. One
  false start of mine: `perf.ps1 -Do start` hung on its timing step (no `tools/node` in the worktree), so my first
  `smoke.mjs` ran before its working folder existed and found no checkpoint; with the instance started directly
  (perf.ps1's environment, no timing) every run passed. No perf or timing checks (the user's rule).

## CP13d — per-session actor
Worktree: C:\Users\alexandros\repos\hermes-rs-p13   Branch: backend/p13-perf   Model: Opus 5.5 (subagent)
On top of 8e3d12d (CP13a-c). Not committed (an Opus reviewer reviews, splits and commits). Phase 13 is not done
(13e-13f remain).

### Inventory (before CP13d)
Every lock, map and atomic keyed by (or living on) a session, on `OpenSession` (`crates/runtime/src/session.rs`),
`Runtime` (`crates/runtime/src/lib.rs`) and `Gateway` (`crates/desktop/src/gateway.rs`). "Turn task" is the task
`begin_turn` spawns; "blocking" a `spawn_blocking` thread (CP13c moved most gateway work there); "worker" an async
worker of the 4-thread runtime; "GPUI" the frozen app's own threads.

| # | State | Kind | Written by (where, thread) | Read by | Notes / invariants |
|---|---|---|---|---|---|
| R1 | `Runtime.sessions` | `Mutex<HashMap<id, Arc<OpenSession>>>` | `open` (insert-if-absent after a store load: blocking / GPUI / a worker in tests), `evict` (idle check + remove under the map lock), `close_quietly` (remove); shared with the scheduler (`Env.open`) | every `Runtime` method (`open`, `is_running`, `processes`, `steer`, `cancel`, `yolo`...), `review_schedule` (`turn_running`, `view`, `claim`), the curator (`anything_running`) | held only for get / insert / remove, except in `evict`, which held it across the `running` check (map -> running) |
| O1 | `OpenSession.record` | `Mutex<SessionRecord>` | `set_session_route` (model switch: blocking), `apply_compaction` (turn task, `/compress` task) | everywhere: the id per streamed chunk (`TurnRuntime::append`), model / provider / cwd / prompt per turn, the snapshot per pass, review, subagents, curator | data; taken alone or before `conversation` ("one lock at a time") |
| O2 | `OpenSession.conversation` | `Mutex<Conversation>` | the turn task per chunk (`append`, `update_message`, effects; the store queue is called *under* this lock, CP13b), `rewind_as`, `reload_if_stale`, `add_command_output`, `apply_compaction` | the snapshot per pass, the gateway's transcripts (`messages`), the review / scheduler views, `usage_text`, `context_breakdown`, `session_tool_definitions`, signals | data; CP13b's order rule needs the store queue under it |
| O3 | `OpenSession.context` | `Mutex<Option<(tokens, len)>>` | the turn task (`RecordUsage`); reset by compaction, rewind, reload | `snapshot` per pass | data, tied to the conversation's length |
| O4 | `OpenSession.running` | `Mutex<Option<RunGuard>>` (token + `TurnLock`) | `begin_turn` / `compress` (claim under the lock: check, take the `TurnLock`, set); cleared **by hand at 8 points** (`begin_turn`: 2 early errors, the early-out path, the normal end; the panic guard task; `compress`: 2 errors and its end) | `is_running` (the gateway's page state, the archive sweep, `note_session_left`), idle checks (`set_session_route`, `rewind_as`, `refine`, `idle_session`, `open_fresh`, `evict`), `cancel` and `close_quietly` (the token), the scheduler's `turn_running`, the curator, the review's pause | lock order **running -> review** (`ReviewSetup::start`), **running -> conversation** (the review's paused snapshot) |
| O5 | `OpenSession.turn_state` | `Mutex<Option<Arc<Mutex<TurnState>>>>` | set in `begin_turn` a few steps after the claim; cleared with `running` (3 points) | `steer`, `set_steer` (clone the inner `Arc`, then lock it) | the inner `TurnState` is the turn's own loop state (its steps lock it), not session state |
| O6 | `OpenSession.review` | `Mutex<Option<ReviewHandle>>` | `ReviewSetup::start` (under `running`: check the scheduler's busy flag, keep / replace a paused review, cancel the old one), `review::run`'s end (clear unless replaced) | the scheduler's `claim` (after setting its busy flag), the curator | a **Dekker pair** with the scheduler: the agentic side checks busy and sets the slot under the slot's lock; the scheduler sets busy, then reads the slot: one of the two always sees the other. `ReviewHandle.started` (atomic) is set by the paused review under `running` |
| O7 | `OpenSession.nudges` | `Mutex<Nudges>` | `begin_turn` (`tick_memory`), the turn's end (`finish_turn`) | — | seeded from the conversation at load (`Nudges::resumed`) |
| G1 | `Gateway.queued` | `Mutex<HashMap<id, VecDeque<params>>>` | `submit` (enqueue when `is_running`: blocking), `enqueue` at the front for a redirect / an undelivered steer, `dequeue` (`prompt.cancel`, `prompt.steer_queued`, `prompt.replace_queued`), `run_queued` (pop the front at a turn's end: blocking) | `pending_rows` (`session_info`, `session.resume`), `has_queued` (a turn's end decides whether the page hears `running: false`) | **order**: front for a redirect / a missed steer, back otherwise; one runs at each turn's end. A check-then-act race: `submit` read `is_running` and enqueued in two steps (a turn ending in between left the prompt queued with nobody to run it) |
| G2 | `Gateway.live_turns` | `Mutex<HashSet<id>>` | `set_live(true)` after a turn starts; `end_live` (a turn's end without a queue, a failed queued prompt) | `is_live` = runtime running OR live (`session_info.running`, `session.active_list`) | keeps the page's turn live across a queue |
| G3 | `Gateway.pending_routes` | `Mutex<HashMap<id, (provider, model)>>` | `switch_model` while running (set / withdraw), `apply_pending_route` (taken at the turn's end) | `session_info`, `model_options` | applied after the turn, before the next queued prompt |
| G4 | `Gateway.efforts` | `Mutex<HashMap<id, String>>` | `config.set reasoning` (worker), `session.mutate reasoning` | `turn_options` (each turn), `session_info`, ACP `settle_effort` / `effort` | exists for ids never loaded (set before a turn); kept for the process lifetime |
| G5 | `Gateway.revisions` | `Mutex<HashMap<id, u64>>` | `bump_revision` (`session.mutate`, `session.rewind`) | `session_info`, the `session.create/resume/branch` replies | default 1 |
| G6 | `Gateway.pending_images` | `Mutex<HashMap<id, Vec<path>>>` | `image.attach*` (worker), `image.detach` | `submit` (takes them) | may exist before the session is loaded (a fresh `session.create` is not loaded) |
| G7 | `Gateway.generation` | `Mutex<HashMap<id, (u64, f64)>>` + `generation-stats.json` | `note_generation` (blocking) | `usage_snapshot` | the file holds every session: each write needs all of them |
| G8 | `Gateway.starts` | `Mutex<HashMap<admission id, oneshot>>` | `submit` (gated) | `release_admission` (after the reply) | keyed by admission; lives for one RPC |
| G9 | `Gateway.context_lengths` | map by `provider\nmodel` | model facts | usage, turn options | not per session |
| G10 | `Gateway.file_tickets`, `branch_keys`, `viewing` | maps by ticket, idempotency key, page socket | | | not per session (`viewing` relates pages to sessions: cross-session) |
| G11 | `Hub.subscribers`, `Hub.prompts` (`hub.rs`) | session -> socket; prompt id -> a waiting answer | subscribe on create / resume / submit; the bridge (approval / clarify from inside a tool call) | every `emit` (per streamed chunk) | the event route: every page event goes through it |
| S1 | `review_schedule::Shared.states` | map by session (timers, busy, back-off) | the scheduler | | its own mutex |
| A1 | `Runtime.subagents`, ACP `AcpServer.sessions` | | | | their own registries (subagent id; the ACP thread's state) |

Cross-map invariants found: (1) O4 / O6 / S1: the review's pause and the one-review-at-a-time rule (above);
(2) O4 + G1 + G2: the page sees `running` = O4 or G2, and a turn's end runs G1's front prompt; (3) at a turn's end
O4 is released, then G3 is applied, then G1's front runs, in that order; (4) R1 + O4 in `evict` (the idle check and
the removal together). Lock order today: map -> running (`evict`); running -> review -> scheduler.states
(`ReviewSetup::start`); running -> conversation (the review's pause); record before conversation; conversation ->
store queue (CP13b). No gateway map lock is held while another is taken (each is locked for one get / insert /
remove).

### Design
`crates/runtime/src/session_actor.rs` (tests in `session_actor/tests.rs`). One **thread** per session
("hermes-session-<id end>", 256 KiB stack) owns `SessionState`; callers hold a cloneable `SessionHandle`
(`std::sync::mpsc::Sender<Command>` + the published status). `Runtime.sessions` is a `SessionRegistry` (id ->
handle), the one map by session that stays.

**What moved into the actor** (one owner, each change one command):

| Before | Now (`SessionState` / `PageState`) | Commands |
|---|---|---|
| R1 `Runtime.sessions` (the loaded `OpenSession`s) | `open: Option<Arc<OpenSession>>` | `Attach` (keep the first of two racing loads), `Detach { only_idle }` (`evict`: the idle check and the drop in one step; `close`) |
| O4 `running` + O5 `turn_state` | `run: Option<RunInfo { claim, cancel, turn_state }>`, `claims` | `ClaimRun` (-> claim number or `Busy`), `Release { claim }` (stale numbers ignored) |
| O6 `review` (+ `ReviewHandle.started`) | `review: Option<ReviewHandle>` | `ClaimReview` (the whole of `ReviewSetup::start`'s locked section, with the scheduler's busy check run inside), `ReleaseReview { started }` (only its own slot), `ReviewSlotTaken`, `IdleEpoch` / `ConfirmIdle` (the paused review's snapshot) |
| O7 `nudges` | `nudges` (seeded by `Attach`) | `TickMemory`, `FinishTurn` |
| G1 `queued` | `page.queue` | `Enqueue { front, only_if_running }`, `Dequeue { admission }`, `TakeNext` |
| G2 `live_turns` | `page.live` | `SetLive` |
| G3 `pending_routes` | `page.pending_route` | `SetPendingRoute` (kept only while running, one step), `TakePendingRoute` |
| G4 `efforts` | `page.effort` | `SetEffort` |
| G5 `revisions` | `page.revision` (starts at 1) | `BumpRevision` |
| G6 `pending_images` | `page.images` | `AttachImage`, `DetachImage`, `TakeImages` |
| — | — | `Barrier` (answers once everything sent before is handled) |

**What stayed, on purpose:**
- `OpenSession.record`, `.conversation`, `.context` (O1-O3): the session's *data*, not its control state. The turn
  writes the conversation per streamed chunk and must queue the store write *under* its lock (CP13b's order rule);
  every reader is synchronous code (the snapshot per pass, transcripts, reviews, `/usage`); a review's, a
  subagent's and the curator's copies are `OpenSession`s without an actor. A command per chunk would put the
  actor on the hot path for nothing: while a turn holds the run slot it is the only writer (rewind, `/compress`,
  model switch and the other writers refuse while it runs, as before).
- `Gateway.generation` (G7): one file holds every session's numbers, so each write needs all of them.
- `Gateway.starts` (G8, by admission id, one RPC), `context_lengths` (by model), `file_tickets`, `branch_keys`,
  `viewing` (G9-G10): not per session.
- `Hub` (G11): the event route; every `emit` (per streamed chunk) reads it, and the bridge's open questions
  (approval / clarify) are keyed by prompt id. Order and shapes of those events are untouched.
- The scheduler's `states` (S1), `Subagents`, ACP's `AcpServer.sessions` (A1): their own components.

**Commands and replies.** Every command in the app is waited for (`ask` from synchronous code: a std channel;
`ask_async` from async code: a tokio oneshot, the task waits, the worker doesn't). Even "set" commands are waited
for, so their effect is in the status the caller (or anyone it tells) reads next; there is no fire-and-forget in
the app (`tell` is test-only). The actor **publishes** a `SessionStatus` (one `Arc`, swapped whole after each
command, before the answer goes out): `is_running`, `turn_state`, `is_live`, `page()`, `open()`, `reviewing()`
read it without a round trip. A read that must be ordered against another caller's command uses a command
(`ReviewSlotTaken`, the `only_if_running` forms).

**Consistent snapshots.** A status is one consistent picture (all fields from the same moment), e.g.
`is_live` = run slot or `page.live` from one status (before: two locks taken at different moments), and
`session_info`'s queue rows, effort, revision and pending route come from it. It reflects every command whose
answer the reader already has.

**Why a thread, not a tokio task.** `Runtime::start_turn`, `compress`, `refine`, `ReviewSetup::start` are
synchronous and must know at once whether they got the session; the `turns` tests (and the GPUI app) call them from
a one-thread tokio runtime. A task-based actor could not run while that thread waits for its answer (deadlock); a
thread of its own always can. Its handlers are in-memory, so a synchronous wait is bounded like taking a mutex
(`a_waiting_call_on_a_one_thread_runtime_does_not_deadlock`).

**The actor never blocks.** Handlers only change `SessionState`: no store, file, network or process work, no
`.await`, no call to any handle. Two things are kept out of it for that reason: the cross-process `TurnLock` (file
open and delete) is taken and dropped by the turn's `RunGuard`, not the actor; and the store is never touched (the
CP13b queue is used by the turn itself, as before). The one outside lock a handler takes is the review scheduler's
`states` (`ClaimReview` calls `Scheduler::is_busy`), see the rules below.

**Unbounded channel.** Commands come at the pace of user actions and turn boundaries, never per chunk; senders
include `Drop` (the `RunGuard`) and code holding other locks, where a full bounded queue would add a way to block
or deadlock and buy nothing.

**Cancel stays immediate.** Stop never goes through the queue: `ClaimRun` publishes the turn's
`CancellationToken`, and `SessionHandle::cancel` (`Runtime::cancel`, `close`, `session.interrupt`,
`session.redirect`) cancels it straight from the status, even while the actor is busy with queued commands
(`cancel_reaches_the_turn_while_the_actor_is_held_up`). The CP13c rule (a tool call dropped by Stop keeps its
output) is untouched: nothing in the tool or machine path changed.

**RAII for the run slot.** `RunGuard` (session.rs) is now the claim: `RunGuard::claim(handle, token,
turn_state)` asks `ClaimRun`; `hold(TurnLock)` keeps the cross-process lock; `Drop` drops the lock first, then
waits for `Release`. The 8 places that reset `running` / `turn_state` by hand are gone: `begin_turn`'s and
`compress`'s early errors just return (`?`), the turn's early-out and normal end `drop(run)` exactly where they
cleared the slot before (so `TurnFinished` is still sent after the slot is free), and a panicking turn's task drops
its guard while tokio drops the future, before its `JoinHandle` resolves (the panic guard no longer resets
anything; `a_panicking_turn_task_frees_the_slot_before_its_join_handle_resolves`). The review's slot is freed by
`ReleaseReview` at its end; the queue / route / images are taken by commands (`TakeNext`, `TakePendingRoute`,
`TakeImages`), never reset in place.

**The review's pause without the `running` lock.** Before, the paused review copied the conversation under the
`running` lock, which `begin_turn` held while claiming. Now: `IdleEpoch` (no run: the number of claims so far),
copy the conversation, `ConfirmIdle { epoch }` (still no run and no claim since: set `started`, use the copy; else
throw it away and wait). `started` is set only inside the actor, so `ClaimReview`'s "has the old review started?"
and its cancel are one step with respect to it, which is what holding `running` in `ReviewSetup::start` was for.

**Errors, never hangs.** `Gone::Stopped` when the actor's thread has ended (send fails) or a handler panicked
(its reply sender is dropped with the command; caught by `catch_unwind`, logged, the actor goes on with the next
command; handlers change the state only in their last step). `Gone::Reentrant` when called from an actor's own
thread (with a debug assertion). Callers map `Gone` to an error (`anyhow` in the runtime: "the session's state
stopped answering ...", RPC `SERVER_ERROR` for `image.attach*`) or a safe default (a read: not running; the
scheduler: treat as reviewed; a review: stop). Unchanged RPC shapes and codes on every normal path.

**Lifecycle.**
- *Create*: `Runtime::handle(id)` (`SessionRegistry::get_or_spawn`) starts the actor on first use, with no store
  or file work (an effort or an image may be set for a session never loaded, as before). Reads (`get`) never start
  one (the archive sweep asks `is_running` of 100,000 sessions).
- *Resume / open*: `Runtime::open` loads from the store outside the actor, then `Attach` (two racing loads keep
  the first, as `entry().or_insert` did).
- *Close* (ACP `session/close`) and *evict* (workspace move): `Detach` (evict: `only_idle`, one step); the actor
  and the page's state stay (efforts, revisions, a queue survive as the gateway maps did).
- *Delete*: `close_quietly` (cancel, `Detach`, the tools' shutdown), then the handle is removed from the registry
  (its page state goes with it), then the store delete (a waiting write, after the turn's queued writes, CP13b).
  The actor ends when the last handle is dropped: a turn still ending holds one through its `RunGuard`, a review
  through its job, so their releases are still handled; a later use of the id starts a fresh actor.
- *Archive*: no change (a side file in the gateway; the actor is not involved).
- *Shutdown*: `Runtime` drop drops the registry; each actor ends once the last turn task / review lets go. The
  store's writer is shut down by `main.rs` / ACP as before (the actor holds no store work, so there is no order to
  keep between them). No thread is joined (a join from async code would block).

**Order.** One channel per session, one consumer: commands from one sender are handled in the order sent, and a
caller that waited for an answer is ordered before anything it does next. At a turn's end the order is as before:
release (inside the runtime, before `TurnFinished`), then the gateway's `message.complete`, `end_live` if nothing is
queued, the title, `apply_pending_route` (`TakePendingRoute`), `run_queued` (`TakeNext`). The queue keeps back / front
order (`the_queue_keeps_back_and_front_order_and_takes_by_admission`).

### Deadlock and re-entrancy rules (enforced where they can be)
1. **A handler never waits.** No IO, no `.await`, no `SessionHandle` call (any session's), no lock a caller may
   hold while waiting. Enforced: handlers are plain `fn`s on `SessionState` (no `.await` possible); a handle call
   from an actor thread returns `Gone::Reentrant` (thread-local flag) and trips a debug assertion
   (`a_call_from_the_actors_own_thread_is_refused_not_deadlocked`); the `TurnLock` file work lives in `RunGuard`,
   outside the actor.
2. **Lock order: actor, then the scheduler's `states`.** `ClaimReview` runs `Scheduler::is_busy` (takes
   `states`) inside the handler. So nobody may wait for an actor while holding `states`: checked by reading every
   `states.lock()` in `review_schedule.rs` (`turn_started`, `forget`, `shutdown`, `status`, `is_busy`, `nudge_due`,
   `arm`, `claim`, `fire`, `finished`, `Claim::drop`): none calls a handle while holding it; `Shared::claim` asks
   `ReviewSlotTaken` after its `states` block ends. No other outside lock is taken by a handler.
3. **No wait for a task from a handler, and no wait for a task-actor from a one-thread runtime**: the actor is a
   thread, so a synchronous `ask` from the only thread of a `current_thread` runtime is answered
   (`a_waiting_call_on_a_one_thread_runtime_does_not_deadlock`).
4. **A held lock while asking is allowed only if no handler takes it**: the conversation and record locks are
   never taken by a handler (holding them while asking would be safe; no code does: `ReviewSetup::after_turn`
   copies the turn's messages and releases the lock before `FinishTurn`).
5. **`RunGuard::drop` asks (waits)**: never drop one on an actor thread (none is ever stored in the actor).

### No lost command
- One `mpsc` channel per session, one consumer: every command sent is handled in send order, or answered with an
  error. A send to a stopped actor fails at once (`Gone::Stopped`); a command still queued when the thread ends is
  dropped with its reply sender, so its waiter wakes with `Gone::Stopped`
  (`commands_queued_when_the_thread_ends_are_answered_with_an_error`); a panicking handler's caller gets
  `Gone::Stopped` and the next command is handled (`a_panicking_handler_answers_an_error_and_the_actor_goes_on`).
- A session that is closing: `delete_session` cancels, detaches, then removes the handle from the registry; a turn
  or review still ending holds its own handle clone, so its `Release` / `ReleaseReview` are still handled before
  the thread ends (`the_actor_ends_when_the_last_handle_is_dropped`). A command to a removed id simply starts a
  fresh actor (an effort set on a deleted id goes to a new, empty state, as the old maps would have kept it
  forever).
- The race fixed on the way: `submit` read `is_running` and enqueued in two steps; a turn ending in between left
  the prompt queued with nobody to run it (until the next turn's end). Now `enqueue(.., only_if_running)` is one
  step; if the turn has ended, the prompt starts at once
  (`a_prompt_is_queued_while_running_only_if_a_turn_still_holds_the_session`). Same for a model picked mid-turn
  (`set_pending_route`): a turn ending in between gets the switch now instead of a pick nobody applies until
  the next turn's end. Shapes and the normal order are unchanged.

### Stages (each left the tree building and green)
Each stage moved one map / one group of coupled state. Each stage's incremental diff is kept outside the repo
(`C:\Users\alexandros\AppData\Local\Temp\claude\27b2cb05ce638374\c52af161-c1c3-40cf-bcd0-adf29c3f76ef\scratchpad\stages\N-*.patch`,
each against the one before), so the reviewer can commit them one by one. The docs, `queue-check.mjs` and two
clippy / rustfmt touch-ups after stage 9 (`enqueue_as` wrapped, a redundant `handle.clone()` in `begin_turn`) are
not in any stage patch.

| # | Moved | Tests after it |
|---|---|---|
| 1 | R1 `Runtime.sessions` -> `SessionRegistry` + the actor (`Attach` / `Detach`); `OpenSession::new` for every fork; `evict` / `close` through `Detach` | runtime lib 225, turns 258/7 ign (default), desktop 183 + acp 68 (acp: `retry_sends_the_last_prompt_again_without_the_old_answer` failed 2 of 7 full runs, see doubts) |
| 2 | O4 `running` + O5 `turn_state` + O6 `review` (one group: the review's pause and `ReviewSetup::start` held running -> review together); `RunGuard` becomes the RAII claim; the 8 hand resets removed; Stop on the published token; the scheduler asks `ReviewSlotTaken` | runtime lib 234, turns 258/7, desktop 183 + 68 |
| 3 | O7 `nudges` (`TickMemory`, `FinishTurn`, seeded by `Attach`) | runtime lib 235, turns 258/7, desktop 183 + 68 |
| 4 | G1 `queued` (`Enqueue` with `only_if_running`, `Dequeue`, `TakeNext`) | runtime lib 238, turns 258/7, desktop 183 + 68 |
| 5 | G2 `live_turns` (`SetLive`; `is_live` from one status) | runtime lib 238, desktop 183 + 68 (turns not re-run: no runtime path changed, only an added command) |
| 6 | G3 `pending_routes` (`SetPendingRoute` only while running, `TakePendingRoute`) | same as 5 |
| 7 | G4 `efforts` (`SetEffort`) | same as 5 |
| 8 | G5 `revisions` (`BumpRevision`) | same as 5 |
| 9 | G6 `pending_images` (`AttachImage`, `DetachImage`, `TakeImages`) + module doc | runtime lib 242, turns 258/7, desktop 183 + 68 |

### Tests (final tree, CARGO_TARGET_DIR=target/claude)
- `hermes_runtime --lib` **242** (213 + 29 actor tests in `session_actor/tests.rs`).
- `turns` **258 passed / 7 ignored under the default, `HERMES_TEST_PROFILE=requests` and `tokens`** (unchanged).
- `hermes-desktop` 183 + acp 68. `hermes_store` 34, `hermes_llm` 489 (+7 ign) + 5, `hermes_agent` 45, `hermes_memory`
  101, `hermes_prompts` 37: green. `hermes_tools` 336 / 1 failed: `script::tests::stopping_the_turn_kills_the_script_and_removes_its_scratch_folder`
  (asserts that PowerShell printed "begun" within the 1.5 s before Stop; on this machine it now fails alone too,
  3 of 3). The crate is unchanged by CP13d (`git diff HEAD -- crates/tools` is empty) and doesn't depend on the
  runtime: an environment timing failure (slow PowerShell start), not weakened, flagged here.
- `cargo build --workspace` OK (GPUI app builds; placeholder `target/hermes-src/apps/desktop/dist-webapp`, removed
  afterwards). `cargo clippy --no-deps -p hermes_runtime`: nothing in the CP13d files (fixed 3 redundant clones of
  mine); the crate still has older clippy errors (`signals.rs`, `mcp.rs`, `changes.rs`...), so the desktop crate's
  clippy pass is blocked by them, as before. `rustfmt` run on `session_actor.rs` (+ tests); my hunks elsewhere
  are formatted by hand (those files have older unformatted code).
- Live, own instance (debug exe, port 47810, `target/perf`; main checkout's `tools/node/node.exe` run in place; the
  instance started with `perf.ps1`'s environment without its timing step; working folders in my scratch folder;
  stopped by its PID): with `mock_llm.py` (started and stopped by its PIDs) `smoke.mjs` (all passed),
  `stop-check.mjs` (both cases ok), `rollback-check.mjs` (restore ok); as their own provider **`queue-check.mjs`
  (new)**, `side-and-branch-check.mjs`, `archive-check.mjs`, `workspace-check.mjs`: all passed;
  `attach-check.mjs` all passed on a fresh instance (after other checks it fails "model does not support vision",
  known since CP13c); `subagents-check.mjs`: all passed except `delegation.status`, which expects
  `max_concurrent_children == 10` (the `balanced` value) while the instance runs the default `requests` (3):
  stale since Phase 3, not CP13d. No perf / timing checks (user's rule).
- There was no live check for steering, queued prompts, approvals or clarify (scripts/perf/README.md has none), so
  `scripts/perf/queue-check.mjs` is new: a held turn with three prompts queued, `prompt.cancel` (twice),
  `prompt.replace_queued`, `session.steer`, `session.resume`'s `pending_submissions`, release -> the missed steer
  then the queue run in order with `running: false` only after the last; `session.redirect` (interrupted, then the
  redirect); an approval card under manual approvals (`approval.pending`, `approval.respond` deny, `settled`);
  a clarify card answered; idle-session answers of `prompt.steer_queued` / `session.steer`.

### Deviations from the plan
- **A thread per session, not a tokio task** (reason in the design: synchronous claimers on one-thread runtimes).
- **`record`, `conversation`, `context` stay on `OpenSession`** (data, not control; the per-chunk path and CP13b's
  queue-under-the-lock rule). So "replace OpenSession's ~7 mutexes" is 4 of 7 (running, turn_state, review,
  nudges) plus the registry.
- **Gateway maps**: the 6 session-keyed ones moved (queued, live_turns, pending_routes, efforts, revisions,
  pending_images); the plan's "~14" counted every map; the others are not per session or are file-backed for all
  sessions (generation), kept with reasons.
- Two check-then-act races closed on the way (a prompt queued after its turn ended, a model pick kept after its
  turn ended); shapes and the normal order unchanged.
- Stages 5-8 were gated without the `turns` suite (gateway-only moves plus an added runtime command each); stages
  1-4 and 9 ran the full gate.
- A live check script added (none existed for these paths).

### For the reviewer
- **No deadlock**: the rules above; check `review_schedule.rs` never asks an actor while holding `states`
  (`Shared::claim` asks `ReviewSlotTaken` after its block), and that no handler in `SessionState::handle` does
  IO or takes any lock but via `evidence_busy`. `RunGuard::drop` waits for the actor: it is dropped only in the
  turn / compress tasks and early returns, never on an actor thread.
- **Cancel still immediate**: `SessionHandle::cancel` reads the published token; `Runtime::cancel`,
  `close_quietly` use it; `cancel_reaches_the_turn_while_the_actor_is_held_up`; `stop-check.mjs`. The CP13c tool
  path is untouched.
- **Release before `TurnFinished`**: `drop(run)` sits where `*running = None` was (early-out after its flush,
  normal end after the flush and the final-answer read); the panic guard relies on tokio dropping the future
  before the `JoinHandle` resolves (`a_panicking_turn_task_frees_the_slot_before_its_join_handle_resolves`).
  `begin_turn` now claims before taking the `TurnLock` (as before, inside one locked section) and builds the turn
  state before the claim (steer can reach the turn a few lines earlier than before; harmless).
- **The review pause** (`IdleEpoch` / copy / `ConfirmIdle`) replaces "copy under the running lock": please check
  the argument in the design section, and `the_review_claim_and_the_schedulers_claim_always_see_each_other`.
- **`ReleaseReview` compares the `started` Arc** instead of "not cancelled -> clear": equivalent while replacing is
  the only canceller, stricter otherwise.
- **Lifecycle**: close / evict keep the actor (page state survives as the maps did); delete removes it. Actors
  accumulate per session used in a process (as `OpenSession`s did in the old map, since the page's
  `session.close` never closed the runtime's session): one parked thread with a 256 KiB stack each. Say if you want
  a cap or idle retirement (would need the page state moved out of the actor first).
- `Gone` mapping: runtime errors carry "the session's state stopped answering ..."; `image.attach*` answers
  `SERVER_ERROR` with it; reads fall back to defaults (not running, revision 1, no effort); the scheduler treats
  a gone actor as "reviewed". None of these happen on a normal path.

### Doubts
- `retry_sends_the_last_prompt_again_without_the_old_answer` (acp) failed in 2 of 7 full ACP runs after stage 1 and
  in none of the later full runs (stages 2-9, final: 9 runs). Stage 1 changed nothing on that path (it only swapped
  the map for the registry), so I believe it is an older timing flake, but I could not see the failing count and
  did not run HEAD to compare. Worth a look if it reappears.
- Thread per session (above).

**User checklist** (no visible change; a sanity pass in the rebuilt app):
1. Rebuild and start the app (`cargo build -p hermes-desktop`, your usual run).
2. Send a long request (ask it to run a slow command); while it works, send two more messages: they appear as
   queue cards. Delete one card, edit the other: the queue shows the edit last.
3. While it still works, use Steer with a short note. When the reply ends, the note runs first, then the queued
   message; the Stop button stays until the last one ends.
4. Start another long request and press Stop: it stops at once (a command's output so far is kept).
5. Pick another model while a reply streams: it shows at once and is used from the next message.
6. Change the effort in the composer, switch to another session and back: the pick is still there.
7. Negative case: Stop with nothing running: nothing happens, no error.

### Review (Opus, 2026-11-15)
Read the whole diff against 8e3d12d, the actor and its tests, and every caller of a handle; built the wait-for graph.

**Deadlock (wait-for graph).** Waiters on an actor: `RunGuard::claim` / `Drop` (start_turn, compress, the turn task,
early returns), `attach` / `detach` (open, evict, close, delete), `ClaimReview` (`ReviewSetup::start`, from the turn
task's end and `/refine`), `TickMemory` / `FinishTurn`, the gateway's page commands (all from RPC handlers or the
turn's event loop, sync), `Shared::claim`'s `ReviewSlotTaken` (scheduler), and the review's `ask_async`s. What a
handler waits on: nothing but `states` (inside `ClaimReview`'s `evidence_busy`) and the status `RwLock` (written
after the handler, never held while waiting). Every `states.lock()` in `review_schedule.rs` was read: none asks an
actor or blocks on anything that does (`fire` holds it across a `tokio::spawn` only; `Claim::drop` takes it after
`ReviewSlotTaken` returned). No caller holds `states` while asking; the conversation / record / Hub / store locks
are never taken by a handler, so holding them while asking cannot close a cycle; the store writer never waits for an
actor. (a) re-entrancy: no handler or closure run inside one calls a handle (the only closure is `is_busy`); (c) a
one-thread runtime: the replier is an OS thread and needs no tokio thread (`states` is never held across an
`.await`: parking_lot guards are `!Send`, and none is); (d) `RunGuard::drop` waits for `Release`, which only needs
the actor's thread; the guard is never owned by the actor, and its drop during a panic (tokio drops a panicking
task's future inside its guard, before the `JoinHandle` resolves) or an abort runs on a worker. No cycle found;
no deadlock test beyond the implementer's could be built that fails.

**Lost commands / races.** Every command answers or errors (`Gone`); one channel per session keeps a sender's order.
The two closed races are correct: an `Enqueue{only_if_running}` that succeeds is handled before the turn's
`Release`, which comes before `TurnFinished`, before the gateway's `has_queued` / `TakeNext`, so a queued prompt is
always run, and a refused one is started by its own `submit` (never both). Found and fixed:
1. **A model picked just after a turn ended could be overridden by the pick made during it.** Turn running, pick B
   (kept); the turn releases; pick C before the turn's end ran `TakePendingRoute`: C was switched now, then the
   turn's end applied B (the last pick lost; the same race existed with the old map). `SetPendingRoute` refused
   (no run) now also drops a kept pick, and the gateway asks the actor whenever one exists (not only when
   `is_running` said so, which left the stale pick in place on that path).
   Test `a_model_pick_after_the_turn_ended_drops_the_pick_it_left_behind`.
2. **A dead actor made its session unusable until restart.** The registry kept a handle whose loop had died
   (a panic outside a handler: e.g. a `Drop` while delivering an answer to a dropped async caller), so every later
   call answered `Gone::Stopped` ("reopen the session", which reopened the same dead actor), its last status said
   "running" forever (curator `anything_running` true, `is_running` true), and Stop could no longer reach its turn
   once replaced. Now: an `alive` flag cleared when the loop ends (normal or unwinding); `SessionRegistry::get`,
   `get_or_spawn` and `all` skip / replace a dead actor and cancel its last published turn; delivering an answer is
   inside `catch_unwind` too. Test `a_dead_actor_is_replaced_and_its_published_turn_stopped` (a test-only
   `Command::Exit` ends the loop).
3. **The losing copy of a racing load was dropped on the actor's thread** (its `SessionTools`: the "a handler does
   no work" rule). `Attach` hands it back; the caller drops it. Test
   `a_losing_load_gets_its_copy_back_to_drop_on_its_own_thread`.
4. **`Runtime::open` started an actor before the store load**, so asking for an unknown id (a stale page asking
   `/api/sessions/<deleted>/messages`) left a parked thread per id. The actor now starts after the load succeeds.

Not changed (noted): the panic guard's flush now runs after the slot is free (the guard was dropped with the task),
so a turn started in that instant could have its queued-write failures reported to the crashed turn's flush:
harmless. `steer` can reach the turn state a few statements earlier than before (published with the claim); if
`start_turn` then fails before spawning, that steer is lost while `steer` said `true` (window: the synchronous
part of `start_turn`; the page steers only a turn it was told runs). Close (`session/close`) while a turn ends now
refuses a new turn with "a turn is already running" until the old one releases, where before the map entry was
gone at once and the new turn failed on the cross-process `TurnLock` instead: both refuse, the text differs.

**Cancel.** `SessionHandle::cancel` reads the published token; `Runtime::cancel`, `close_quietly`,
`session.interrupt` / `redirect` use it. `cancel_reaches_the_turn_while_the_actor_is_held_up` is meaningful: with
a `Command::Cancel` through `ask` it would block 10 s behind the held actor (the 1 s bound fails), through `tell` the
token would not be cancelled. The tool path is unchanged (`git diff HEAD -- crates/tools crates/agent` was empty
before this review), so CP13c's kept-output property holds; `stop-check.mjs` re-run below.

**Behaviour parity.** The eight hand resets map one to one onto `drop(run)` / `?` (the early-out and the normal end
drop at the same statements, before `TurnFinished`; compress after `after_save`, before `TurnFinished`; the panic
guard above). RPC shapes, codes and texts unchanged on every normal path (`image.attach*` answers `SERVER_ERROR`
only on `Gone`). ACP unchanged (no file in `acp/` touched). GPUI app: API unchanged, builds. Schema untouched.

**Resources.** A thread per session opened (or given page state) in a process, never retired. Before CP13d each
such session already stayed loaded for the process (the map was never pruned by the page's `session.close`), with
its conversation and tools; a parked thread adds a kernel object and a few committed stack pages (the 256 KiB is
reserved address space). Every path that starts one is a user opening or acting on one session (resume, its
messages, a submit, an effort / image / revision); lists, search, the archive sweep and insights only read. With
the unknown-id leak fixed, hundreds of threads after a long day is the realistic bound: acceptable. Retiring idle
actors is not cheap-safe now (the page state would be lost or must move out, and a retire racing a
`get_or_spawn` or a clone held elsewhere makes two actors per id); not added. If it is ever needed: retire only
actors with no `open`, no run, no review and a default `PageState`, under the registry lock, checking
`Arc::strong_count(inner) == 1`.

**Failing test.** `script::tests::stopping_the_turn_kills_the_script_and_removes_its_scratch_folder` fails on HEAD
(8e3d12d, a throwaway worktree, 1 of 1) as on the CP13d tree (3 of 3): environment timing (PowerShell now takes
over 1.5 s to print its first line here), not a regression. Made robust without weakening it: the script writes
`begun.txt` after its line, Stop is sent once that exists (deadline 90 s), and it must end the 120 s sleep within
30 s with the output kept and the scratch folder gone. 3 of 3 green alone, and in the full crate run. PITFALLS
notes the rule.

**ACP flakes.** 15 full ACP runs on the reviewed tree (alone on the machine): 13 green, one
`retry_sends_the_last_prompt_again_without_the_old_answer`, one `cancel_ends_the_turn_with_cancelled`. On HEAD
(8e3d12d, throwaway worktree `hermes-rs-p13-head`, removed): `retry_...` failed in 2 of 4 runs (`left: 3, right:
2`), so it is older than CP13d; stopped after 4 runs, the cause being found. Alone, 30 of 30 green. Cause (probed):
`/retry` is an undone turn, a learning signal, so the `balanced` profile's `nudges` starts a background review
after the retried turn; its request (same tools and system prompt as a turn, the review prompt last) sometimes
reached the mock before the test read the request list. The test now counts the turns' requests only (not the
review prompt's) and prints the agent's reply on failure. Found on the way, **not fixed (outside CP13d)**: the
desktop app never applies `background_review: false` from `config.json` (only the GPUI app calls
`Runtime::set_background_review`), so the ACP tests and the perf instance review after turns whatever the config
says. `cancel_...` cancelled after a fixed 800 ms: under load the turn had not claimed the session yet, so Stop
found nothing and the turn ran its 20 s. It now waits for the turn's request to reach the model, then requires
`cancelled` within 10 s. Then: 3 more full ACP runs green.

**Live checks** (own instance: the debug exe of this tree, port 47810, fresh `target/perf`, started with perf.ps1's
environment without its timing step; main checkout's `tools/node/node.exe`; working folders in the scratchpad;
`mock_llm.py` and the instance stopped by their PIDs): with `mock_llm.py`: `smoke.mjs` all passed, `stop-check.mjs`
both cases interrupted with `running: false`, `rollback-check.mjs` restored; as their own provider:
`queue-check.mjs` all passed (queue, cancel, replace, steer, order, `running: false` after the last, redirect,
approval, clarify), `side-and-branch-check.mjs`, `archive-check.mjs`, `workspace-check.mjs`, `review-check.mjs`
(needs the home folder as its second argument when run from outside the repo), `staging-check.mjs`,
`subagents-check.mjs` (now reads the effective width: `requests` 3) all passed.

**Tests (final tree).** `hermes_runtime --lib` 245 (242 + 3 review tests); `turns` 258 passed / 7 ignored under
default, `requests` and `tokens`; `hermes-desktop` 183 + acp 68 (3 runs); `hermes_tools` 337 (the script test
green), `hermes_store` 34, `hermes_llm` 489 (+7 ign) + 5, `hermes_agent` 45, `hermes_memory` 101, `hermes_prompts`
37; `cargo build --workspace` OK (placeholder `target/hermes-src`, removed).

**Commit.** One commit for CP13d (the stage patches each build, but the review fixes cut across stages 1, 6 and the
module; one commit listing the stages is the honest history).

## CP13e — explicit agent loop
Worktree: C:\Users\alexandros\repos\hermes-rs-p13   Branch: backend/p13-perf   Model: Opus 5.5 (subagent)
On top of acad3ca (CP13a-d). Not committed (an Opus reviewer reviews and commits). Phase 13 is not done (13f remains).

### Inventory (before CP13e)
Goose's `StateMachine` (`crates/agent/src/machine.rs`) ran a `Vec<Step>` of `Operation` / `Inference` trait objects. Four
users built one: the main turn (`lib.rs` `begin_turn`), a `delegate_task` child (`delegate.rs`), the agentic review
(`review.rs`), the curator's agent loop (`curator.rs`). Nothing outside `hermes_runtime` used `machine.rs` (the desktop
crate, ACP and the GPUI app use only `hermes_agent::events::AgentEvent`; `hermes_tools` only `Emitter`).

**Steps, in order** (S = only where there is `/steer` and compression: the main turn and a child; the review and the
curator have neither):

| # | Step (`name()`) | Reads | Acts when -> result | Events |
|---|---|---|---|---|
| 0 | `FrozenPrompt` ("prompt") | `session.system_prompt` | never acts; contributes `("system", prompt)` to every request | — |
| 1 | `IterationBudget` | `TurnState.summary_requested/summary_retried`, `assistant_turn_count` since kickoff | count >= `max_iterations`, the model next, the turn not ended: summary nudge (continue). During the summary: text -> yield; empty -> hidden once (continue), then the canned "I ran out of steps" (yield) | `Message` per new message |
| 2 | `ValidateToolCalls` | the last agent-visible reply with tool calls, once per id (`TurnState.validated`) | output limit hit: hidden + `max_tokens` boosted (continue) up to `truncated_tool_retries`, then hidden + `TRUNCATED_MESSAGE` (yield). Duplicate call ids made unique, invented names repaired (`UpdateMessage`). An all-invalid batch: a strike (continue, possibly no effects) up to `invalid_tool_retries`, then error answers + "Model generated invalid tool call: X" (yield). Malformed JSON cut in transit: hidden + `TRUNCATED_UNREPORTED_MESSAGE` (yield); else hidden (continue) twice, the third (`MAX_INVALID_JSON_RETRIES` 3) gets error answers (continue) | `Message`; `HistoryReplaced` when an `UpdateMessage` is applied |
| 3 | `ToolOperation` ("tools") | unanswered, agent-visible tool requests since kickoff | any with an offered tool: run concurrently (`join_all`), one message of responses (continue) | the tools' own (`Status` taint, `ToolProgress`, `Subagent*`), then `Message` |
| 4 | `UnhandledToolCalls` | the same, after tools | a request nothing answered (invented, disabled, deferred not loaded): error answers (continue); awaits `DeferredTools::unavailable_reply` | `Message` |
| 5 | `DeliverSteer` (S) | last message a tool response, `TurnState.pending_steer` | a steer waits: the user's text (user-only) + the marked nudge (agent-only) (continue) | 2 x `Message` |
| 6 | `LengthContinuation` | the last reply cut by the output limit, no tool calls, not in `length_trail` | repetition: hidden + `REPETITION_MESSAGE` (yield); nudge (continue) while n < `length_continuations`; at the ceiling the trail hidden and stitched into one answer or `CEILING_NO_TEXT` (yield) | `Message`, `HistoryReplaced` |
| 7 | `EmptyResponseRecovery` | the last message since kickoff: the inference empty marker, or an agent-visible empty reply | text beside housekeeping-only calls: hidden (yield); after tools, once (`post_tool_nudged`): "(empty)" placeholder + nudge (continue); thinking only: nudge (continue), `MAX_THINKING_PREFILLS` 2; else the marker hidden + `EMPTY_TERMINAL_MESSAGE` (yield) | `Message`, `HistoryReplaced` |
| 8 | `ExitOnError` | the last message an error | yield | — |
| 9 | `AutoCompress` (S) | model next, no summary under way, no failed summary this turn, `compacted_at`, the estimate vs the (fallback-bound) threshold | over it: the summary (awaits `compact`) -> `Compacted` (continue); nothing to summarize: `compacted_at` set, not applicable; error: `compression_failed`, not applicable | `Compacting` first, `CompactionFailed(None/Some)`; on apply `HistoryReplaced` + `Compacted` |
| 10 | `InferenceRunner` ("llm") | `prepare_session` (identity), `applies` (no trailing error and the model next), the tools (`ToolOperation::inference_tools` -> `GuardedTools::tools`: none during the summary), `max_tokens_override()` | the model next: additional messages, usage, the streamed reply (continue). Inside: `empty_response_retries` re-asks of an empty reply, then the hidden marker; the fallback chain in `open_stream` (Phase 12); `Opened::Compress` -> empty `applied` (continue) | `Status{fallback}`, `Message` per chunk; `Usage` on apply |

**The loop** (`StateMachine::run`): per pass `runtime.load()` (a snapshot; `TurnState.max_tokens.take()`), `step()`,
the effects applied (`ensure_message_ids`, `TurnRuntime::apply_effects`); `Continuing` -> another pass,
`Yielded` / `Cancelled` -> `finalize`. `step()`: for each step, `is_cancelled()` -> `Cancelled` (no interrupted step);
else the step's future raced in `tokio::select! { biased; step, cancel.cancelled() }`; `Err` without Stop -> the run's
error (no finalize); `Err` after Stop, or Stop won -> `Cancelled` with that step remembered as interrupted;
`NotApplicable` -> the next step; `Applied` -> ids ensured, status `Cancelled` if Stop came meanwhile (its effects are
still applied). No step acted -> `Yielded`. `finalize`: load; nothing unless cancelled; the interrupted step's
`finalize_cancellation` (saved = applied + reloaded), then one "Tool call was interrupted before completing" answer
per unanswered tool request since kickoff (ids deduplicated; emitted as `Message`), then every other step's
`finalize_cancellation` in order. Only two steps keep leftovers: `InferenceRunner` (`take_output`: additional
messages, usage, the accumulated chunks) and `ToolOperation` (its `response` slot, in practice always empty: filled
and taken with no `.await` between).

**Cancellation points** (an `.await` Stop can drop): `UnhandledToolCalls` (`unavailable_reply` -> the deferred
wrapper's `tools()`), `ToolOperation` (every call; CP13c's `off_worker_unless_stopped` lets a stopped call finish in
the poll that sees Stop), `AutoCompress` (`compact`: the summary request; a dropped one leaves `TurnState` as it was,
`Compacting` already sent), `InferenceRunner` (`inference_tools`, `resume`, `open_stream` with the chain's `stream` +
peek, `fetch_model_info`, every `stream.next()`; a dropped one leaves its output for `finalize`). The other steps
never wait (their `async fn`s had no `.await`), so they always finished in the first poll. `load` and `apply` are not
raced.

**Limits and their log lines** (unchanged): `budget.max_iterations` (main turn), `REVIEW_MAX_ITERATIONS` 16,
`budget.delegate_child_iterations`, `budget.curator_max_iterations`; `budget.truncated_tool_retries` ("truncated tool
call — retrying (n/N)", cap `boosted_output_cap`); `budget.invalid_tool_retries`; `MAX_INVALID_JSON_RETRIES` 3
("invalid JSON in tool call arguments — retrying"); `budget.length_continuations` ("requesting continuation (n/N)");
`MAX_THINKING_PREFILLS` 2; one post-tool nudge; `budget.empty_response_retries` inside the runner ("Provider returned an
empty response; retrying (n/N)"); `budget.http_retries` in the provider; the fallback chain. Values per profile:
`budget.rs`.

**Metadata notes.** Producers of `metadata.operations` notes: `inference.empty_response` (the runner's hidden empty
marker; read by `EmptyResponseRecovery` via `is_empty_response_marker`, nowhere else), `preinject.*`,
`compress.{handoff,copy,notice,hidden}`, `command.command` (`COMMAND_NOTE`), `references.typed`,
`foreground_subagent.{delivered,cancelled}` (Goose's `SubagentOperation`, unused). Only the first is loop plumbing; all
are stored with their message in `sessions.db`, and the others are read by search, transcripts, compaction and the
GPUI app. `Operation::set_message_meta` / `message_meta` and `ConversationEffect::SetMessageOperationNote` had no
producer. No retry counter was ever kept in metadata (they are `TurnState` fields), so the plan's "instead of string
notes in message metadata" means the step-result plumbing; nothing stored changes.

### Design
`crates/runtime/src/agent_loop.rs` (tests in `agent_loop_tests.rs`):
- `pub enum Next { Continue, Retry(Reason), Done }`; `pub enum Reason { IterationLimit, EmptySummary,
  TruncatedToolCall{attempt}, InvalidToolName{strike}, InvalidJson{attempt}, InvalidJsonAnswered,
  LengthContinuation{attempt}, EmptyAfterTools, ThinkingOnly{attempt} }`; `pub struct Outcome { effects, next }`;
  `pub type Checked = Result<Option<Outcome>>` (`None` = Goose's `NotApplicable`); helpers `proceed` / `retry` / `done`.
  Old to new: `applied` -> `Continue` or `Retry(reason)`; `yielded` / `yielded_with` -> `Done`. `Continue` and `Retry`
  both start another pass (`Retry` names why; logged with the step at debug).
- `pub struct AgentLoop { budget, validate, tools: ToolOperation, unhandled, steer: Option, length, empty,
  compress: Option, inference: InferenceRunner, cancel }`: concrete fields instead of `Vec<Arc<dyn ...>>`.
- `run` / `pass` / `check` / `finalize` are the old `run` / `step` / the step bodies / `finalize`: same snapshot per
  pass, same order (`PASS`; `FrozenPrompt` dropped, it only supplied the system prompt, now built in
  `inference_input`), same `is_cancelled` check and `biased` race per check, the interrupted check returned from `pass`
  (was a `Mutex` on the machine), same finalize order (only tools and inference have leftovers).
- `ops.rs` / `compress.rs`: each check is an inherent `check` method (sync where it never awaited: `IterationBudget`,
  `ValidateToolCalls`, `DeliverSteer`, `LengthContinuation`, `EmptyResponseRecovery`; async: `UnhandledToolCalls`,
  `AutoCompress`); `ExitOnError` became the fn `exit_on_error`. Bodies unchanged apart from their return values. A sync
  check called inside the raced `async` block behaves as before: an `async fn` without `.await` finished in its first
  poll.
- `TurnRuntime::load` / `apply_effects` are inherent methods (were `SessionLoader` / `EffectHandler` impls whose bodies
  never awaited; the unused `session_id` / `&TurnSession` parameters are gone).
- Agent crate: `machine.rs` deleted (`StateMachine`, `Step`, `SessionLoader`, `EffectHandler`, `EffectUsage`, and its one
  test `rejects_duplicate_tools_across_operations`: with one tool step nothing can collide across steps;
  `ToolOperation::available_tools` still refuses two providers with one name). `MachineSession` moved to `operation.rs`.
  `ToolOperation`, `InferenceRunner`, `SubagentOperation` and the `Operation` / `Inference` traits stay (Goose code,
  used; Phase 14 trims).
- Why the plan's "prepare -> infer -> validate -> run tools -> post-checks -> maybe compress" is not one loop
  iteration: every pass re-reads the conversation and starts at the top (after tools the budget is asked before the
  next request; a repaired call is validated against a new snapshot; compression is judged right before the request).
  A fall-through cycle would change which check sees which snapshot, and so the requests. The module doc maps the
  plan's words onto the pass.

### Golden trace (before / after, byte for byte)
Method: a throw-away patch (not kept; `git apply -R` restored the three files) added `hermes_llm::golden::record`,
which, with `HERMES_GOLDEN_DIR` set, appends one line per event to `<dir>/<thread name>.log`: `R <purpose> <request
JSON>` in `ApiRequestBuilder::response_post` (every HTTP attempt, the exact body sent), `E <AgentEvent:?>` in
`Emitter::emit` (all but `ToolProgress`, which is timing), `S <op>` in `Store::queue` (upsert with seq and the
message's stored JSON, replace, usage, prompt). Every `turns` test is `#[tokio::test]` (`current_thread`), so its
spawned tasks run on the test's own thread and log under the test's name. The suite was run with the patch at
acad3ca (HEAD: the default profile twice, `requests`, `tokens`) and on this tree (the three profiles), then compared
per test and per kind as sequences after normalising what differs between any two runs: uuids, `resp_N`, session ids
and other hex, `created` stamps, temp folder names, ports, durations (`duration_ms`, `duration_seconds`, "N s"),
clock times in session search, subagent / delegation ids. Noise floor: HEAD default run 1 vs run 2 differ in nothing
after that normalisation (with a weaker first normaliser, 25 tests differed, all in durations and random ids).

Result: **no difference** in any test, any kind, any profile.

| Profile | Requests | Agent events | Queued store writes | Tests differing |
|---|---|---|---|---|
| default (`balanced`) | 987 = 987 | 2129 = 2129 | 2572 = 2572 | 0 |
| `requests` | 987 = 987 | 2129 = 2129 | 2572 = 2572 | 0 |
| `tokens` | 987 = 987 | 2129 = 2129 | 2572 = 2572 | 0 |

(244 tests log something; the other 14 make no request, event or queued write. The counts are equal across profiles
because the harness pins `max_iterations` and the full tool list, and a test that depends on a profile names it.)
The patch and the comparer are in the implementer's scratchpad (`golden/instrument.patch`, `golden/compare.py`), not
in the repository; TESTING.md describes how to redo it.

### Tests (final tree, instrumentation removed, CARGO_TARGET_DIR=target/claude)
- `turns` **258 passed / 7 ignored under the default, `HERMES_TEST_PROFILE=requests` and `tokens`** (file unchanged;
  also green with the instrumentation, 6 more runs).
- `hermes_runtime --lib` **259** (245 + 14 new in `agent_loop_tests.rs`: the truncated-tool-call retry up to
  `truncated_tool_retries` with the cap growing as `boosted_output_cap` and the count restarting after the refusal,
  under each profile's knobs; a reply validated once; invented names striking out at `invalid_tool_retries` (a batch
  with a real call not striking); malformed JSON withdrawn twice then answered, then counting from 1 again; arguments
  cut in transit ending at once; a cut-off answer continued up to `length_continuations` then stitched; repetition
  not continued; the empty-after-tools nudge once; thinking-only asked twice; text beside housekeeping tools; the
  iteration budget's summary, its one retry and the canned text; a provider error; the agent-crate mapping; Stop's
  interrupted answers deduplicated).
- `hermes_agent` **44** (45 minus `rejects_duplicate_tools_across_operations`, deleted with `machine.rs`), `hermes_llm`
  489 + 5, `hermes_store` 34, `hermes_tools` 337, `hermes_memory` 101, `hermes_prompts` 37, `hermes-desktop` 183 +
  acp 68; `cargo build --workspace` OK, no warning (placeholder `target/hermes-src`, removed). No perf or timing
  check (the user's rule).

### Live checks
Own instance: the debug exe of this tree, port 47810, a fresh `target/perf/home`, started with `perf.ps1`'s
environment without its timing step; the main checkout's `tools/node/node.exe` run in place; working folders in my
scratchpad (outside any repository). With `mock_llm.py` (started by me, stopped by its PID's tree): `smoke.mjs` all
checks passed, `rollback-check.mjs` (restored, the file gone), `stop-check.mjs` (stalled reply and slow command both
interrupted, `running: false`). As their own provider: `queue-check.mjs`, `provider-error-check.mjs`,
`fallback-check.mjs` all passed; the instance was restarted after it (fallback-check leaves `mock`'s quota marked
used up in memory); then `side-and-branch-check.mjs`, `coding-commands-check.mjs` (the `/review` subagent: a child
loop), `review-check.mjs` (the evidence review; given the home folder), `staging-check.mjs`, `subagents-check.mjs`
(a child's steer after its tool results, stop) all passed. No panic, "turn crashed", "The request stopped" or "not
fully saved" line in the instance's logs. Only processes under `target\claude` and the mock I started were stopped.

### Deviations from the plan
- The loop iterates **passes** (one action each, from the top), not the plan's linear "prepare -> infer -> validate
  -> run tools -> post-checks -> maybe compress" per iteration; see Design for why. The order inside a pass is
  Goose's step order, unchanged.
- "Instead of string notes in message metadata": no retry or continuation state was in metadata; the one note the
  loop reads (the empty-response marker) is stored data and stays. What changed is the step-result plumbing
  (`OperationResult` / `RunStatus` -> `Option<Outcome>` / `Next`).
- `ToolOperation` and `InferenceRunner` keep Goose's `Operation` / `Inference` traits (the loop calls them through the
  traits and maps the result). Rewriting them as plain methods would touch the fallback chain and the tool runner for
  no behaviour gain; Phase 14 trims the agent crate.
- `Next::Retry` and `Continue` behave the same in the loop; `Reason` is for logs and tests.

### For the reviewer
- **Semantics to check against the deleted `machine.rs`** (`git show acad3ca:crates/agent/src/machine.rs`): `pass` =
  `step` (cancel check before each check; the `biased` race; `Err` after Stop is a Stop; an acting check's effects
  applied even when Stop came meanwhile; no check acting = `Done`); `run` = `run` (load per pass, `ensure_message_ids`
  in `pass` and before apply, as before; break on not-continuing); `finalize` = `finalize` (load even when not
  cancelled, the interrupted check first, the unanswered answers, the others; `save` reloads only when it applied
  something). Absent checks (`steer` / `compress` = `None`) are skipped before the cancel check, as absent steps were.
- `Some(Outcome)` with no effects (`Retry(InvalidToolName)` below the strike limit, the inference `Opened::Compress`
  path) must stay `Some`: it ends the pass. The unit test asserts the first; the second is `from_operation`.
- `inference_input` builds `InferenceInput` directly: tools from `ToolOperation::inference_tools`, the system prompt as
  the one prompt part (the identity preparer joins parts with "\n\n"; one part = the prompt as is), no moim parts. The
  duplicate-tool check across steps went with the steps (one tool step).
- Sync checks run inside the raced `async` block: same as the old `async fn`s without `.await` (finished at the first
  poll). The async ones (`UnhandledToolCalls`, `AutoCompress`, tools, inference) are the cancellation points listed in
  the inventory.
- `TurnRuntime::load` keeps taking `TurnState.max_tokens` on every load, including finalize's and `save`'s reloads.
- Docs: ARCHITECTURE (crates table, "A turn" step 3, "The agent loop (Phase 13e)", the fallback paragraph), PITFALLS
  (new section; the Stop bullet points at `agent_loop.rs`), TESTING (agent loop tests, the golden-trace method),
  PORTING (Goose divergence; the `compress.rs`, `ops.rs` row: plumbing diverged).
- `cargo fmt`: the new files are formatted; the touched runtime files have exactly as many rustfmt differences as at
  HEAD (none added).
- My slip: one shell command meant to loop over file names ran `git stash` in the worktree; I popped it at once
  (`git stash list` empty, every change back, the untracked new files were never stashed). The main checkout was not
  touched (`git status` there clean).

### Doubts
- The golden trace covers what `turns.rs` drives. Paths it does not reach are covered by reading only: a Stop during
  `UnhandledToolCalls`' deferred lookup, a check's error after Stop. Both go through the same `pass` code as every check.
- `Reason` granularity (`InvalidJsonAnswered` is a `Retry`; unanswered calls, steer and compaction are `Continue`) is a
  naming choice; nothing branches on it.

**User checklist** (no visible change; one sanity pass in the rebuilt app):
1. Rebuild and start the app. Send "list the files here": the reply streams, the tool card shows, the answer ends the
   turn as before.
2. Ask for something long and press Stop while a command runs: the turn stops at once, the tool card keeps the output
   it had.
3. Type a steer while a tool runs: it appears after the tool result and the model follows it.
4. In a long session run `/compress`, then send a message: the summary notice shows and the reply works.
5. Ask the agent to delegate a small task: the subagent row runs and its summary comes back.

### Review (Opus, 2026-11-15)
Reviewed against `git show acad3ca:crates/agent/src/machine.rs`, the old `ops.rs` / `compress.rs` operation impls and the
four step chains (`lib.rs`, `delegate.rs`, `review.rs`, `curator.rs`), line by line.

**1. Step order and conditions: same action in every state.** `PASS` is the old `Vec<Step>` minus `FrozenPrompt`, in
the same order for all four users (review and curator: `steer` / `compress` `None`, skipped before the cancel check
exactly as an absent step was). `FrozenPrompt` only had `prompt_parts`; no other step overrode `prompt_parts`,
`moim_parts` or `inference_tools` except `ToolOperation::inference_tools` (grep of every trait method at acad3ca), so
`inference_input` (tools from `ToolOperation`, one `("system", prompt)` part of the *prepared* session, no moim) is
what `collect_inference_input` built. Each check body is the old `run` with `not_applicable` -> `Ok(None)`,
`applied` -> `proceed` / `retry` (both `Continuing`), `yielded` / `yielded_with` -> `done` (`Yielded`); no condition,
counter or reset moved (the diff touches only return expressions plus one `let strike` read after the increment);
no constant changed (`MAX_INVALID_JSON_RETRIES` 3, `MAX_THINKING_PREFILLS` 2, `boosted_output_cap`, `budget.rs`
untouched, `REVIEW_MAX_ITERATIONS` 16). `from_operation` maps `Applied{Continuing}` -> `Continue`,
`Yielded`/`Cancelled` -> `Done`; a `Cancelled` status from a step ended the old run too, and `finalize` reads the token,
not the status, so it is the same. The inference `Opened::Compress` path (empty `applied`) stays `Some` -> another pass
-> `AutoCompress` reads `FallbackShared` (Phase 12) as before. The states `turns.rs` does not reach follow from the
same order: budget spent with calls pending (budget needs the model next, so tools run first, then the summary
nudge), steer with unhandled calls (unhandled answers first, the steer the pass after), length + tool call (length
skips replies with calls; validation's truncated path handles them), empty after tools vs thinking-only (one body,
unchanged), provider error + compress (exit-on-error comes first, as before). `TurnRuntime::load` takes
`max_tokens` once per load, called at the same points (each pass, finalize's load, each `save` reload): a pass that
acts on something else first consumes it, as before.

**2. Stop.** `pass` = `step`: `is_cancelled` before each present check, `biased` race, `Err` after Stop -> cancelled with
that check interrupted, `Err` without Stop -> the run's error with no finalize, an acting check's ids ensured and
effects applied even when Stop came meanwhile (`Cancelled(None)`); `finalize` = interrupted check's leftovers, the
deduplicated "interrupted" answers, then the others in `PASS` order (only tools and inference have
`finalize_cancellation`; the default for the others returned nothing, as did `AutoCompress` / `FrozenPrompt`).
The two paths verified only by reading now have tests (below), and mutating the `if !self.cancel.is_cancelled()`
guard fails one.

**3. Stored and visible state.** `inference.rs` changed only an import: the `inference.empty_response` note is produced
byte for byte as before. `ensure_message_ids` runs at the same two points, `apply_effects` is the same body (now sync;
the old `async fn` never awaited). The duplicate-tool guard: the machine's cross-step check could not fire any more
(one tool step, and `ToolOperation::available_tools` already bailed on a name offered twice by two providers or by one
provider, before the machine's check ran; `register` replaces a same-named registered tool). It stays the guard; it
had no test, so `tool::tests::a_tool_name_offered_twice_is_refused` now pins it (two providers, one provider twice).
MCP tools reach the turn through the one `GuardedTools` provider, so a colliding name fails the request with that
error instead of reaching the provider, as before.

**4. Consumers.** Nothing outside `hermes_runtime` used `machine.rs` (desktop, ACP and the GPUI app only use
`AgentEvent`; `cargo build --workspace` passes). Fallback: `InferenceRunner` / `ChainRun` / `FallbackShared` untouched;
`AutoCompress` unchanged apart from returns. Stale "state machine" / `agent/machine.rs` references fixed in
`AGENTS.md`, `README.md`, both crates' `Cargo.toml` descriptions, `hermes_tools` (`lib.rs`, `blocking_tests.rs`,
`shell.rs` comments) and PITFALLS (the `Arc<Message>` section). `PLAN.md` (history) and the Goose attribution in
`THIRD_PARTY_NOTICES.md` left as they are. PORTING rows say `diverged`.

**5. Passes.** Justified: a fall-through cycle would judge later checks on a stale snapshot and change requests.
Per-pass cost is what it was (one snapshot of `Arc` pointers per pass since CP13a, as the machine loaded per pass);
the trait objects and boxed `async_trait` futures of the sync checks are gone. Nothing measured (the user's rule).

**6. Tests.** `turns.rs` unchanged (`git diff` empty). The implementer's 14 check-level tests are meaningful: mutating
`MAX_THINKING_PREFILLS` 2 -> 3 or the truncated-retry `<` -> `<=` fails them. Added `whole_loop` (6 tests, running
`AgentLoop` over a real `TurnRuntime` with a counting test provider): Stop during `UnhandledToolCalls`' deferred lookup
(the call answered "interrupted", no request sent); an error after Stop is a Stop (answered); an error without Stop
ends the run with no finalize (call left unanswered); unanswered calls are answered a pass before a waiting steer
(swapping `Unhandled` / `Steer` in `PASS` fails it); a spent budget lets pending calls run, then one summary request
ends the turn; `load` takes the one-shot cap once. Mutating the error-after-Stop guard fails its test.

**Golden trace, redone independently** (`requests`): a throw-away `git worktree` at acad3ca with `instrument.patch`
applied, `turns` built and run (HEAD), then this tree's diff applied on top in the same throw-away copy, rebuilt and run
(new); worktree removed afterwards, nothing instrumented left in this tree. `compare.py`: 244 tests, requests 987 = 987,
events 2129 = 2129, store writes 2572 = 2572, no content difference; one test differs in event *order*
(`delegation_width_follows_the_profile`, parallel children): my HEAD run differs from the implementer's HEAD run in
the same way and my new run equals the implementer's HEAD run exactly, so it is the noise floor (concurrent subagents),
not the change.

**Results** (final tree, CARGO_TARGET_DIR=target/claude): `turns` 258 passed / 7 ignored under default, `requests` and
`tokens` (file unchanged); `hermes_runtime --lib` 265 (259 + 6 `whole_loop`); `hermes_agent` 45 (44 + the duplicate-tool
test); `hermes_llm` 489 + 5, `hermes_store` 34, `hermes_tools` 337, `hermes_memory` 101, `hermes_prompts` 37;
`hermes-desktop` lib 183, acp 68 three runs in a row; `cargo build --workspace` OK, no warning (placeholder
`target/hermes-src`, removed). Live, own instance (debug exe of this tree, port 47810, fresh `target/perf/home`, the
main checkout's `tools/node/node.exe` in place, working folders in my scratchpad): with `mock_llm.py` (started and
stopped by its PID): `smoke.mjs` all passed (in a folder with a file in it, as `scripts/perf/README.md` says: an empty
folder gets no first checkpoint), `stop-check.mjs` (stalled reply and slow command, both interrupted, `running:
false`), `rollback-check.mjs` (restored, file gone); as their own provider: `queue-check`, `provider-error-check`,
`fallback-check` all passed, instance restarted, then `side-and-branch-check`, `review-check`, `staging-check`,
`coding-commands-check` all passed; no panic / "turn crashed" / "The request stopped" / "not fully saved" line in the
restarted instance's logs. No perf or timing check (the user's rule). Phase 13 not done (13f follows).

## CP13f — typed gateway dispatch
Worktree: C:\Users\alexandros\repos\hermes-rs-p13   Branch: backend/p13-perf   Model: Sonnet 5.5 (subagent)
On top of a110767 (CP13a-e committed). Not committed, not reviewed. Phase 13 not done (the reviewer closes it).

Done: `Request` / `RpcError` / `Reply` (`gateway/rpc.rs`), `dispatch` as one `match` over the typed request, `gateway.rs` split into
`gateway/{mod,rpc,sessions,turn,slash,settings,projects,attachments,delegation,usage,rest}.rs` (+ `tests.rs`,
`rpc_table_tests.rs`), the table fixture, `rpc-dispatch-check.mjs`, the docs. **No behaviour change**: the answers of every
served method over ~25 params each (2,181 answers) are identical before, after the typed dispatch and after the split, and
802 live request / reply pairs are identical but for a measured tokens-per-second figure. Not committed.

### Inventory (before CP13f, `gateway.rs` at a110767)

How the old `dispatch(method, params, outbox)` read `params`: a loose `serde_json::Value`, `params` = `Null` when the
frame had none. Every read is `params["f"]` (a missing field, a non-object `params` and `null` all give `Null`),
then `.as_str()` / `.as_u64()` / `.as_i64()` / `.as_bool()` / `== true` / `.clone()`. A field of the wrong type reads as
absent (a number is never turned into a string, `"7"` is not 7, `1.5` is not an integer, `1` is not `true`), so **no
shape of `params` made a served method answer an error by itself**; the only errors are the ones each handler raises
on purpose (below). `dispatch` loaded `Settings` before the `match`, for every method (also an unknown one).

Errors: the three codes (`-32601` `Method not found: <name>`, `-32602`, `-32000`), as `(i64, String)`. `handle` added
`data: {"reason": "session_busy"}` when the message was exactly `session busy` (any code), logged a -32601 under
`hermes_desktop::missing` and every other error as a warning. `Gateway::call` (ACP) returned the tuple unchanged.

| Method(s) | Params as read | Errors it raises | New file |
|---|---|---|---|
| `gateway.ping`, `client.capabilities`, `onboarding.state`, `projects.discover_repos`, `projects.record_repos`, `session.close`, `pet.info`, `shared_metrics.status`, `free_tier.status`, `wake.status` | none | none | `mod.rs` / `settings.rs` |
| `setup.status`, `setup.runtime_check`, `profiles.list` | none (Settings) | none | `settings.rs` |
| `projects.tree`, `projects.project_sessions` | `preview_limit` (`as_u64`, default 3), `project_id` (`as_str`) | none | `projects.rs` |
| `projects.list` | none | none | `projects.rs` |
| `projects.create` | `name` (str, default "Project", trimmed), `folders` (array of str, non-strings skipped), `primary_path` (str, non-empty), `use` (`== true`) | -32000 store failure | `projects.rs` |
| `projects.update`, `projects.add_folder` | `id` (str, default ""), `name` (str, non-blank), `path` (str, non-empty), `is_primary` (`== true`) | -32000 `No project <id>`, store failure | `projects.rs` |
| `projects.delete` | `id` (str, default "") | -32000 store failure | `projects.rs` |
| `projects.set_active` | `id` (any value, kept as sent) | none | `projects.rs` |
| `session.active_list` | none | none | `sessions.rs` |
| `model.options` | `session_id` (str) | none | `settings.rs` |
| `requests.status` | `provider` (str) | -32602 `No provider is configured` | `usage.rs` |
| `staging.list` | none | none | `usage.rs` |
| `staging.apply`, `staging.reject` | `id` (str) | -32602 `id is required`; -32000 the failure's message | `usage.rs` |
| `config.get` | `key` (str) | none | `settings.rs` |
| `config.set` | `key` (`== "model"` picks the model path, else str, default ""), `value` (any), `session_id` (str); the model path reads the whole params | -32602 `key is required`, a bad `budget.*` value (the validator's text), `a model is required`; -32000 `No provider is configured`, `Unknown provider <id>`, store failures | `settings.rs` |
| `commands.catalog` | none | none | `slash.rs` |
| `complete.slash` | `text` (str) | none | `slash.rs` |
| `complete.path` | `word` (str), `cwd` (str, non-empty), `session_id` (str) | none | `slash.rs` |
| `slash.exec` | `command` (str; a command starting `goal` after the slashes answers an empty exec first), whole params for the session | -32000 `not a quick/plugin/bundle/skill command`; the builtins' and coding commands' own (-32602 `session_id is required`, `/init needs a session with a folder`, -32000 store text) | `slash.rs` |
| `command.dispatch` | `name`, `arg` (str, default ""), whole params | -32000 `not a quick/plugin/bundle/skill command`, the coding command's | `slash.rs` |
| `session.status` | `session_id` | -32602 `session_id is required`; -32000 `Session <id> not found` | `sessions.rs` |
| `session.title` | `session_id`, `title` (str; none = read only) | -32602 `session_id is required`, `No session <id>.`; -32000 store | `sessions.rs` |
| `session.mutate` | `session_id`, `operation` (str), `payload` (object read by field) | -32602 `session_id is required`, `target_message_id is required`, `cwd is required`, `No such folder: <f>`; -32000 `session busy` (+data), the move/store text | `sessions.rs` |
| `session.rewind` | `session_id`, `replay_only` (`== true`), `target_message_id` (`as_u64`; absent = "nothing") | -32602 `session_id is required`; -32000 `session busy` (+data) | `sessions.rs` |
| `rollback.list`, `.diff`, `.restore` | `session_id`, `hash` (str, trimmed lowercase), `file_path` (str), `all` (`== true`) | -32602 `session_id is required`; -32000 `No checkpoint <h>; /rollback lists them.`, checkpoints off, `session busy` (+data) | `sessions.rs` |
| `session.export` | `session_id`, `format` (str, default json), `redact` (`== true`) | -32602 `session_id is required`, `format is json, md or html`; -32000 `Session <id> not found` | `sessions.rs` |
| `session.foreign.list` | `source` (str), `offset` (`as_u64`, default 0) | none | `sessions.rs` |
| `session.foreign.preview`, `.import` | `id` (str, default "") | -32602 the importer's text; -32000 `No provider is configured`... | `sessions.rs` |
| `session.branch`, `.branch_whole`, `.branch_stored` | `parent_session_id` or `session_id` (non-empty str), `idempotency_key`, `through_message_id`, `count`, `cwd`, `name`; whole params | -32602 `session_id is required`, `No session <id>.`, the folder text; -32000 store | `sessions.rs` |
| `prompt.btw`, `prompt.background` | `session_id`, `text` (str, trimmed) | -32602 `session_id is required`, `text required`; -32000 `Session <id> not found`, `No provider is configured`, the runtime's | `turn.rs` |
| `file.attach` | `session_id`, whole params (`path`, `name`, `staged_upload`, `data_url`, `content_base64`) | -32602 `session_id is required`, `No session <id>.`, the importer text | `attachments.rs` |
| `image.attach` | `session_id`, `path` (str, trimmed) | -32602 `image not found: <p>`, `unsupported image: <p>`; -32000 actor | `attachments.rs` |
| `image.attach_bytes` | `session_id`, whole params | -32602 the upload's detail | `attachments.rs` |
| `image.detach` | `session_id`, `path` (str, trimmed) | -32602 `session_id is required` | `attachments.rs` |
| `insights.get` | `days` (`as_i64`, > 0, default 30) | none | `usage.rs` |
| `session.compress` | `session_id`, `focus_topic` (str, trimmed, non-empty) | -32602 `session_id is required`; -32000 not found, no provider, the compressor's | `sessions.rs` |
| `session.delete` | `session_id` | -32602 `session_id is required`; -32000 the runtime's | `sessions.rs` |
| `session.create` | `provider`, `model`, `cwd`, `title`, `messages`, `parent_session_id`; whole params | -32000 `No provider is configured`, `Unknown provider <id>`, `No model is chosen`, store | `sessions.rs` |
| `session.resume`, `session.history` | `session_id` | -32602 `session_id is required`; -32000 `Session not found: <id>` | `sessions.rs` |
| `prompt.submit` | `session_id`, `text` (a string, else the value's JSON text), `submission_id` / `input_id`, `attachments`, `confirm_truncate`, `truncate_before_*`, ...; whole params | -32602 `session_id is required`, `Unknown session <id>`, the attachments' text; -32000 `session busy` (+data), no provider, the runtime's | `turn.rs` |
| `approval.respond`, `clarify.respond` | `prompt_id` or `request_id` (str), `choice` / `answer` (str, default "") | -32602 `prompt_id is required`; -32000 `No pending approval|clarify request` | `turn.rs` |
| `approval.pending` | `session_id` | -32602 `session_id is required` | `turn.rs` |
| `session.context_breakdown` | `session_id` | -32602 `session_id is required`; -32000 `Session not found: <id>` | `sessions.rs` |
| `process.list`, `process.stop` | `session_id` | -32602 `session_id is required` | `delegation.rs` |
| `process.kill` | `session_id`, `process_id` (str, default "") | -32602 `session_id is required` | `delegation.rs` |
| `subagent.list` | `session_id` | -32602 `session_id is required` | `delegation.rs` |
| `subagent.tail`, `.interrupt` | `session_id`, `subagent_id` (str, default "") | -32602 `session_id is required` | `delegation.rs` |
| `subagent.steer` | `session_id`, `subagent_id`, `text` (str, trimmed) | -32602 `session_id is required`, `text is required` | `delegation.rs` |
| `delegation.status` | none | none | `delegation.rs` |
| `delegation.pause` | `paused` (`as_bool`, default **true**) | none | `delegation.rs` |
| `session.steer` | `session_id`, `text` (str, as sent) | -32602 `session_id is required` | `turn.rs` |
| `prompt.cancel` | `session_id`, `admission_id` (str, default "") | -32602 `session_id is required` | `turn.rs` |
| `prompt.steer_queued` | `session_id`, `admission_id` | -32602 `session_id is required`; -32000 `session_idle`, `admission_started` | `turn.rs` |
| `prompt.replace_queued` | `session_id`, `admission_id`, `text`, `submission_id` (any value, as sent) | -32602 `session_id is required`; -32000 `admission_started` | `turn.rs` |
| `session.redirect` | `session_id`, `text` (str, as sent) | -32602 `session_id is required` | `turn.rs` |
| `session.interrupt` | `session_id` | -32602 `session_id is required` | `turn.rs` |
| anything else | - | -32601 `Method not found: <name as sent>` | `rpc.rs` |

`rpc_table_tests.rs` and the table fixture cover the rest of the 80 (below); the live checks cover 36 of them.

### Design

**`Request`** (`gateway/rpc.rs`): `#[derive(Deserialize)] #[serde(tag = "method", content = "params")] enum Request`, one
variant per served method (80), `#[serde(rename = "a.b")]`, the payload `P<T>` where `T` is the method's params type
(`SessionRef`, `ProjectsCreate`, `ConfigSet`, `QueueRef`, ... 28 small structs, `Nothing` for methods without params, `Raw`
for those that hand the whole value on). `Request::parse(method, &params)` builds `{"method": m, "params": p}` and
`serde_json::from_value`s it; **any** failure is `RpcError::MethodNotFound(method)`, which is exactly the old `_` arm
(`Method not found: <name as sent>`, -32601, no `data`). No `#[serde(other)]`: the name is already in hand, and a name
with other case, spaces, a trailing NUL or an empty string simply is not a variant.

**How the leniency was kept.** Every old read is `params["f"]` then `as_str()` / `as_u64()` / `as_i64()` / `as_bool()` /
`== true` / `.clone()`: never an error for the shape of the JSON. serde's struct deserializer would break that three ways
(`null` params fail, an array is read positionally, a wrong type is an error), and a failed parse would look like -32601.
So `P<T>` implements `Deserialize` by reading a `Value` first and calling `T::from_params(&value)`, the old reads moved
into one place per method (`FromParams`), which cannot fail. What a handler required it still checks, in the same order:
`SessionRef::id()` is `session_id(params)?` ("session_id is required", -32602), the empty-text, empty-id, unknown-folder
errors are raised where they were. Defaults are the old ones (`preview_limit` 3, `days` 30, `paused` **true** unless a JSON
boolean, `offset` 0, the project `name` "Project"). The 15 methods whose handlers pass the params to a long function
(`session.create`, `.resume`, `.history`, `prompt.submit`, `session.mutate`, `.rewind`, `.compress`, `.export`,
`session.branch*`, `rollback.*`, `file.attach`) carry `Raw` (the old function reads 10-30 fields, and `enqueue_as` writes into the
value): typing them would be its own change.

**`RpcError`**: `MethodNotFound(name)`, `InvalidParams(text)`, `Server(text)`, `Busy`; `code()`, `message()`, `data()`
(`{"reason": "session_busy"}` for `Busy` only), `to_json()` (the `error` object, built as before: `code`, `message`, then
`data`), `into_parts()`. `RpcError::server("session busy")` **is** `Busy`, because the old `handle` attached the `data` to any
error whose message was exactly `session busy` (`rollback` and `rewind_turn` wrote it as a plain string). Constructors
`invalid` / `server` / `failed` (an `anyhow` chain, `{e:#}`). 65 tuple literals `(INVALID_PARAMS|SERVER_ERROR, text)` were
rewritten by a script that matches parentheses (not by hand), every `(i64, String)` became `RpcError`. `Reply = Result<Value,
RpcError>` is what handlers return; `Gateway::call` (ACP) still returns `Result<Value, (i64, String)>` (`RpcResult`, the type
the old tests and `acp/server.rs` use), converted with `into_parts` at the boundary: ACP and its tests are untouched.

**`dispatch`** is `Settings::load` (before parsing, as before, for every name) then `match Request::parse(..)? { Request::X(P(p))
=> self.handler(p).. }`, 80 arms of one line (a few of two or three). The arms that were inline bodies became handlers
(`projects.rs`, `settings.rs`, `usage.rs`, `delegation.rs`, `attachments.rs`, `slash.rs`, `sessions.rs`, `turn.rs`), same
statements in the same order, `self.blocking(..)` where it was. `blocking` / `rest_blocking` answer a failed thread as
`RpcError::server("The request stopped: ...")` (CP13c) and are in `mod.rs` / `rest.rs`. `handle`: the missing-method log
fires for `MethodNotFound`, the warning for the rest, the reply is built with `error.to_json()`; the admission release after
`prompt.submit` and the tracing lines are unchanged.

### The file split (pure moves, done last)

| File | Lines | What moved in (existing functions, verbatim apart from `pub(super)`) |
|---|---|---|
| `mod.rs` | 474 | `Gateway`, `new`, `new_for_acp`, `build`, `schedule`, `install_review_route`, `connection`, `handle`, `release_admission`, `call`, `blocking`, `dispatch`, the free `admission_id`, `session_id`, `send`; the layout table |
| `rpc.rs` | 1,065 | new: `Request`, params types, `RpcError`, `Reply`, tests (815 lines of types and the enum, 250 of tests) |
| `sessions.rs` | 1,101 | `active_sessions`, `context_breakdown`, `mutate`, `rewind_turn`, `rollback`, `window_for`, `status_text`, `context_text`, `compress_session`, `export_session`, `set_archived`, `rename`, `revision`, `bump_revision`, `pick_effort`, `session_info`, `branch`, `create_session`, `resume_session`, `viewing`, `auto_archive`, `pending_route`, `apply_pending_route` (+ the handlers for status, title, delete, foreign, branch, create, resume) |
| `turn.rs` | 920 | `turn_options`, `pending_rows`, `enqueue*`, `dequeue`, `subscribe`, `submit`, `run_queued`, `has_queued`, `set_live`, `end_live*`, `title_session`, `is_live`, `emit` (+ the handlers for submit, btw, background, respond, pending, steer, the queue, redirect, interrupt) |
| `slash.rs` | 387 | `review_task`, `coding_command`, `builtin_command` (+ catalog, complete.slash, complete.path, slash.exec, command.dispatch) |
| `settings.rs` | 588 | `apply_settings`, `apply_approvals_mode`, `model_options`, `switch_model`, `route_session`, `known_context`, `context_length`, `model_facts`, `sends_images`, `provider_for`, `session_provider`, `import_route`, `ModelFacts`, `FALLBACK_CONTEXT`, `provider_limits` and its tests (+ setup, profiles, config.get / set, the "off" answers) |
| `usage.rs` | 156 | `note_generation`, `usage_snapshot`, `requests_status` (+ requests.status, staging.*, insights.get) |
| `rest.rs` | 794 | `data_dir`, `file_path`, `ticket_allows`, `rest_blocking`, `rest`, `session_route`, `stored_messages`, `session_row(s)`, `session_list` |
| `projects.rs` | 137 | `project_list` (+ projects.*) |
| `attachments.rs`, `delegation.rs` | 95, 113 | new handlers only |
| `tests.rs`, `rpc_table_tests.rs` | 253, 365 | the old `budget_tests` (bodies unchanged), the new table test |

`git diff --stat -M` cannot call this a rename (ten files out of one), so the proof is a script: every non-blank line of
the pre-split `gateway.rs` (4,278 distinct-position lines, trimmed, `pub(super)` ignored) appears in the new files exactly
as often, and the only lines in the new files that were not in it are the `impl Gateway {` / `}` wrappers, the `use` lines,
`mod` declarations, the layout doc and the two test-module wrappers (`mod budget_tests {` -> a file); the dropped lines are
`use hermes_agent::events::AgentEvent;` (no longer used there). `git diff --color-moved=dimmed-zebra` shows the same.
Visibility edits: `ModelFacts` (and its two fields) and `FALLBACK_CONTEXT` are `pub(super)` (the struct field of `Gateway`
and two other files use them), every moved method is `pub(super)`.

### Tests (final tree, CARGO_TARGET_DIR=target/claude)
Run on the final tree (on top of fce5fec, hermes_testdir in the new tests; `clean-test-temp.ps1 -Delete` afterwards, no
"could not remove" line): `cargo build --workspace` OK, no warning; `hermes-desktop` lib **191** (183 + 6 `rpc.rs` + 2 table tests;
the old tests unchanged) and acp **68, three runs in a row**; `turns` 258 passed / 7 ignored under default, `requests` and
`tokens` (turns.rs untouched; run before fce5fec, which only touched test folders); `hermes_runtime --lib` 265; agent 45, llm
489+5, store 34, tools 337, memory 101, prompts 37 (all ok).


### Golden comparison (old vs new, byte for byte)

**1. Per-method gateway test** (a throw-away module, copy in the scratchpad `golden_tmp.rs`): for each of the 80 served
names and 11 unserved ones, a fresh data folder (config.json with a mock provider on a closed port, a working folder with a
text file and a PNG, one session and one project created first), then ~21 generic params (absent, `{}`, `[]`, `["a","b"]`,
`"x"`, `7`, `true`, `{"session_id":5}`, `{"session_id":""}`, an unknown id, the real session id, with extra fields, nulls,
and an object with all 60 field names set to `5` / `"5"` / `null` / `true` / `[]` / `{}` / `-1` / `1.5`) plus 0-18 payloads
shaped like the page's (from the live checks), 2,181 answers in all, every one the code and text of the error or the
whole JSON of the success, normalised for ids, long numbers, temp and home folders, task ids and image names. Run at
a110767 **twice** (noise floor: identical, 0 differing lines after normalising two random task ids and two image names),
after the typed dispatch with the handlers still in one place (stage 1) and after the file split (stage 3): **0 differing
lines in 2,181, both times.** Includes: every error text and code in the inventory, `session busy` + the lenient reads, the
-32601 names (`Gateway.Ping`, `gateway.ping `, empty, ...), `projects.create` with wrong-typed folders, `delegation.pause`
with `"false"`, `insights.get` with 0 / -3 / 1.5 / `"7"`, `session.branch` idempotency, `session.mutate` operations, `file.attach`
and `image.*` over paths and base64, the slash commands and coding commands, `prompt.submit` over a real session (queued
while a turn runs), `session.delete`.

**2. Live traffic** (every request and reply frame written to a file by a throw-away line in `handle`, HEAD's exe and the
new exe each built with it, removed afterwards: the exes are in `target/claude/head` and `target/claude/after`, the
sources are clean): the fifteen live checks, then `rpc-dispatch-check.mjs`, on a fresh `target/perf/home` each. Compared per method as a multiset of (params, reply) after normalising ids (session, project, admission, task, uuid),
long numbers, ISO times, temp and checkpoint names (`trace_diff.cjs`, scratchpad). **802 request / reply pairs** (159 from the
fifteen checks, 643 from `rpc-dispatch-check.mjs`; the new exe was run twice, the second time with the folders named as at
HEAD): **801 identical; one `session.resume` reply differs only in `info.avg_tps` (80.x against 81.x), the status bar's
measured generation speed, which comes from the mock's timing**. No error code, message or `data` differs; the busy
refusals (`session busy` + `data.reason`), the workspace "No such folder" error and the approval / clarify answers are among
the identical ones. Per-method counts: `scratchpad/traces/*.jsonl` (not kept in the repo).

**3. The table fixture** `tests/fixtures/rpc_table.txt` (900 lines) was recorded at a110767 (the HEAD gateway plus the
test module, blessed, run again unchanged to prove it stable) and passes unchanged on the new code.

### Methods not covered by a live check
Of the 80 served methods the fifteen requested live checks call **36**. The other **44**: `client.capabilities`,
`commands.catalog`, `complete.path`, `complete.slash`, `delegation.pause`, `delegation.status`, `free_tier.status`,
`gateway.ping`, `image.attach`, `model.options`, `onboarding.state`, `pet.info`, `process.kill`, `process.list`,
`process.stop`, `profiles.list`, `projects.add_folder`, `projects.create`, `projects.delete`, `projects.discover_repos`,
`projects.list`, `projects.project_sessions`, `projects.record_repos`, `projects.set_active`, `projects.update`,
`requests.status`, `session.active_list`, `session.close`, `session.context_breakdown`, `session.delete`,
`session.foreign.import`, `session.foreign.list`, `session.foreign.preview`, `session.history`, `setup.runtime_check`,
`setup.status`, `shared_metrics.status`, `staging.apply`, `staging.reject`, `subagent.interrupt`, `subagent.list`,
`subagent.steer`, `subagent.tail`, `wake.status`. Where each is covered instead: **all 80** by the new
`rpc-dispatch-check.mjs` (live, over the socket: no -32601, shapes, the errors) and by the table test + golden record (every
method over ~25 params); some also by existing checks outside the requested list (`subagents-check.mjs`: `subagent.*`,
`delegation.status`; `import-check.mjs`: `session.foreign.*`; `requests-check.mjs`, `fallback-check.mjs`:
`requests.status`; `endpoints-check.mjs`: `model.options`; `deferred-tools-check.mjs`: `session.context_breakdown`;
`budget-check.mjs`: `delegation.status`) that were not re-run.

### Live checks (own instance, port 47810, fresh `target/perf/home`, main checkout's `tools/node/node.exe` run in place,
working folders in `%TEMP%\p13f`, `mock_llm.py` started and stopped by its PID)
All passed on the HEAD exe (a110767 + the throw-away trace line) and on the new exe (twice): `smoke`, `stop-check` (both cases:
a stalled reply and a slow command, interrupted, `running: false`), `rollback-check` (restored, file gone) with
`mock_llm.py` started and stopped by its PID; `queue-check`, `side-and-branch`, `archive`, `workspace`, `staging`,
`memory`, `toolsets`, `attach`, `review`, `coding-commands`, `provider-error`, `preinject` as their own provider; and the
new `rpc-dispatch`. Each ran on a freshly started instance with a deleted `target/perf/home` (so `attach` was a fresh
instance too). The exes: `target/claude/head`, `target/claude/after` (they carry the trace line; the sources do not). No
timing check; `stop-check`'s "complete=" milliseconds vary (14 ms to 244 ms) as on any run, it asserts only that the turn
ends and stays ended. `perf.ps1 -Do start` was not used (its timing step needs `tools/node`, which a worktree lacks): the
runner starts the exe with perf.ps1's environment (`HERMES_RS_HOME`, port 47810, WebView2 folder, `RUST_LOG`).

### Deviations from the plan
- **15 of 80 methods carry `Raw`** (and 6 more keep the raw value beside typed fields) (the whole params) instead of a typed struct, as the task allowed ("fall back to the raw
  Value for that method"): their handlers are long functions that read many fields themselves; typing them would touch
  `submit` (460 lines), `branch`, `create_session`, `mutate`, with no way to keep "no behaviour change" cheap. The other 65 are
  typed, including every method with a read the golden record could show a difference in.
- `Request::parse` clones the params once (`from_value` wants an owned value); the handlers cloned them anyway.
- The module for slash commands is `slash.rs`, not `commands.rs` (`crate::commands` is the logic module `gateway` imports).
- The full per-method golden is not committed (about 1 MB and four minutes); the compact table (900 lines, 85 KB, ~27 s) is.
- Added `scripts/perf/rpc-dispatch-check.mjs` (the live check of every method) and changed `scripts/contract.mjs` to read
  `gateway/rpc.rs` (it grepped `"name"` in `gateway.rs`).
- `git diff -M` shows no renames (one file became ten); see "The file split" for the proof.
- Not done: the optional `mimalloc` line (measure first; the user's rule forbids it for now), the `PHASES.md` closing.

### For the reviewer
1. **Params, method by method**: read `rpc.rs` `FromParams` impls against the inventory table above (each read is the old one;
   defaults; which fields are trimmed; `delegation.pause` default true; `projects.create` `use == true`; `insights.get`).
2. **-32601**: `Request::parse` is the only place a name is refused; `a_name_that_is_not_exactly_served...` and the table's 12
   unserved names x 8 params, `rpc-dispatch-check.mjs`. No `#[serde(other)]`.
3. **Errors**: `to_json` builds the same object as the old `handle` (`code`, `message`, `data` only for `Busy`); `Busy` from any
   `"session busy"` message. ACP: `Gateway::call` returns `(code, message)` as before; its tests ran 3 times.
4. **CP13c / CP13d rules**: `git diff --color-moved` of the handlers against the old arms; `self.blocking(..)` /
   `spawn_blocking` calls are where they were (the arms that ran inline still run inline: `process.*`, `subagent.*`,
   `session.steer`, the queue calls, `approval.*`); Stop, interrupt and redirect handlers are the old statements;
   `Settings::load` still runs before parsing.
5. **The split**: the line-accounting above (rerun it: `git show a110767:crates/desktop/src/gateway.rs` vs the files).
6. `tests.rs` is `budget_tests` unchanged except `mod` -> file; `rpc_table_tests.rs` takes ~27 s (1,100 calls on one gateway).
7. Docs: ARCHITECTURE (gateway section), CONVENTIONS, PITFALLS (new section), TESTING, PORTING (two rows), GAPS-PLAYBOOK,
   `scripts/perf/README.md`, `scripts/contract.mjs`; a few stale `gateway.rs` mentions in `docs/acp/` fixed, the history
   docs (`PLAN.md`, `GAPS.md`) left.

### User checklist (no visible change is expected; this is a plumbing change)
1. Rebuild and restart your app (`cargo build -p hermes-desktop`, your own `target/debug`), open it: the sidebar lists your
   sessions and the project tree as before.
2. Open a chat, send "say hi": the reply streams, the title appears, the status bar shows tokens.
3. While a turn runs, type a second prompt: it queues (the queue panel); press Stop: the turn stops at once.
4. Settings: change the model with the picker, switch Settings > Sessions auto-archive, set the budget profile: each is kept
   after a restart.
5. Move a session to another folder (composer folder picker): it works, and while a turn runs it says the session is busy.
6. Negative: Voice / pet / "wake" features stay hidden (the backend still answers "not here" for them); nothing new appears.

### Doubts
1. **15 methods carry `Raw`**: typing them is the part of "typed dispatch" this change does not do; each is a long function
   that mutates or forwards the value (`enqueue_as` writes into it and would panic on a non-object `params`: still true).
   A reviewer who wants them typed should ask for it as a step of its own with a golden record, not fold it in here.
2. **The golden and the trace instrumentation are not in the repo** (a 1 MB, four-minute test and a patch of 12 lines).
   They are in the session's scratchpad (`golden_tmp.rs`, `trace_on.cjs`, `trace_diff.cjs`, `runchecks.ps1`); the table
   fixture and `rpc-dispatch-check.mjs` are the lasting part. The compact table test takes ~27 s (1,100 calls on one
   gateway, the slow ones are `session.create` and the commands that read the skills): trim the spread if that is too slow.
3. `git diff -M` shows the old file as deleted and ten new ones; `git add -N` was run so they appear in `git diff`. Nothing
   is committed.
4. `scripts/contract.mjs` was edited but not run (`target/hermes-src` is a placeholder here); the change is two lines.
5. Per-call cost: `Request::parse` builds one `json!` object (a clone of the params) and one `from_value`. Not measured
   (the user's rule); the handlers cloned the params at least once already.
6. `prompt.btw` is a plain `fn` that `tokio::spawn`s (it was async code before); it is only called from `dispatch`, inside the
   runtime, like the old arm.
7. `session_id(&Value)` (free function) and `SessionRef::id()` both say "session_id is required": the raw handlers use the
   first, the typed ones the second; the tests pin the text.

### Review (Opus, 2026-11-17)
Reviewed the uncommitted tree on fce5fec with comparisons of my own (not the implementer's golden artifacts alone).

**1. Method set.** Extracted every top-level arm of HEAD's `dispatch` (`git show HEAD:crates/desktop/src/gateway.rs`,
a brace-matching script; the guarded `"config.set" if key == model` and `"slash.exec" if goal...` arms folded into their
names) and compared with the `rename = "..."` list of `Request`: the same 80 names, none lost, none added. The other
`match method` blocks in the old file were inside handlers (subagent tail / steer / interrupt) or REST (`rest`, moved
verbatim). `Request::parse` is the only refusal; a name that is not exactly a variant (case, spaces, NUL, empty, a prefix,
`rpc.discover`) is `MethodNotFound`, -32601 `Method not found: <name as sent>`, no `data`, logged under
`hermes_desktop::missing` as before. The socket layer (`connection`, `handle`) is line-identical but for the error branch:
non-JSON lines skipped, a frame without a string `method` (a batch array, a response, `method: 5`) gets no reply, `id`
echoed as sent (string, number, null), absent `params` = `Null`, extra fields ignored. These are now checked live
(`rpc-dispatch-check.mjs` section 0, added in review) and pass on HEAD's exe and on the new one. `scripts/contract.mjs`
(not runnable here: no `target/hermes-src` export) was checked by running its old and new `served()` over Hermes's
contract (`../hermes-agent/apps/shared/src/gateway-contract.openrpc.json`, read only): 256 of 257 methods agree; the one
that moved, `session.usage`, was a false "answered" before (the old check matched any `"session.usage"` string literal in
`gateway.rs`); 68 contract methods are answered.

**2. Params.** Read every `FromParams` against the old arm (defaults, trims, `== true`, `as_u64` / `as_i64`, `paused`
default true, `use`, `request_id` for `prompt_id`, `choice` vs `answer` by method) and the order of the checks in each
handler (session id first, then text, then the rest): equal. Then a golden of my own: a throw-away `git worktree add` of
fce5fec (`hermes-rs-p13-head`, removed afterwards) and the new tree each ran the same scratch test, 764 answers: nine odd
params shapes on every method (arrays, `true`, `1.5`, ids that are arrays / objects / blanks, every field `true` or
`[]`), then 100 calls on a real session and a real project (titles, mutate operations with wrong-typed payloads, rewind,
rollback, export formats, branches, effort, model, images, `file.attach`, processes, subagents, the queue calls, steer,
redirect, interrupt, ten slash commands, `command.dispatch`, completion, compress, projects update / add folder with
`is_primary` 1 and "true", tree, delete, insights, delegation, the btw / bg refusals, `session.delete`), successes kept in
full up to 600 characters, ids, temp names and times normalised: **0 differing lines in 764.** The table fixture
re-recorded at HEAD (the table test adapted to HEAD's gateway, blessed, on the second path
`C:\Users\alexandros\repos\hermes-rs-p13-head`) is **byte-identical** to the committed `rpc_table.txt` (900 lines): it
really is the old gateway's, and it does not depend on the checkout's path. Which methods answer -32602, -32000 or
success is unchanged throughout (no served method can fail to parse: `P<T>` reads a `Value` first).

**3. Errors and ACP.** `to_json` builds `{code, message}` plus `data: {reason: "session_busy"}` for `Busy` only; every
"session busy" text goes through `RpcError::server` (two `Busy`, two `server("session busy")`, one through `rollback`'s
`error` closure) and so keeps the `data`; nothing builds an `RpcError` variant directly outside `rpc.rs`; no
invalid-params text can be "session busy" (the old rule looked at the message for any code). The 65 rewritten literals:
the multiset of old and new lines (script below) pairs each `(INVALID_PARAMS|SERVER_ERROR, text)` with an
`RpcError::invalid|server|failed(text)` of the same text; `failed` is `{e:#}` as before. `Gateway::blocking` answers a
failed thread `server("The request stopped: ...")` (same text); `rest_blocking` and the REST error paths answer
`(500, {"detail": message})` as before (`error.message()` is the old string); `session.foreign.*` keep their own
`e.to_string()` join errors. `Gateway::call` returns `(code, message)` through `into_parts`; `acp/server.rs` is unchanged.

**4. The split.** My own check: every non-blank trimmed line of HEAD's `gateway.rs` against the new files (minus `rpc.rs`
and the table test), as multisets, `pub(super)` ignored. Only in the old file: the dispatch arms, the `params[..]` reads
now in `FromParams`, the tuple error literals and `tempfile::tempdir()` in the old test module (now `hermes_testdir`).
Only in the new files: their typed counterparts, the handler signatures, `impl Gateway {` wrappers, `use` / `mod` lines
and module docs. Nothing else moved or changed. No statics, `OnceLock`s or thread-locals in either; the two
`#[cfg(test)]` modules are `settings.rs`'s provider-limit tests and `tests.rs`; `use super::*` in the area files resolves
the same names; every `self.blocking(..)` / `spawn_blocking` is where it was, the arms that ran inline still do
(`process.*`, `subagent.*`, `session.steer`, the queue calls, `approval.*`; `prompt.btw` is now a plain fn that
`tokio::spawn`s inside the runtime, as the arm did); no std `Mutex` guard is held across an `.await` in a new handler;
Stop, interrupt and redirect are the old statements. After these checks, `rustfmt` on `gateway/` (the old `gateway.rs`
was rustfmt-clean, the moved and de-indented code was not); the table test ran green on the result.

**5. `Raw`.** Kept: the 15 `Raw` methods (and the six with `raw` beside typed fields) hand the value to long functions
(`submit`, `branch`, `create_session`, `mutate`, `rollback`, `export`, `switch_model`, `coding_command`, `builtin_command`,
the image upload) that read 10-30 fields, and `enqueue_as` writes into it. Typing them now would re-implement those reads
for little compile-time gain at a real parity risk; the enum already makes the dispatch exhaustive. A later step with its
own golden (PHASES, Phase 13 "Done").

**6. Test hygiene.** The table test uses `hermes_testdir`, a fixed config on a closed port, no clock in its lines (ids
and long numbers normalised) and no port; it ran green twice on the new tree and blessed identically at HEAD on another
path. One soft spot, left: the `complete.path` row with an unknown session falls back to the home folder and records only
`{items}` (a machine with an empty home folder would print `{"items":[]}`).

**Fixed in review:**
- ACP `undo_n_drops_n_turns` failed once in the first full run (the last request still held "two" / "three"): the same
  race as CP13d's `/retry` test, the background review that `/undo` (a learning signal) starts, whose 500-and-retry
  request landed after the next turn's (the desktop app ignores `background_review: false`, GAPS section 4). A
  `turn_requests` helper filters the review's requests out, used by both `/undo` tests and the `/retry` test. Not caused
  by CP13f.
- `rpc-dispatch-check.mjs`: the envelope section (ids, frames that get no reply, two frames in one message,
  `params: null`).
- `slash.rs`: `builtin_command`'s doc comment sat on top of `review_task` (as in the old file); moved back. `mod.rs`'s
  doc named `commands.rs` for `slash.rs`.
- Docs: stale single-file `gateway.rs` references in `docs/acp/PHASES.md`, `docs/acp/README.md`, `PITFALLS.md`,
  `TESTING.md` (which also still said `tempfile::tempdir()`) and `GAPS.md`; `scripts/perf/README.md` (the envelope).

**Tests (final tree, CARGO_TARGET_DIR=target/claude):** `hermes-desktop` lib 191 (twice) and acp 68 (three runs in a
row after the `turn_requests` fix; the first full run before it: 67 + `undo_n_drops_n_turns`, then three green runs
without the fix too, so it is a race, not a regression); `turns` 258 passed / 7 ignored under default, `requests` and
`tokens` (first run on top of fce5fec); `hermes_runtime --lib` 265; agent 45, llm 489 + 1 + 4, memory 101, prompts 37,
store 34, testdir 4, tools 337; `cargo build --workspace` OK, no warning (placeholder `target/hermes-src`, removed
afterwards); `scripts/clean-test-temp.ps1 -Delete` afterwards.

**Live checks** (own instance, port 47810, a fresh `target/perf/home` per check, the main checkout's
`tools/node/node.exe` run in place, the exe started directly with perf.ps1's environment (no timing step), working
folders in `%TEMP%\p13f-review` with a file in each, `mock_llm.py` started and stopped by its PID): all passed on the final exe, 20 checks: `smoke`, `stop-check` (a stalled
reply and a slow command, both interrupted, `running: false`), `rollback-check` (restored, file gone), `endpoints`
(with the mock), `rpc-dispatch` (with the new envelope section), `queue`, `side-and-branch`, `archive`, `workspace`,
`staging`, `memory`, `toolsets`, `attach` (fresh instance), `review`, `coding-commands`, `provider-error`, `preinject`,
`import` (a Hermes home built by `make-hermes-db.py` from `../hermes-agent`'s `hermes_state`, read only with
`PYTHONDONTWRITEBYTECODE`, as `target/hermes-src` has no export here), `subagents`, `requests`. `rpc-dispatch-check.mjs`
(87 checks, envelope included) also passed against HEAD's exe (fce5fec, built in the throw-away worktree): the check
describes the old behaviour. Not run: `scripts/contract.mjs` (no `target/hermes-src` export; checked as in point 1).

**Left:** the desktop app ignoring `background_review: false` (GAPS section 4, pre-existing); typing the `Raw` methods.
