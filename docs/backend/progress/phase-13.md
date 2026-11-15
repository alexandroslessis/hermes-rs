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
