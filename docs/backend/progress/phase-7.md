# Phase 7 progress

## CP7.1 — learning signals: detection, weights, evidence excerpts, persistence
Worktree: C:\Users\alexandros\repos\hermes-rs-p7   Branch: backend/p7-review   Model: Opus 5.5 (subagent)
Cut from `main` at c941729 (Phases 0-6 merged). Uncommitted.
Done:
- **New `crates/runtime/src/signals.rs`** (+ `signals/tests.rs`). Pure `analyze(TurnBase, TurnOutcome)
  -> Analysis` and `rewind_signal(..)`; types `Signal { seq, kind, weight, turn, message_id, at,
  evidence: Vec<Excerpt> }`, `SignalKind` = `Correction{phrases}` | `Explicit{via: remember|learn}` |
  `Rewind{via: rewind|undo|rollback, turns}` | `Denial{what, reason}` | `Workaround{tool, failures}` |
  `LongTurn{calls, tools}`; `SessionSignals` with `score()`, `triggered()`, `evidence()` (unreviewed,
  oldest first), `mark_reviewed(seq)`.
- **Weights** (constants, placeholders for settings): `REVIEW_THRESHOLD` 3.0; one message capped at
  2.5 (never enough alone unless explicit); weak "instead" 0.75; "no," 1.0 (start only); wrong /
  not-what-I-asked 1.25; don't 1.0; always/never rule 1.5; "from now on"/"I prefer" 2.0 (start of
  message; elsewhere lower); explicit 10; rewind/undo 3.0; denial 2.0; workaround 1.5; long turn
  (>=15 calls, completed with an answer) 2.0, >=30 calls 3.0. Two corrections cross, one strong
  signal crosses.
- **False-positive design**: text cleaned first (fenced/inline code, `>` quotes, indented code,
  log/stack/code-looking lines, `"..."`/`'...'` quotes, pasted e-mail from its headers on, long
  pastes cut to 600 chars); English only (non-Latin script or no common English word -> nothing);
  generated prompts (`/plan`, `/init`, skills, bundles, `/review`, anything starting `/` or `[/`)
  skipped, `/learn` explicit; imperatives only at sentence/clause start, always/never only before a
  listed verb, "don't worry/mind/know/..." idioms excluded, wrong-idioms ("nothing is wrong",
  "went wrong", "I used the wrong...") removed; questions count only for "why did you..." and half
  for non-wh "instead"; first message: rejections 0, rules x0.75, don't x0.5; answer to the
  assistant's question: "no" 0, others x0.5 (durable markers keep full). Only user text and steers
  are read, never assistant text or tool output.
- **Wiring**: `TurnState` gets `calls`/`call_count`/`steers` (`GuardedTools::call` records every
  call with `classify_failure`; denial = Hermes's "BLOCKED: User denied this command."; `DeliverSteer`
  records steers). `begin_turn` builds `TurnBase` before the message is added, reads the final
  answer before releasing `running`, and right before `TurnFinished` sends `analyze(..)` to the
  store. `Runtime::rewind` -> `rewind_as(.., via)` (`undo` from `/undo`, `rollback` from checkpoint
  restores) records a Rewind when the dropped range holds something the agent did (command
  reports excluded). Denial reason = the user's next typed message, attached to the latest
  unreasoned denial. `delete_session` forgets the file. New API: `Runtime::signals(id).await`,
  `Runtime::mark_signals_reviewed(id, seq)`, `pub mod signals`.
- **Persistence**: `data_dir/signals/<id>.json` (`v`, `next_seq`, `reviewed_seq`, `dropped`,
  `dropped_weight`, `signals`), written by one OS thread (`SignalStore`, ledger pattern: channel
  sends, ordered, read-modify-write per change, temp + rename, joined on last drop). Caps: 32
  signals (reviewed evicted first, then lightest non-explicit; evicted unreviewed weight stays in
  the score), 6 excerpts/signal, 400 chars/excerpt, 300 for tool text, 500 call records/turn.
  Ids that aren't `[A-Za-z0-9_-]` never become paths; damaged file -> fresh.
- `/learn` wired as an explicit signal (trivial: `learn_request` on the turn's text).
- Docs: PORTING (row, diverged), ARCHITECTURE (crate row, turn step 5), GAPS (Phase 7 in
  progress), PITFALLS (new section).
Files: crates/runtime/{Cargo.toml (regex with `unicode`), src/signals.rs (new), src/signals/tests.rs
  (new), src/lib.rs, src/ops.rs, tests/turns.rs}, Cargo.lock (runtime now lists `regex`), GAPS.md,
  PORTING.md, docs/ARCHITECTURE.md, docs/PITFALLS.md, docs/backend/progress/phase-7.md
Tests (CARGO_TARGET_DIR=target/claude):
- `cargo test -p hermes_runtime` lib 97 passed (20 new in `signals::tests`: an ~80-case corpus, 33
  positives/explicit + ~48 tricky negatives, each labelled; threshold arithmetic; head weighting; long
  paste; regexes compile; denial marker tied to `approval::denied_message`; evidence context;
  assistant text/tool output ignored; `/learn`; denial + reason; workaround and 4 non-workarounds;
  long turn and its edges; steers; rewind; caps/eviction/review; file round-trip, damaged file,
  unsafe id; store ordering + restart + forget).
- `--test turns` 142 passed / 7 ignored under default, balanced, requests, tokens. 5 new: a
  correction costs no request (2 requests for 2 turns, same system prompt, plain user message) and
  survives a second `Runtime` on the same data folder, deletion forgets it; `/undo` adds a Rewind,
  keeps the earlier correction and triggers, a page rewind adds one, an empty rewind doesn't; a
  denied command -> Denial (2 requests); a 15-call turn -> LongTurn (2 requests); `/btw` adds none.
- `hermes_tools` 181 (+1 ignored), `hermes_memory` 22, `hermes_store` 15, `hermes_prompts` 12; `hermes-desktop` 174 + 68 (acp); `cargo build --workspace` OK.
Request counts: unchanged (asserted in the new turns tests).
Deviations from the plan:
- Evidence is captured text, not pointers (plus the user message id): survives rewinds and
  compaction; ±2 context = previous user message, previous answer, the message, the reply.
- Hermes's runtime `/retry` after a provider error (`retry_turn`) is not a signal (the provider
  failed); the page's `/retry`, `/undo`, edit, regenerate are rewinds and are.
- A branch starts with no signals (the parent keeps them; copying would double-review).
- The "remember this" explicit trigger also covers "remember that <fact>", "save this to memory",
  "can you remember this"; "remember to X" is only a weak rule (1.0).
- Persistence goes through a writer thread instead of `spawn_blocking` (same guarantee: no disk
  work on async workers; plus ordering, which `spawn_blocking` doesn't give).
For the reviewer:
- Weights and thresholds are judgement calls; check the corpus labels you disagree with first
  (e.g. "Don't X" in a first message scores 0.5; "Use the existing helper instead." 0.75;
  `remember that <X>` forces a review).
- Workaround exclusions (`NOT_WORKAROUNDS`: read/search/patch/write/web/...) mean only terminal,
  `process_manage`, MCP and unknown tools count. A `cargo test` failing twice with different test
  filters then passing would count (1.5, below threshold alone).
- `TurnBase::before` scans the whole conversation once per turn (sync, under the conversation
  lock); cheap but O(n).
- `CallRecord::new` serialises each call's args (then cuts to 2000 chars): a huge `write_file`
  content is serialised once more per call.
- Two processes on one data folder: last writer wins per change (each change re-reads the file).
- Only the new `signals.rs`/`signals/tests.rs` were rustfmt'ed; `lib.rs`, `ops.rs`, `turns.rs`
  already had pre-existing rustfmt diffs on this branch (not from this checkpoint), left alone.
- Not built: packet, review request, JSON contract/apply, scheduling, prompt (CP7.2/7.3). The
  existing nudge-driven review is untouched.
- A gitignored junction `target/hermes-src` -> the main checkout's was made for the desktop build.

### Review (Opus 5.5)
Main had not moved (branch still at c941729); no merge.
Findings (false positives first; ~75 new adversarial inputs run through `score_text`, kept as
`reviewer_corpus()` in `signals/tests.rs`):
- **Ordinary messages that scored** (fixed): narrated "instead" ("I went with sqlite instead",
  "Instead of reqwest I used ureq") 0.75 each, so four narrations in a long session made a
  review; status reports after "No," ("No, still failing", "Nope, same error", "No, I haven't
  tried that") 1.0 each, so a normal debugging loop made a review in three turns; "why did you
  choose X?" (curiosity, frequent for a user learning Rust) 1.25; "I never said it was easy"
  1.25; "You missed nothing" 1.25; "wrong" as a topic ("Handle the case when the key is wrong",
  "Add a test for the wrong path case") 1.25; forecasts ("In the future this module will be
  split") 2.0; pasted chats ("User: no, that's wrong / User: never do that again") 2.5 and
  "Alice: Don't merge / Bob: Always rebase" 2.25; URLs and Windows paths containing
  never/wrong/instead 0.75; "Remember that function we wrote yesterday, extend it." was an
  **explicit** (10, forces a review). Fixes: `NARRATION` (first person + past tense, no "you")
  excludes "instead"; status words added to `COURTESY`; "why did you" only with a complaint verb
  (+ "why didn't you"); "I didn't/never ask/say" only with an object that makes it a correction;
  `NOT_WHAT_IDIOMS`; conditional/prepositional "wrong" idioms; `FUTURE_SUBJECT` (a non-"you"
  subject before will/'ll/might) for durable phrases; speaker lines of a pasted chat (≥2,
  labels like "Note:" excluded) and URLs/paths removed in `clean`; "remember that" explicit only
  when its clause has a verb (`FACT_VERB`). Every original corpus label still holds.
- **Resend double-count** (fixed): `/undo` restores the message to the box; resending it counted
  the same correction again (plus the rewind). Signals from the user's words now carry
  `key` (FNV hash of the words + what they answered, not the text); `SessionSignals::add` skips a
  key already unreviewed. Same words to a later answer still count. Runtime test extended (undo,
  resend → no new correction, 3 requests).
- **Workaround false triggers** (fixed): any terminal command failing twice and then *any other*
  terminal command succeeding counted (`cargo build` ×2 failing while the agent fixes code, then
  `git add`; `rg` exiting 1 on no match ×2, then `rg` with a hit). Now probing programs
  (`rg`/`grep`/`test`/`Test-Path`/`ls`/read-only `git`…) are skipped, and for the terminal the
  success must share a non-program word with a failure and the failures must differ or the
  program change (`npm test`→`npx jest`→`pnpm test` and `pip`→`pip3`→`pip --user` still count).
- **Privacy** (fixed): excerpts kept pasted keys verbatim (`sk-or-…` in "remember this: my key
  is …", `Authorization: Bearer …` in a denied curl). `excerpt()` now runs
  `hermes_tools::redact::redact(.., Output)` before cutting (with slack so a key at the cut is
  still recognised); `CallRecord::output` is cut only, masked when it becomes evidence. Deleted
  with the session (existing `forget`); nothing is sent anywhere in CP7.1.
- **Cost on a huge paste** (fixed): `clean` ran its regexes over the whole message before the
  head cut: 5 MB took ~3.8 s (debug) on the turn's async task. Now only the first `MAX_READ`
  (16 Ki chars) is read: ~10 ms worst case. Rust's `regex` has no catastrophic backtracking;
  timed adversarial inputs (quotes, newlines, LEAD repetition) are linear.
- **Writer robustness** (fixed): a panic in one change killed the thread silently (every later
  change and snapshot lost) → `catch_unwind` per op. A file whose `v` is newer than ours was
  parsed as damaged and overwritten → left alone now. Unbounded `mpsc` is fine (tiny ops);
  joined on last drop; an app killed without dropping `Runtime` loses at most the ops of the
  last milliseconds.
- Checked, no change: `begin_turn` adds no await, no request, no change to messages or prompt
  (turns tests assert requests and the sent system prompt); only `begin_turn` (main turns, ACP
  included) and `rewind_as` produce signals; every rewind path (page rewind/edit/regenerate/
  `/retry` via `Runtime::rewind` in gateway.rs ×3, `/undo`, checkpoint `rollback`) goes through
  `rewind_as`; compaction doesn't touch the file (captured text); branch starts empty; store
  schema untouched; UTF-8: all cuts are by chars, the one byte slice is at a regex match end.
Left, with reasons:
- No decay or per-session rate limit: the plan says the score accumulates; once reviewed it
  restarts at 0, and CP7.3 schedules at most one review per idle period. Documented in the
  module doc.
- "I prefer not to say" / "I'd rather wait for the release" still score 2.0 (Prefer): rare, and
  splitting situational "rather" from durable "prefer" needs more than a regex. "Don't stop now,
  keep going" scores 1.0. "No"/"wrong" alone after a statement score 1.0/1.25 (plausibly real).
- Long turns count read-only exploration calls too (a 15-file read is a "procedure"); judged
  acceptable at 2.0 (below the threshold alone).
Tests (CARGO_TARGET_DIR=target/claude): see the commit; `hermes_runtime` lib 103 (6 new:
reviewer corpus, huge message, resend dedupe, secrets masked, probing/unrelated not workarounds,
newer file left alone), `--test turns` 142 / 7 ignored under default, balanced, requests, tokens;
`hermes_tools`, `hermes_memory`, `hermes_store`, `hermes_prompts`, `hermes-desktop` green;
`cargo build --workspace` OK.

## CP7.2 — evidence packet, one-shot request, JSON contract + repair, deterministic apply, staging
Worktree: C:\Users\alexandros\repos\hermes-rs-p7   Branch: backend/p7-review   Model: Opus 5.5 (subagent)
On top of the CP7.1 commit 123e118. Uncommitted.
Done:
- **New `crates/runtime/src/review_once.rs`** (+ `review_once/{packet,contract,apply,udiff,tests}.rs`,
  `golden_packet.txt`). `run_review_once(OnceJob, evidence: Vec<Signal>) -> ReviewOutcome` (crate-
  private) and the public wrapper `Runtime::review_once(session, provider, &TurnOptions,
  ReviewOnceSettings) -> ReviewOutcome` that CP7.3 calls. `ReviewOutcome { status: Done |
  NothingToReview | Skipped | Failed, requests, json_schema, ops: Vec<OpReport>, reviewed_seq,
  packet_tokens, budget_tokens }`, `actions()` gives Hermes's summary lines for the app.
  `ReviewOnceSettings { packet_tokens (24k), require_approval, format: Auto | JsonSchema | Text }`
  (knobs; CP7.3 reads them from the config). Nothing calls it from the app yet: the agentic
  review is untouched and still the active one.
- **Packet** (`packet.rs`): signals with excerpts, skills used in the session (`skill_view` of
  SKILL.md, the `preinject` note, `/skill` invocations; first-use order), top 3 related skills
  from the index (query: the user excerpts of the signals + the first message, project name;
  index default floor 2.5; used ones excluded), MEMORY.md / USER.md entries with usage. Fixed
  order, no clock. Budget: `min(knob, window/2)` tokens at bytes/4 (compress.rs's estimator);
  one writer refuses what doesn't fit, so the bound holds by construction (test over 11 budgets
  0..200k on a worst-case input). Priority: signals (≤40%, chosen explicit → weight → age, shown
  by seq, "N weaker left out"), used skills (≤30%), related (≤20%), memory gets the rest (≥10%,
  whole entries, "N more entries not shown"). Skill bodies cut at 12k chars or the room left.
  Everything through `redact(.., Output)`; `<excerpt`/`<skill`/`<entry` tags inside untrusted
  text become `&lt;…`. A skill is "shown whole" only if not cut, masked or defused.
- **Request**: one `complete()` with no tools, system = `evidence_review_prompt` (new, in
  `crates/prompts/src/review.rs` beside the agentic one), user = the packet; `response_format:
  json_schema` (strict schema, `contract::schema`) merged into the request params with the
  effort params. Aux review model when set, else the session's model; governor `Review` check
  first; the caller scopes the ledger context `Purpose::Review`; usage recorded like the agentic
  review. New `ProviderConfig::one_shot_provider` = `OpenAiCompatibleProvider::with_transient_only`
  (new in `hermes_llm`): 5xx/network/429 still retried `http_retries` times, a 4xx never.
- **Contract + repair** (`contract.rs`): the object is taken from the whole text, one code fence,
  or first `{`..last `}`; >512 KB, not an object, or `memory`/`skills` not a list = malformed →
  exactly one repair request (packet, the bad reply cut to 4k, `evidence_review_repair(why)`),
  then give up (`Failed`, logged, signals not marked). A 400/422 to the json_schema request →
  the same request once in text, remembered per (base URL, model) for the process. Max 3
  requests (refused schema + text + repair); normally 1.
- **Validation per op**: allowed fields only (unknown → rejected, also `"exec"` at top level is
  reported, not obeyed), `op`/`target` enums, string-or-null types, add/replace/remove field
  rules, memory text/old ≤1000 chars, why ≤500, patch ≤100k (SKILL.md's limit), skill names via
  `skill_manage::validate_skill_name` (new pub wrapper), one op per skill, ≤8 memory / ≤4 skill
  ops. Each refusal is an `OpReport` with its reason and a `tracing::info!` line.
- **Apply** (`apply.rs`, in `spawn_blocking`), one op at a time: memory → `MemoryStore::apply`
  (one-item batch: limits, threat scan); disabled store refused. Skills: create (name must not
  exist; full body, or a diff against "") / patch (must be shown in the packet; SKILL.md must
  still have the fingerprint read for the packet; a unified diff applied strictly by `udiff.rs`,
  or a full body only if shown whole; a no-op refused) → `SkillOp` `create`/`edit` through
  `SkillStore::apply_staged(ops, WriteOrigin::Review, [FileBase])` (base re-checked under the
  write lock, validation, threat scan of the skill, git commit, index hook). Never delete, no
  supporting files. Hold (tainted session → its reason and sources; `require_approval` →
  "review.require_approval is on"; a store's `write_approval` → "write approval is on") →
  `StagingStore::stage_memory_ops` / `stage_skill_ops`, one item per op (same dry-run checks).
- **Strict diff** (`udiff.rs`): header counts must match the body; context/removed lines must
  match exactly (only a trailing `\r` ignored); text before the first hunk or after a complete
  hunk, overlapping/out-of-order hunks → rejected. Like `git apply`, a hunk whose exact lines are
  at another line than stated is applied if they occur exactly once after the previous hunk;
  never fuzz. CRLF and "\ No newline" kept. A model's dropped space on an empty context line is
  read as the empty line.
- Signals marked reviewed (`mark_signals_reviewed(max seq of the packet's input)`) only after a
  reply was read, including an empty or all-refused one.
- Small public additions: `hermes_tools::skill_manage` is now `pub mod`;
  `skill_manage::MAX_SKILL_CONTENT_CHARS` and `validate_skill_name` pub.
- Docs: PORTING (diverged row), ARCHITECTURE (crate row), GAPS (CP7.2), PITFALLS (new section).
Files: crates/runtime/src/{review_once.rs, review_once/apply.rs, review_once/contract.rs,
  review_once/packet.rs, review_once/udiff.rs, review_once/tests.rs, review_once/golden_packet.txt}
  (new), crates/runtime/src/lib.rs, crates/runtime/tests/turns.rs, crates/prompts/src/review.rs,
  crates/llm/src/openai_compatible.rs, crates/tools/src/{lib.rs, skill_manage.rs}, GAPS.md,
  PORTING.md, docs/ARCHITECTURE.md, docs/PITFALLS.md, docs/backend/progress/phase-7.md
Tests (CARGO_TARGET_DIR=target/claude):
- `cargo test -p hermes_runtime --lib`: 125 passed (22 new: packet golden + determinism, budget
  over 11 budgets, priority/explicit-first/memory-keeps-its-tenth, cut/masked not whole, window
  budget, used skills; apply: memory add/replace/remove + skill patch/create one by one with
  commits, threat-scan and limit refusals, disabled store, strict patches (fuzzy diff, unshown
  skill, no-op, existing create, no front matter, threat in body, full body of a cut skill,
  changed-after-read), hold → staged then approvable, `write_approval` → staged; contract:
  extraction, per-op refusals with exact reasons, oversize/too many, strict schema; udiff: 9).
- `cargo test -p hermes_runtime --test turns`: 155 passed / 7 ignored under default, balanced,
  requests and tokens. 13 new: one request applied (purpose `review` 1, main 2, no tools,
  `response_format` strict, packet ≤ budget, signals reviewed, second call 0 requests); no
  signals → 0 requests; empty reply → Done, reviewed; malformed twice → exactly 2 requests,
  Failed, nothing applied, still triggered; repair success → 2; 400 to json_schema → 2 (second
  without `response_format`, text contract) and the next review 1 in text; `ReplyFormat::Text`;
  invalid ops refused individually (7 of 8) with the valid add applied; injection in evidence
  (steered model: threat-scan, unknown op/field, bad remove, evil create all refused; only a
  plain add applied); tainted session → 2 staged items with reason/sources, USER.md and skills
  untouched; `require_approval` → staged; a `skill_view`ed skill: unclean diff refused (file
  unchanged, "shown whole" in packet), clean diff applied next review (2 review requests); aux
  review model → 0 on main, 1 on the aux server, budget = half its window.
- `cargo test -p hermes_prompts` (+1: the prompt's contract, no "Be ACTIVE"), `hermes_tools`,
  `hermes_memory`, `hermes_store`: prompts 23, tools 181 (+1 ignored), memory and store green;
  `hermes-desktop` 174 + 68 (acp); `cargo build --workspace` OK. Clippy: no warnings in the new
  code (`-A clippy::redundant_clone` for the known `hermes_tools` errors).
Request counts: S1–S6 unchanged (nothing schedules the new review yet). New facts, asserted in
  `turns.rs`: an evidence review = 1 `review` request; 2 with the repair; 2 when the server
  refuses json_schema (then 1 for later reviews in the process); at most 3; 0 with no
  unreviewed signals or when the governor defers it.
Deviations from the plan:
- Packet sections have share caps (40/30/20/rest) on top of the priority order, so memory always
  keeps ~10% of the budget (it is what stops duplicate adds); with the default 24k budget nothing
  real is cut.
- The diff applier tolerates wrong line numbers when the exact old lines occur once (git apply's
  offset, never fuzz). Say if you want stated-position-only.
- Memory replace/remove of an untainted session are applied directly (the brief's design);
  Hermes's agentic review stages unattended patches/removals (`unattended_tool`). Injection needs
  untrusted content, which taints the session, which stages everything.
- Fallback on any 400/422 (not only ones naming `response_format`), at most once per review.
For the reviewer:
- Budget proof is "one writer refuses what doesn't fit" + the test; the system prompt (~2.5k
  tokens) is not in the budget (the window/2 cap leaves room for it and the reply).
- `mark_signals_reviewed(max_seq)` also covers signals left out of the packet for budget (the
  weakest ones). Alternative: mark only the shown ones (needs a different store API).
- No max_tokens on the request: a long `create` cut by a provider's default limit is malformed →
  repair → probably cut again → give up.
- Two `review_once` calls on one session at once (or one next to the agentic review) could both
  apply; CP7.3 must own the slot/no-double-review guard.
- The strict json_schema (type arrays with null, every field required) is OpenAI's documented
  shape; not checked against a real OpenRouter/LM Studio/Ollama endpoint (no live check: nothing
  page-facing).
- `ReviewOutcome` is not reported to the page (`bridge.review_finished`); CP7.3 decides.
- Only new files and my hunks were rustfmt'ed; lib.rs, turns.rs, openai_compatible.rs (imports
  under edition 2024) keep their pre-existing rustfmt diffs.
- Mid-session I ran a stray `git stash` in this worktree (refs/stash is shared with the main
  checkout) and popped it at once (`stash@{0}` on backend/p7-review, dropped after a clean pop);
  the stash list is as before. Nothing else touched.

### Review (Opus 5.5)
Main had not moved (branch at 123e118); no merge. `/code-review` not available in the subagent;
reviewed by reading every new file and the diff, thinking as an attacker who controls text that
reaches the packet (pasted pages, project files read without taint, tool output).
Findings and fixes:
- **Budget estimate not conservative** (fixed): bytes/4 counts CJK at 0.75 tokens a character
  (real tokenizers spend ~1-1.5), and code/JSON at ~4 chars a token (real ~3), so a "24k" packet
  could be ~2x that. `packet::estimate_tokens` now counts an ASCII character 1/3 token and other
  characters 1/2 token per UTF-8 byte (CJK 1.5, emoji 2), never less than bytes/4; the writer,
  section shares, skill cuts and memory room all count in these units. New test on a CJK session
  (a CJK character a token + ASCII/4 still fits every budget).
- **Budget ignored the rest of the request** (fixed): the ~2.5-3.5k-token system prompt, the
  schema, the repair turn (quoted reply up to 4k *characters*) and the reply were outside it, and
  `window/2` alone overflowed small windows (4k window: 2k packet + 3k prompt). `budget_for(knob,
  window, reserved)` = min(knob, window/2, window − reserved), `reserved_tokens` = text prompt +
  schema + repair text + `REPAIR_QUOTE_TOKENS` (1000; the repair quote is now cut by the same
  estimator) + reply room (min(16k, window/4)). A window too small gives 0 → `Failed`, no request.
  Unknown window: the 24k knob (documented). Test: packet + reserve ≤ window for 6k-128k windows.
- **No `max_tokens`** (kept, documented): sending a cap a model doesn't accept is a 400, which the
  review would read as a json_schema refusal; the main turns send none either. Reply room is
  reserved in the budget instead.
- **No wall-clock limit** (fixed): a streaming provider that trickles is only bound by the 600 s
  idle timeout; each request now has a 10-minute `tokio::time::timeout` (→ `Failed`).
- **A 400 poisoned the json_schema memory** (fixed): any 400/422 (bad model name, proxy error)
  switched the endpoint to text for the process. Now remembered only when the text request then
  goes through. New turns test (400, 400 → Failed, 2 requests, not reviewed; next review asks for
  json_schema again).
- **Flaky test, pre-existing** (fixed): the json_schema memory is process-global and a later
  test's mock server can get a freed port, so `the_evidence_review_makes_one_request...` failed
  once in a full run (json_schema false). The two tests that make/check the memory now review
  under a model name of their own (`review_as`).
- **Untainted memory replace/remove applied directly** (fixed, deviation reverted): that was
  weaker than the agentic review it replaces (`MemoryStore::unattended_tool`, Hermes's
  `_background_delete_gate`), and taint doesn't cover project files (a README read in the
  workspace reaches excerpts as tool text). Replace/remove now always go to staging ("a
  background review may not change or delete memory entries unattended"); adds and skill writes
  of an untainted session still apply (same power as the main agent's own tools there).
- **Diff applier** (tightened): `---`/`+++` paths were ignored: now at most one pair, naming
  `SKILL.md` (no `..`, no drive), never `+++ /dev/null` (deletion), `--- /dev/null` only for an
  empty file (it used to prepend a whole second copy). A context-free insertion hunk was placed by
  its line number alone (wrong place silently): now refused unless the file is empty. rename /
  binary / deleted-file headers were already refused. The offset leniency (exact lines, unique
  after the previous hunk) is kept: it never fuzzes and an ambiguous match is refused.
- Checked, no change: every op type reaches `MemoryStore::apply` / `stage_memory_ops` or
  `SkillStore::apply_staged` / `stage_skill_ops` (limits, threat scan, frontmatter, size, git
  commit; staged items re-checked on apply); skill names are ASCII `[a-z0-9._-]` (no traversal,
  no lookalikes; a trailing-dot name colliding on Windows is refused by create's non-empty-dir
  check; reserved device names like `con` only fail the write); ops capped 8/4, oversize fields,
  duplicates per skill refused; reply ≤512 KB, serde_json's recursion limit (128) handles deep
  nesting, BOM/fences/trailing prose are tolerated only to find one object; `why` is never stored
  or shown; staging carries the session taint's reason/sources; signals marked only after a read
  reply; old agentic review untouched; no blocking work on async workers; store schema untouched;
  main session prompt untouched; `with_transient_only` is opt-in (no other caller changes);
  `pub mod skill_manage` only newly exposes `fingerprint`, `validate_skill_name` and the size
  constant (everything else was already re-exported).
Left, with reasons:
- Signals left out of the packet for budget are marked reviewed too (the watermark API can't
  express "only the shown ones"; with 32 capped signals only the weakest beyond ~40% of 24k drop).
- For CP7.3: one review per session at a time (also vs the agentic review; two at once can both
  apply or stage the same lesson), take the evidence and `max_seq` at the start so signals added
  meanwhile stay unreviewed (already so), a `Failed` review with 0 requests (window too small,
  budget 0) must not be retried every idle period, decide whether `ReviewOutcome` reaches the page,
  and the final prompt wording.
Tests (CARGO_TARGET_DIR=target/claude): `hermes_runtime` lib 128 (3 new: CJK estimate, reserve fits the window, diff headers; insertion test reworked), `--test turns` 156 passed / 7 ignored under default, balanced, requests, tokens (1 new: a 400 the text request gets too); `hermes_tools` 181, `hermes_memory`, `hermes_store`, `hermes_prompts` 23, `hermes_llm` green; `hermes-desktop` 174 + 68 (acp); `cargo build --workspace` OK.
