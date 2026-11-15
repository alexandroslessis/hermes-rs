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
