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
