# Backend v2, phase by phase

The plan that comes out of the backend review of 2026-11-11: make the agent **request-frugal**
(the user's provider allows ~800 requests/day but unlimited tokens), make self-improvement
(skills, memory) **signal-driven and cheap**, close the **security** holes, and clean up the
**Rust architecture**. From here the backend may deviate from Hermes wherever that helps (see
Phase 0); the page's protocol stays what Hermes's frontend expects.

Each phase ends in something the user can try, with its tests green and its docs true. Phases are
split into **checkpoints**: Sonnet (or Opus, where marked) implements up to a checkpoint and stops
without committing; a separate, fresh Opus reviewer reviews, fixes and commits. Since 2026-11-12 one
orchestrator session starts both as subagents (the `checkpoint` skill). Don't start a phase before
its dependencies are merged into `main`, and don't start a checkpoint before the previous one is
committed. See *How the work is run* for the sessions, lanes and briefs. Within a checkpoint, do
the steps in order.

Before Phase 0, read: `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/TESTING.md`,
`docs/PITFALLS.md`, and from the code: `crates/runtime/src/lib.rs` (`Runtime::begin_turn`, the
step list), `crates/runtime/src/review.rs`, `crates/tools/src/lib.rs` (`dispatch`,
`check_command`, `check_edit`), `crates/llm/src/api_client.rs`, `crates/llm/src/retry.rs`.

Rules that matter most here:
- **The store schema stays frozen** (ground rule 6). New persistent data goes in side files or in
  *separate* SQLite files (`index.db`, `memory.db`), never new tables in `sessions.db`.
- **Prompt caching** (ground rule 8). Anything retrieved per turn goes into the **user message**
  (as a context block), never into the system prompt. The system prompt and the tool list are
  fixed for the life of a session.
- **Never block the async workers** (ground rule 7).
- **Every phase that changes request counts adds a budget assertion** to
  `crates/runtime/tests/turns.rs` (Phase 0 builds the harness). The numbers are the spec.
- **Don't touch the user's data.** Tests set `HERMES_RS_HOME` to a temp folder; build into
  `CARGO_TARGET_DIR=target/claude`; live checks run on `scripts/perf/perf.ps1` (port 47810).

Sizes: S ≈ a day, M ≈ 2–4 days, L ≈ a week or more (agent-days, rough).

---

## Phase map

| # | Phase | Size | Depends on | Coding | Lane |
|---|---|---|---|---|---|
| 0 | Groundwork: rules, request-counting test harness | S | — | Sonnet | A |
| 1 | Path policy, env scrub, skill-script flag — ✅ 2026-11-12 | S–M | — | Sonnet | B |
| 2 | Request ledger — ✅ 2026-11-12 | M | 0 | Sonnet | A |
| 3 | Budget profiles and the knobs they set — ✅ 2026-11-12 | M | 2 | Sonnet | A |
| 4 | Side jobs off the main quota — ✅ 2026-11-12 | S–M | 3 | Sonnet | A |
| 5 | Local index (FTS5) and per-turn pre-injection — ✅ 2026-11-12 | M | 3 | Sonnet | A |
| 6 | Taint tracking and the staging area — ✅ 2026-11-12 | M | 1 | Sonnet | B |
| 7 | Review rewrite: signals, evidence packet, one shot — ✅ 2026-11-13 | M–L | 5, 6 | **Opus** | C |
| 8 | Skills v2: outcomes, provenance, project skills, curator v2 — ✅ 2026-11-13 | M–L | 7 | Sonnet | B |
| 9 | Memory v2: tiers, structured entries, links — ✅ 2026-11-13 | L | 5, 7 | Sonnet (CP9.1 **Opus**) | B |
| 10 | Fewer round trips: script tool, first-turn context, batching — ✅ 2026-11-14 | M | 3 | Sonnet | A |
| 11 | Token mode: deferred tool schemas | S–M | 3 | Sonnet | A |
| 12 | Provider fallback chain | M | 2 | Sonnet | A |
| 13a/c/f | Perf: `Arc<Message>`, `spawn_blocking`, typed dispatch | M | 0 | Sonnet | filler |
| 13b | Perf: store writer actor | M | 0 | **Opus** | C |
| 13d/e | Perf: session actor, explicit agent loop | L | all others | **Opus** | last, alone |
| 14 | Trim `hermes_llm` | S–M | 2, 12 | Sonnet | filler |
| 15 | OS sandbox for the shell | L | 1, 6 | **Opus** | C |

Phase 1 is independent of the request work and can start while Phase 0 is in review. Lanes and
fillers are explained in the next section.

---

## How the work is run: models, checkpoints, sessions

### Orchestrator and subagents (the default since 2026-11-12)

One **orchestrator** session (Sonnet 5.5, in the main checkout) runs the work through the project
skill [`.claude/skills/checkpoint/SKILL.md`](../../.claude/skills/checkpoint/SKILL.md). It writes no
product code: it checks the gates (dependencies merged, previous checkpoint reviewed), creates the
phase's worktree `..\hermes-rs-p<N>` on `backend/p<N>-<slug>`, starts the implementer and then the
reviewer as **subagents** with the model each role needs, reads their reports, and asks the user
before merging into `main`. The roles, the checkpoint protocol and the lanes below are unchanged;
only who starts the sessions changed. The skill holds the exact briefs, so the orchestrator never
improvises them. Two differences from separate sessions:

- The reviewer can't ask the user anything while it runs, so at a phase's last checkpoint it stops
  after its commit; the **orchestrator** asks about the merge and does it (local, never pushed).
- `/code-review high` may not be available inside a subagent; the reviewer brief has the manual
  review pass that replaces it (and runs the skill too when it is available).

The manual way (a session per role, started by hand with the briefs at the end of this section)
still works, e.g. to steer an implementer live.

### Roles

- **Implementer, Sonnet 5.5**: the default for coding. A fresh session or subagent per checkpoint
  (or per phase, continued across its checkpoints), in **its own git worktree** on branch
  `backend/p<N>-<slug>`. It works up to the next checkpoint and then **stops without committing**.
- **Reviewer, Opus 5.5**: a fresh session or subagent **in the implementer's worktree**, started
  after the implementer has stopped. It reviews the uncommitted diff, fixes what it finds, runs the
  tests and commits. It adds no scope beyond the checkpoint.
- **Implementer, Opus 5.5**: for the work marked **Opus** (design-heavy, concurrency, unsafe
  Win32, data migration): Phases 7, 13b, 13d, 13e, 15 and checkpoint CP9.1. Its checkpoints still
  go to a *separate*, fresh Opus reviewer (fresh eyes, not the one that wrote the code).

Why fresh reviewers: the prompt cache is per model, so switching a coding session to Opus pays for
the whole coding history again; a fresh reviewer starts from the plan, the progress note and the
diff, and hasn't absorbed the implementer's assumptions.

### Checkpoint protocol

**The implementer, at a checkpoint:**
1. The checkpoint's tests pass (listed per checkpoint below); `cargo build --workspace` passes when
   shared crates changed. Always `CARGO_TARGET_DIR=target/claude`.
2. Writes or extends `docs/backend/progress/phase-<N>.md` (one file per phase, so parallel sessions
   never edit the same note):
   ```markdown
   ## CP<N>.<x> — <title>
   Worktree: <path>   Branch: backend/p<N>-<slug>   Model: Sonnet 5.5
   Done: <what, in a few bullets>
   Files: <changed files>
   Tests: <commands run, results>
   Request counts: <changed baseline numbers, if any>
   Deviations from the plan: <what and why, or "none">
   For the reviewer: <doubts, risky spots, things not verified>
   ```
3. Stops. No commit, and it doesn't start the next checkpoint.

**The reviewer:**
1. Reads AGENTS.md, this plan's phase section, the progress note, then the diff (`git status`,
   `git diff`, and untracked files).
2. If `main` has moved since the branch was cut, merges `main` into the branch first and resolves
   conflicts.
3. Runs `/code-review high` on the diff and checks the checkpoint's **review focus** (listed per
   checkpoint below) by reading the code.
4. Fixes what it finds, re-runs the checkpoint's tests, and appends to the progress note:
   `### Review (Opus 5.5)`: findings, what was fixed, what was left and why.
5. Commits on the phase branch: `backend p<N> cp<x>: <summary>` with the session's
   `Co-Authored-By:` trailer.
6. At the phase's **last** checkpoint, also: updates the docs per ground rule 10, marks the phase
   ✅ with the date in the phase map, and asks the user before merging the branch into `main`
   (local merge only; never push).

The next checkpoint of the same phase starts only after the reviewer's commit: either a new
implementer session, or the same one told "CP reviewed and committed, continue with CP<N>.<x+1>"
(the fixes are in its worktree already).

### Running two or three sessions at once

The implementer **alternates between two lanes**, so that while Opus reviews one lane's
checkpoint, Sonnet works on the other lane's in another worktree (ping-pong):

- **Lane A (requests):** 0 → 2 → 3 → 4 → 5 → 10 → 11 → 12
- **Lane B (security, self-improvement):** 1 → 6 → 8 → 9
- **Lane C (Opus implementer, the optional third session):** 15 (after 1 and 6 are merged) →
  7 (after 5 and 6) → 13b
- **Fillers** (Sonnet, when a lane waits for a dependency): 13a, 13c, 13f, 14
- **Last, alone:** 13d, 13e (Opus). They rewrite the runtime plumbing that every other phase
  touches, so run them when no other phase branch is open.

Rules:
- Never two sessions writing in the same worktree; the reviewer enters only after the implementer
  has stopped.
- A phase branch is cut from `main` after all of its dependencies are merged.
- Two open branches may touch the same file only if they're in different lanes; the reviewer of
  the branch that merges second resolves the conflicts.

The start of the schedule (then continue the same pattern):

| Round | Sonnet implements | Opus reviews (then commits) | Opus implements |
|---|---|---|---|
| 1 | P0 CP0.1 | — | — |
| 2 | P1 CP1.1 | P0 CP0.1 → merge | — |
| 3 | P2 CP2.1 | P1 CP1.1 | — |
| 4 | P1 CP1.2 | P2 CP2.1 | — |
| 5 | P2 CP2.2 | P1 CP1.2 → merge | — |
| 6 | P6 CP6.1 | P2 CP2.2 | — |
| 7 | P2 CP2.3 | P6 CP6.1 | — |
| 8 | P6 CP6.2 | P2 CP2.3 → merge | — |
| 9 | P3 CP3.1 | P6 CP6.2 | — |
| 10 | P6 CP6.3 | P3 CP3.1 | — |
| 11 | P3 CP3.2 | P6 CP6.3 → merge | P15 CP15.1 |
| 12 | P13c (filler) | P3 CP3.2 → merge | P15 CP15.2 |

### Session briefs (manual way)

With the orchestrator, ask it in plain words: `run CP3.1` (implement, then review),
`implement CP3.1`, `review CP3.1`, `continue CP3.2`, `round 11` (the schedule's parallel round),
`merge phase 3`, `status`. The skill's full briefs are what it sends.

By hand: start the implementer as a **worktree session on Sonnet 5.5**, and the reviewer as a
session **in that worktree's folder on Opus 5.5** (the path is in the progress note), with these
short briefs.

Implementer:
```
Implement Phase <N>, checkpoint CP<N>.<x> only, of docs/backend/PHASES.md. Read AGENTS.md
whole, then the plan's intro, "How the work is run" and Phase <N>, and
docs/backend/progress/phase-<N>.md if it exists. Work in this worktree on branch
backend/p<N>-<slug>. Stop at the checkpoint: its tests pass, the progress note is written,
nothing is committed.
```

Reviewer:
```
Review checkpoint CP<N>.<x> of docs/backend/PHASES.md in this worktree. Read AGENTS.md whole,
the plan's "How the work is run" and Phase <N>, docs/backend/progress/phase-<N>.md, then the
uncommitted diff. Follow the reviewer steps of the checkpoint protocol: /code-review high,
the checkpoint's review focus, fix, re-run its tests, append the Review section, commit.
If this is the phase's last checkpoint, update the docs, mark the phase done, and ask me
before merging into main.
```

---

## Phase 0 — Groundwork

**Coding: Sonnet.** Checkpoints:
- **CP0.1** (steps 1–4, all of it). Tests: `turns`. Review focus: the baselines record *today's*
  real counts (no behaviour changed to make numbers nicer); the mock's counter is robust to
  parallel requests; the rule 1 wording keeps the protocol rule strict.

**Goal:** the rules allow deviating from Hermes, and tests can count model requests.

1. **AGENTS.md rule 1.** Replace "Port Hermes, don't invent" with two rules:
   - *Protocol:* the page's protocol (JSON-RPC methods, REST routes, event shapes) stays what
     Hermes's frontend expects; port it from Hermes as before.
   - *Backend:* the agent's behaviour (loop, tools, review, curator, memory, prompts) is ours. When
     we deviate from Hermes, the module doc says so and why, and the `PORTING.md` row is marked
     `diverged` (so a Hermes update knows not to re-port it).
   Update `docs/WORKFLOW.md` to match. Add a link to this file in AGENTS.md's "Read next" table.
2. **Request-counting mock.** The mock HTTP provider used by `turns.rs` already serves
   chat/completions; give it a shared counter of requests, split by a header we send on every
   request: `x-hermes-rs-purpose: main|review|curator|title|approval|compress|delegate|btw`
   (added in Phase 2; until then everything counts as `main`). Helper:
   `assert_requests(&mock, Purpose::Main, ..=n)`.
3. **Baseline scenarios** in `turns.rs`, each with today's request count written down as the first
   assertion (exact numbers, so later phases show their savings):
   - S1 "read three files, edit one": scripted mock replies with 4 tool rounds.
   - S2 "empty reply then answer": exercises `EmptyResponseRecovery`.
   - S3 "truncated tool call": exercises the length/truncation retries.
   - S4 "ten-turn session": triggers the memory and skill review nudges.
   - S5 "delegate two children".
4. Record the baseline numbers in a table in this file (section *Baselines* at the end).

**Tests:** `cargo test -p hermes_runtime --test turns`.
**Done when:** the five scenarios pass with their baseline counts, AGENTS.md/WORKFLOW.md say the
new rule.

---

## Phase 1 — Path policy, env scrub, skill-script flag

**Coding: Sonnet.** Checkpoints:
- **CP1.1** (steps 1–2: `path_policy.rs`, `check_edit`). Tests: `hermes_tools` unit tests,
  `turns`. Review focus: canonicalisation can't be escaped (`..`, junctions, `\\?\`, 8.3 short
  names, case, trailing dots/spaces); every file-writing tool goes through `check_edit`
  (`write_file`, `patch`, and checkpoint restore if it writes outside the workspace; MCP
  filesystem servers are out of scope, say so in the module doc).
- **CP1.2** (steps 3–6: terminal paths, env scrub, skill scripts, docs, live check). Tests:
  `hermes_tools`, `turns`, `path-policy-check.mjs`. Review focus: the env scrub doesn't drop
  `PATH`/`USERPROFILE`/`SystemRoot` and similar; MCP servers keep their configured env; the
  bundled-skill hash check can't be fooled by editing the extracted copy.

**Goal:** the file tools can't silently write outside the places they should, and the shell
doesn't see secrets.

**Design.** One module, `crates/tools/src/path_policy.rs`, used by **both** the file tools
(`check_edit`) and the terminal guard (`approval.rs`, replacing the `APP_CONFIG_PATH` special
case). It classifies a resolved path (use `paths::resolve` + canonicalize the existing prefix, so
`..` and junctions can't escape):

| Class | Paths | Write via file tools |
|---|---|---|
| `Workspace` | session cwd and below; scratch dir | allowed (current behaviour in AcceptEdits/Auto) |
| `AppData` | `data_dir` (`HERMES_RS_HOME`): configs, `sessions.db`, approvals | **denied**, with a message pointing to the right tool |
| `SelfImprove` | `data_dir/skills`, `data_dir/memories` | denied; must go through `skill_manage` / `memory` (threat scan, staging) |
| `Persistence` | Startup folders, `$PROFILE` files, `.git/hooks`, `~/.ssh`, `~/.gitconfig`, scheduled-task dirs, the PowerShell/bash rc files | **always ask**, even in Yolo; never smart-approved |
| `Outside` | everything else | ask (AcceptEdits/Auto), allowed (Yolo) |

Steps:
1. `path_policy.rs`: `enum PathClass`, `fn classify(path, ctx) -> PathClass`, `ctx` holds cwd,
   scratch, data_dir, home. Unit tests for each class, including `..`, MSYS paths, a junction in a
   temp dir, case differences (`C:\USERS\...`).
2. `check_edit` (`tools/src/lib.rs:580`): call `classify` for every mode, not only Plan/Manual.
   Deny/ask per the table. `patch` with several files checks each.
3. Terminal (`approval.rs`): when a command's text names a path (simple extraction: quoted or
   bare tokens that resolve to an existing file or a `Persistence`/`AppData` location), apply the
   same classes. This is defence in depth only; Phase 15 is the real boundary.
4. **Env scrub** (`shell.rs`, `shell_env`): build the child env from the parent minus names
   matching `(?i)(KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTH)` and the explicit
   `hermes_rs_or_api_key`, plus an allowlist setting (`terminal.env_passthrough: []`) for names
   the user wants kept. Use `Command::env_clear()` + the filtered set. Same for MCP stdio servers
   only if they have no explicit env config (they often need their key: keep their configured env).
5. **Skill scripts:** a terminal command that runs a file under `data_dir/skills/*/scripts/`
   (agent-writable) is a *flagged* command (goes through approval like the dangerous patterns),
   with the description "runs a script the agent wrote into a skill". Bundled skills (embedded,
   extracted) are exempt if their hash matches the embedded copy.
6. Docs: `docs/PITFALLS.md` (path policy surprises), `GAPS.md` row.

**Tests:** unit tests above; a `turns.rs` scenario where the mock model calls `write_file` on
`data_dir/desktop-config.json` → tool result is the denial, file unchanged; env test: spawn the
shell with `FOO_TOKEN=x` set and check `env` output lacks it.
**Live check:** `scripts/perf/path-policy-check.mjs`: on the perf instance, mock provider asks for
a write into `HERMES_RS_HOME`; expect the denial text in the transcript.
**User checklist:** in your app, ask the agent to "write hello to %APPDATA%\hermes-rs\x.txt":
it should refuse; ask it to edit a file in the repo: works as before.

---

## Phase 2 — Request ledger

**Coding: Sonnet.** Checkpoints:
- **CP2.1** (observer trait, purpose task-local, ledger actor, side file). Tests: `hermes_llm`,
  ledger unit tests. Review focus: retries and streaming requests counted exactly once per HTTP
  attempt; the hot path takes no lock and does no I/O; the task-local is set for spawned tasks
  too (task-locals don't cross `tokio::spawn`).
- **CP2.2** (every caller's purpose, governor, `requests.status`). Tests: `turns` (S1–S5 now per
  purpose), governor tests. Review focus: no caller left unlabelled (grep every provider call);
  the governor can't block the main turn except at 0.
- **CP2.3** (page display, ACP usage, docs, live check). Tests: `hermes-desktop`,
  `requests-check.mjs`. Review focus: the patch/plugin is minimal and documented.

**Goal:** every request to a provider is counted, attributed and visible; the app knows how much
of today's quota is left.

**Design.**
- Count at the lowest level: `ApiClient` (`crates/llm/src/api_client.rs`), where every HTTP
  attempt goes out, **including retries** (`retry.rs`) and streaming requests. Add a
  `RequestObserver` trait object on `ApiClient` (`fn on_request(&self, meta: RequestMeta)`,
  `fn on_response(&self, meta, status, usage)`). `RequestMeta`: provider id, model, purpose,
  session id, attempt number.
- **Purpose** travels with the call: a `task_local!` (`CURRENT_PURPOSE`) set by each caller
  (main turn, review, curator, title, approval judge, compression, delegate child, `/btw`), read by
  the observer. Also sent as the `x-hermes-rs-purpose` header (only to the mock/loopback hosts,
  never to real providers: it's for tests).
- **Storage:** `data_dir/requests.json` side file (schema frozen) holding, per provider, a rolling
  map `day (provider's reset timezone, default UTC) → { purpose → count, 429s }` for the last 31
  days. Written through a small actor (one tokio task, debounced flush every 2 s and on exit) so
  the hot path is an atomic increment.
- **Quota config:** per provider in the provider settings: `daily_request_limit: Option<u32>`,
  `reset_tz` (default UTC). Learn it too: when the provider returns a 429 whose body says
  "N of N" (the shape `transcript.rs` already recognises), store N if unset.
- **Governor** (`crates/runtime/src/governor.rs`): `fn allow(purpose) -> Allow | Deny(reason)`.
  Defaults (all profiles): below 20% left, background purposes (review, curator, title) are denied (deferred);
  below 5%, delegate is denied and the main turn gets a warning event; at 0, main turns are refused
  with a clear message (unless a fallback provider exists, Phase 12).
- **Surface:**
  - A gateway method `requests.status` → `{ provider, used, limit, resets_at, by_purpose }`.
  - The page: Hermes's status bar shows usage from the existing usage events; first check whether
    an existing field can carry "312/800" (look at what `session.usage` / the status bar reads in
    `target/hermes-src`). If not, a small patch in `crates/desktop/patches/` that renders it.
  - ACP: include it in the usage update we already send, if the shape allows; else skip.
  - Per turn: add the request count to the turn's usage record in the transcript metadata
    (side file per session if the store row has no room).

Steps: observer trait + purpose task-local (llm) → ledger actor + side file (runtime) → wire
every caller's purpose → governor checks at each background job start and at delegate →
gateway method → page display → docs.

**Tests:** unit tests for the ledger (day rollover across timezones, 31-day pruning, debounced
flush); `turns.rs`: S1–S5 now assert per-purpose counts; a retry scenario asserts retries are
counted; governor test: at 19% left, the review does not run.
**Live check:** `requests-check.mjs`: run two turns on the perf instance, call `requests.status`,
compare with the mock's own count.
**User checklist:** status bar shows used/limit; it goes up by the number of model calls in a
turn; Settings → provider has a daily limit field.

---

## Phase 3 — Budget profiles

**Coding: Sonnet.** Checkpoints:
- **CP3.1** (steps 1–2: `budget.rs`, constants replaced by knobs, `balanced` profile still the
  effective one). Tests: `turns` under all profiles. Review focus: `balanced` is byte-for-byte
  today's behaviour; no constant left behind; knobs read once per turn.
- **CP3.2** (steps 3–4: config, UI, default `requests`, profile-dependent prompt text). Tests:
  `turns`, `hermes-desktop`. Review focus: prompt text chosen at session start only (caching);
  configs without the key get `requests`.

**Goal:** one setting, `budget.profile = requests | balanced | tokens`, sets a bundle of knobs.
**The default is `requests`** (decision D1), for new installs and for existing configs without the
key. `balanced` = today's behaviour, kept as the comparison point and for anyone who switches
back. Each knob can still be overridden individually.

Because the default changes behaviour, knobs whose `requests` value needs a later phase (review
`idle_once`, skills preload, heuristic titles, curator per cluster) fall back to their `balanced`
value until that phase lands; the release notes of each phase say what changed.

**Design.** `crates/runtime/src/budget.rs`: `struct BudgetKnobs` with every knob below, `fn
for_profile(p) -> BudgetKnobs`, merged with per-knob overrides from config. `RuntimeConfig` gets a
`budget: BudgetKnobs` (replacing scattered constants). Read once per **turn** (settings apply from
the next turn; anything that changes the prompt or tool list applies from the next **session**).

| Knob | Today (balanced) | requests | tokens | Where it's used now |
|---|---|---|---|---|
| `max_iterations` | 500 | 150 | 500 | `RuntimeConfig` |
| `empty_response_retries` | 3 | 1 | 3 | `inference.rs:78` |
| `length_continuations` | 4 | 2 | 4 | `ops.rs` |
| `invalid_tool_retries` | 3 | 2 | 3 | `ops.rs` |
| `truncated_tool_retries` | 4 | 2 | 4 | `ops.rs` |
| `http_retries` | 3 | 3 (429-daily: 0) | 3 | `retry.rs` |
| `compress_at` (<512k / ≥512k) | 75% / 50% | 90% / 85% | 60% / 40% | `compress.rs` |
| `result_size_limit` | current | ×4 | ×0.5 | `ops.rs` |
| `read_file default lines` | current | whole file up to 4k lines | current | `files.rs` |
| `search context lines` | current | ×2 | current | `search.rs` |
| `delegate.max_children` / `child_iterations` | 10 / 250 | 3 / 60 | 10 / 250 | `delegate.rs` |
| `review.mode` | nudges (today) | `idle_once` (Phase 7) | nudges | `review.rs` |
| `curator.max_iterations` | 200 | one-shot per cluster (Phase 8) | 200 | `curator.rs` |
| `skills.index` | full | categories + retrieval (Phase 5) | names only | `skills.rs` |
| `skills.preload` (Phase 5) | off | top 3 | off | `preinject.rs` |
| `memory.preload` (Phase 5) | off | top 3 | off | `preinject.rs` |
| `titles` | model | heuristic (Phase 4) | model | `lib.rs:223` |
| `tool_schemas` (Phase 11) | all | all | deferred | `prompts/tools` |

Steps:
1. `budget.rs` with the table as code; `BudgetProfile::default() == Requests`; unit test that
   `balanced` equals today's constants (so "switch back to today" stays exact).
2. Replace each constant at its use site with the knob (thread `&BudgetKnobs` through the step
   constructors in `begin_turn`).
3. Config key + gateway `config.get/set` support; Settings UI: if Hermes's settings page has a
   free-form or select field we can reuse via `plugin.js`, add "Budget profile" there; else a
   small patch. ACP: expose as a session config option (Zed's mode menu-like selector) if cheap.
4. Prompt text that depends on the profile (e.g. "Err on the side of loading") is chosen at
   session start and frozen with the system prompt.

**Tests:** S1–S5 run under all three profiles; `requests` assertions are lower than baseline for
S2, S3, S5. **User checklist:** switch profile; next turn behaves per the table (e.g. in
`requests`, the agent reads whole files).

---

## Phase 4 — Side jobs off the main quota

**Coding: Sonnet.** Checkpoints:
- **CP4.1** (steps 2–4: heuristic titles, smart approval without main-model fallback, review not
  cancelled). Tests: `turns` (S4, slow-mock review scenario). Review focus: a review that keeps
  running while a turn runs can't race the turn on the store or the session state.
- **CP4.2** (steps 1, 5: one-click local aux models, compression via aux). Tests: `turns`,
  `hermes-desktop`. Review focus: falls back cleanly when the local server is down.

**Goal:** titles, approval judging, compression summaries, `/btw` and review never spend the
main provider's requests unless the user wants them to.

1. **Aux model defaults.** `desktop/src/aux_models.rs`: a one-click "Use a local model for
   background jobs" in Settings that sets every auxiliary slot to a chosen local provider
   (Ollama/LM Studio, detected via `discovery.rs`). In the `requests` profile, if no aux model is
   set, background jobs that *can* run without a model use their heuristic (below), and those that
   can't (review, curator) wait for idle and go through the governor.
2. **Heuristic titles.** `fn heuristic_title(opening) -> String`: first sentence, strip
   filler/@refs/code blocks, cap 6 words, title case. Used when profile = requests and no aux model.
   Keep the model title as an upgrade path if an aux model appears later.
3. **Smart approval.** `lib.rs:965`: in `requests`, no fallback to the main model; without an aux
   model, a flagged command just asks the user.
4. **Don't cancel the review on a new turn** (`lib.rs:982-985`). Change to: a turn starting while a
   review runs **pauses** it if the review hasn't made its first request yet, otherwise lets it
   finish (it writes only memory/skills, which the turn can't see until next session anyway due to
   the frozen prompt). This alone stops the wasted requests. (Phase 7 replaces the review entirely,
   but this fix is small and lands first.)
5. Compression: in `requests` with an aux model set, use it for the summary (it's the one
   main-model side call that grows with session length).

**Tests:** S4 under `requests`: zero `title` and `approval` requests on the main provider; review
not cancelled when a turn starts mid-review (scenario with a slow mock).
**User checklist:** with "local model for background jobs" on, the request counter only moves on
main turns.

**Done 2026-11-12** ([progress](progress/phase-4.md)). Built as planned, with these notes for later
phases and for testing:
- Under `requests` (the default) with no aux title model, titles are heuristic and **no** model
  title is requested, and a heuristic title is not upgraded later. A live check or test that expects
  a model-written title must set `budget.profile = balanced` or an aux title model (the ACP tests
  write `budget.profile`; of `scripts/perf/*-check.mjs`, only `aux-models-check.mjs` asserts a model
  title, and it picks an aux title model, so none needed changing).
- Compression via an aux model falls back to the session's model when the aux server is down,
  errors, returns nothing or doesn't answer within 300 s (`compress::AUX_SUMMARY_DEADLINE`); the aux
  provider retries once at most. The fallback is logged, not shown in the chat.
- The one-click route `GET/POST /api/model/auxiliary/local` is ours (not Hermes's contract); the
  page reaches it through patch 0023, which is not yet built into a webapp.

---

## Phase 5 — Local index (FTS5) and per-turn pre-injection

**Coding: Sonnet.** Checkpoints:
- **CP5.1** (index file, incremental rebuild, query, retrieval goldens). Tests: index unit tests
  and goldens. Review focus: all SQLite work in `spawn_blocking`; FTS query escaping (user text
  with quotes, `*`, `NEAR`); rebuild cost on a large skills folder.
- **CP5.2** (injection into the user message, display/storage, `index_prompt` variants, profile
  wiring, live check). Tests: `turns`, `preinject-check.mjs`. Review focus: **caching** —
  nothing per-turn reaches the system prompt; resume/rewind reproduce the exact stored block; the
  page shows the user's text without the block.

**Goal:** the model gets the skills and memories that matter for this message *without asking for
them*, at zero requests; the system prompt no longer lists every skill.

**Design.**
- `crates/index` (new crate) or `crates/tools/src/index.rs`: a separate SQLite file
  `data_dir/index.db` with FTS5 tables:
  - `skill_doc(name, category, description, body, path, mtime, hash)` built from SKILL.md files
    (bundled + user + project skills, Phase 8).
  - `memory_doc(id, tier, scope, text, tags)` (filled from MEMORY.md/USER.md now, from `memory.db`
    in Phase 9).
  Rebuilt incrementally on startup (mtime/hash) and after every `skill_manage`/`memory` write.
  All access through `spawn_blocking`. BM25 ranking, `porter unicode61` tokenizer.
- **Query** at turn start: the user's message + the last assistant message's first 500 chars +
  the cwd's project name. Top-k skills with a score threshold (k = `skills.preload`), top-k memory
  entries.
- **Injection**: a `<context>` block **prepended to the user message** (not the system prompt):
  ```
  <relevant-skills>  (full SKILL.md bodies, up to N tokens)
  <relevant-memory>  (entries with their ids)
  ```
  The same block is stored with the message (so resume/rewind reproduce it exactly and caching
  holds). Hidden in the page: keep the message's display text separate (check how `@file`
  references are stored today in `references.rs` and do the same).
- **Skills index in the system prompt** (`skills.rs:392 index_prompt`): per profile:
  `full` (today), `categories` (category names + counts + "relevant skills are attached to the
  user's message; use skill_view for others"), `names` (one line per skill, no descriptions).
  Drop the "Err on the side of loading" paragraph from `categories` and `names`.
- A skill already injected this session isn't injected again (track names per session in the
  session side file); the model is told "already provided earlier".

Steps: index crate + rebuild → query function + tests on a fixture of the 37 bundled skills
(golden: "write a git commit" → git skill top-1, etc.) → injection in `begin_turn` (a new step
before inference, or where `@references` expand) → index_prompt variants → profile wiring.

**Tests:** retrieval goldens (10–15 queries); `turns.rs` S1 under `requests`: the mock asserts the
skill body is in the first request and `skill_view` is never called; request count lower than
baseline. **Live check:** `preinject-check.mjs`. **User checklist:** ask for something a skill
covers; the transcript shows no `skill_view` call and the skill is followed.

---

## Phase 6 — Taint tracking and the staging area

**Coding: Sonnet.** Checkpoints:
- **CP6.1** (taint sources and propagation, approval effects incl. the Yolo warning). Tests:
  `hermes_tools`, `turns`. Review focus: every untrusted source sets taint (web, `@url`, MCP,
  outside reads, delegated children's results); taint survives compression and resume.
- **CP6.2** (staging files, `staging.*` methods, memory/skill writes staged). Tests: `turns`,
  `hermes-desktop`. Review focus: applying a staged item re-runs the threat scan and limits
  (state may have changed since staging).
- **CP6.3** (Pending in the existing memory and skills screens, live check, docs). Tests:
  `hermes-desktop`, a live check. Review focus: patch size; user checklist written.

**Goal:** after the agent has read untrusted content, risky actions ask the user and
self-improvement writes wait for approval.

**Design.**
- **Taint sources** (set a per-turn flag, carried for the rest of the session at a lower level):
  `web_search`/`web_extract`/browser results, `@url` references, MCP tool results (configurable
  per server: `trusted: true` exempts it), `read_file`/search results from paths classified
  `Outside` (Phase 1), images from the web.
- `struct Taint { turn: bool, session: bool, sources: Vec<String> }` on the session, set by
  `GuardedTools` after a tainting tool returns.
- **Effects while tainted:**
  - Flagged commands: always ask, never smart-approve (Manual, AcceptEdits, Auto).
  - `Outside` writes: ask (AcceptEdits, Auto).
  - **Yolo: never blocks** (decision D2). The command or write runs, and the transcript shows a
    one-line warning event ("ran after reading untrusted content: <source>"), also logged.
  - `memory` and `skill_manage` writes → **staged**, in every mode including Yolo (staging
    doesn't interrupt the turn; it only waits for the user later).
- **Staging area:** `data_dir/staging/{id}.json`: `{ kind: memory|skill, op, diff, reason,
  session_id, sources, created }`. Gateway methods `staging.list`, `staging.apply(id)`,
  `staging.reject(id)`. The tool returns "Saved for review by the user" to the model.
  UI (decision D3): **in the existing screens**, no new panel. Pending memory items show at the
  top of the memory screen, pending skill items (new skills, patches, curator merges) at the top
  of the skills screen, each with its diff and Apply / Reject. First look at how those screens
  render in `target/hermes-src` and whether `plugin.js` can insert a section there; if not, one
  small patch per screen in `crates/desktop/patches/`. Phase 7 and 8 reuse the same staging.
- The approval prompt the user sees says *why* ("this turn read a web page").

**Tests:** unit tests for taint propagation; `turns.rs`: web result then `memory add` → staged,
MEMORY.md unchanged; flagged command after web read in Auto mode → asks; same in Yolo → runs,
warning event emitted.
**User checklist:** after a web search, ask the agent to remember something → it appears in
Pending, not in memory; approve it → it's in MEMORY.md next session.

---

## Phase 7 — Review rewrite

**Coding: Opus** (the prompt and the evidence design decide whether self-improvement works at all;
the scheduling interacts with live turns). Checkpoints:
- **CP7.1** (`signals.rs`: detection, weights, evidence excerpts, persistence). Tests: signal
  unit tests on fixtures. Review focus: false positives on ordinary messages ("no problem",
  quoted code); score survives resume.
- **CP7.2** (evidence packet, one-shot request, JSON contract + repair, deterministic apply,
  staging when tainted). Tests: packet golden, `turns` (malformed JSON, apply paths). Review
  focus: the packet stays within a token budget; apply can't bypass threat scan or limits.
- **CP7.3** (idle/close scheduling, `idle_once` vs `nudges`, prompt rewrite, `agentic` kept behind
  a setting). Tests: `turns` S4 under `requests` (exactly 1 review request), no-signal session (0).
  Review focus: an app exit during a review loses nothing important; no double review.

**Goal:** the background review runs when something worth learning happened, costs 1–2
requests, never gets cancelled, and its output is applied deterministically.

**Design.**
1. **Signals** (`crates/runtime/src/signals.rs`), computed locally after each turn, each with a
   weight and the message excerpts as evidence:
   - user correction phrases ("no,", "don't", "instead", "wrong", "always", "never", "remember",
     "from now on", "I prefer") — regex + position (start of the user's message weighs more);
   - `/undo`, rewind, `/retry` of a turn (strong);
   - a denied approval (what was denied, the user's reason if typed);
   - the same tool failing ≥2 times then succeeding with different args (a learned workaround);
   - a long turn (≥15 tool calls) that ended successfully (a procedure worth a skill);
   - an explicit "remember this" or `/learn` (always triggers).
   Score accumulates per session; stored in the session side file.
2. **When:** `review.mode = idle_once` (requests profile): run once when the session has been idle
   for N minutes (default 10) or is closed/switched away, if score ≥ threshold. `nudges` (other
   profiles) keeps today's counters but adds "score ≥ threshold" as an extra trigger. A turn
   starting during a review never cancels it (Phase 4).
3. **Evidence packet** instead of replaying the conversation:
   - the signals with their excerpts (±2 messages each, tool outputs truncated);
   - the skills loaded/injected in this session, full text;
   - the top 3 related skills from the index (Phase 5) for the session's topic, full text;
   - current MEMORY.md / USER.md (or the relevant memory entries, Phase 9).
   No tools. One request with `response_format: json_schema` when the provider supports it, else
   a JSON-in-text contract with one repair retry.
4. **Output schema:**
   ```json
   { "memory": [ { "op": "add|replace|remove", "target": "memory|user", "old": "...", "text": "...", "why": "..." } ],
     "skills": [ { "op": "create|patch|none", "name": "...", "patch": "unified diff or full body", "why": "..." } ] }
   ```
5. **Apply** deterministically: memory ops through `MemoryStore::apply` (limits, threat scan),
   skill ops through `skill_manage`'s write path (threat scan, git commit). Tainted session
   (Phase 6) or `review.require_approval` → everything goes to staging. Rejected/failed ops are
   logged with the reason.
6. **Prompt** (`crates/prompts/src/review.rs`): rewrite for the evidence-packet form. Remove "be
   ACTIVE / nothing to save should NOT be the default"; instead: "Return no ops unless the
   evidence shows a durable preference, a correction, or a reusable procedure."
7. Keep the old agentic review behind `review.mode = agentic` for comparison; delete after a few
   weeks.

**Tests:** signal detection unit tests (fixtures of real-looking turns); `turns.rs` S4 under
`requests`: exactly 1 review request, triggered at idle; a session with no signals: 0 review
requests; malformed JSON → 1 repair retry then give up. Golden test for the packet builder.
**User checklist:** correct the agent twice in a session, go idle 10 min → one memory/skill
update appears (or in Pending); a smooth session → nothing.

---

## Phase 8 — Skills v2

**Coding: Sonnet.** Checkpoints:
- **CP8.1** (steps 1–2: outcomes in `.usage.json`, provenance frontmatter). Tests: curator and
  skills unit tests. Review focus: old `.usage.json` files still load; frontmatter round-trips
  without reformatting user-written skills.
- **CP8.2** (step 3: `.agents/skills` project skills, read-only fallbacks, index scope). Tests:
  skills tests, `turns` with a temp repo. Review focus: a repo's skills never leak into other
  repos' sessions; name clashes resolved as D5 says.
- **CP8.3** (step 4: curator v2 clustering, one request per cluster, proposals to staging).
  Tests: clustering fixtures, curator run on a fixture library. Review focus: the bundled 37
  don't cluster; request count = clusters; governor respected.

**Goal:** skills that are measured, traceable, scoped to projects, and merged with the user's
approval at low request cost.

1. **Outcomes** (`tools/src/curator.rs`, `.usage.json`): per skill, per use: `{ session, turn,
   outcome: clean|corrected|undone|errored }`, derived from Phase 7's signals for the turn the
   skill was loaded/injected in. Summary fields: `uses`, `clean_rate`, `last_used`. The skills
   screen shows the rate (plugin or patch).
2. **Provenance:** SKILL.md frontmatter `sources: [{session, date, why}]`, appended by every
   review write. `skill_manage` keeps it.
3. **Project skills:** `<git root>/.agents/skills/<name>/SKILL.md` (D5; also read-only from `.hermes/skills/`
   and `.claude/skills/`) loaded for sessions whose cwd
   is inside that repo; indexed with `scope = project:<root>`. The review chooses the scope
   (output field `scope: global|project`); default project when the evidence only involves files of
   one repo. Project skills are not git-committed by us (they're the repo's files): the user
   commits them.
4. **Curator v2** (`runtime/src/curator.rs`):
   - Deterministic pass (as today: stale/archive by age) + **low performers**: `clean_rate < 0.5`
     over ≥5 uses → candidate for repair.
   - **Duplicate clusters**: pairwise similarity over skill bodies (BM25 cross-scores from the
     index, or MinHash/Jaccard of 5-word shingles; pick one, threshold tuned on the bundled set so
     none of the 37 bundled skills cluster).
   - **One request per cluster / repair**: the LLM gets the cluster's full bodies (and for
     repairs, the "corrected" evidence excerpts) and returns one merged body + which names to
     archive. Max clusters per run from the knob (requests profile: 3).
   - Output goes to **staging** as a proposal with a diff; applied only on approval (setting
     `curator.auto_apply` for people who want today's behaviour).
   - Governor-gated; never runs below 20% quota.

**Tests:** outcome derivation unit tests; clustering on fixtures (two near-duplicate skills
cluster; bundled set doesn't); curator run on a fixture library: number of requests = clusters,
proposals staged. **User checklist:** create two similar skills by hand, run the curator → one
proposal with a diff in Pending.

---

## Phase 9 — Memory v2

**Coding: Sonnet, except CP9.1 on Opus** (the migration touches the user's real memory; a bug
there loses data). Checkpoints:
- **CP9.1, Opus** (`memory.db` schema, migration from MEMORY.md/USER.md, export view, hash-based
  re-import of manual edits). Tests: migration round-trip, manual-edit re-import, a crash
  mid-migration leaves the .md files intact. Review focus: no path where the .md files are
  overwritten with less than they had; a backup copy is made before the first migration.
- **CP9.2** (tiers and scopes, limits, notes files, memory tool schema with backward
  compatibility). Tests: memory unit tests, `turns`. Review focus: old-shape tool calls still
  work; project scoping by git root handles worktrees and subfolders.
- **CP9.3** (retrieval into the context block, dedupe on add, hygiene pass, links for the map).
  Tests: retrieval goldens, `turns` (turn 2 sees turn 1's archive entry). Review focus: caching
  (nothing mid-session in the system prompt); the map endpoint shape matches what the page reads.

**Goal:** memory that scales past 3.5k characters without bloating every prompt, with entries
that point to skills, files and sessions.

**Design.**
- **Store:** `data_dir/memory.db` (separate file; FTS5): table `entry(id, tier, scope, kind,
  text, tags, links, source_session, created, last_confirmed, hits, superseded_by)`.
  - `tier`: `core` (always in the system prompt), `project` (in the system prompt of sessions in
    that project; scope = git root or the store's project id), `archive` (retrieved per turn).
  - `kind`: `user` (about the user, today's USER.md) or `fact` (today's MEMORY.md).
  - `links`: `["skill:git-commit", "file:C:/…/notes.md", "session:abc", "memory:42"]`.
- **Migration:** on first start, import MEMORY.md/USER.md entries as `core`; keep writing
  MEMORY.md/USER.md as an **exported view** of `core` (so other tools and the user's habits keep
  working, and the GPUI app still reads something sensible). The DB is the source of truth;
  a manual edit of the .md files is detected (hash) and re-imported.
- **Limits:** `core` keeps today's char limits (the prompt cost). `project` gets its own limit
  (default 1500 chars per project). `archive` is unbounded.
- **Notes:** long-form knowledge goes in `data_dir/memories/notes/<slug>.md`; an entry is a
  one-line summary with a `file:` link. The memory tool gets `note_write`/`note_read` ops (or the
  memory tool's `add` accepts `body` and creates the note).
- **Retrieval:** archive entries ranked by BM25 + recency + hits go into the Phase 5 context
  block. Linked skills are boosted for injection.
- **Hygiene** (curator pass, deterministic): a `core` entry not hit/confirmed for 60 days →
  proposed demotion to archive (staged). On `add`, find near-duplicates (BM25 top-1 above
  threshold) and return them to the model: "update this one instead?" (no extra request: it's the
  tool result of the same call). Contradictions are handled by the review (Phase 7 output gets
  `supersedes: id`).
- **Memory tool schema** (`prompts/tools/memory.json`): add `tier`, `tags`, `links`, `note`; keep
  the old shape working (defaults: tier core, no links).
- **Star map / memory screen:** `links` become real edges (check what the page's map expects:
  `desktop/src/learning.rs`).
- **Mid-session visibility:** an entry written this session is retrievable by the next turn's
  context block (archive/project tiers) without touching the frozen system prompt.

**Tests:** migration round-trip (md → db → md identical); retrieval goldens; dedupe on add;
demotion proposal; a `turns.rs` scenario where turn 1 writes an archive entry and turn 2's first
request contains it. **User checklist:** your existing memory still shows; add a project memory
in repo A → it appears in A's sessions, not in B's.

---

## Phase 10 — Fewer round trips

**Coding: Sonnet.** Checkpoints:
- **CP10.1** (step 1: `run_script` and its approval). Tests: guard tests, `turns`. Review focus:
  **security** — every line of the script goes through the guard; a script can't smuggle commands
  past it (here-strings, `Invoke-Expression`, base64 `-EncodedCommand`, `iex`); size and time
  limits hold.
- **CP10.2** (steps 2–5: batching guidance, first-turn context pack, multi-path `read_file`,
  todo guidance). Tests: `turns` S1 under `requests` (≤2 main requests). Review focus: the
  context pack is cached per HEAD and capped.

**Goal:** fewer model calls per task by letting one call do more.

1. **Script tool** `run_script` (Hermes's `execute_code` idea, Windows flavour): args
   `{ language: powershell|python|bash, code, timeout }`. Runs in scratch with the session cwd,
   output captured and size-limited like the terminal. Approval: the **whole script** goes through
   the command guard (every line checked against patterns; any flagged line → ask), plus Phase 1
   path checks on obvious path literals. Python via `uv run --no-project` if available.
   Prompt guidance: "When a task needs several independent reads/searches/commands, do them in
   one `run_script` or as parallel tool calls in one response."
2. **Parallel-calls guidance** in the system prompt (frozen per session), all profiles.
3. **First-turn context pack** (requests profile, and `tokens` off): on the first user message
   in a session with a cwd inside a git repo, prepend (in the Phase 5 context block): the file
   tree (depth 3, `.gitignore`-aware, capped at ~300 lines), AGENTS.md/CLAUDE.md/README head,
   `git status --short`, current branch, last 5 commit subjects. Cached per repo HEAD in scratch.
4. **Bigger results** come from Phase 3's knobs; here, make `read_file` accept several paths in
   one call (`paths: []`) and `search` return grouped context.
5. **todo tool:** in `requests`, change the guidance to "update the todo list only at phase
   boundaries" (making it a side field of other tool calls would be too invasive).

**Tests:** S1 under `requests` with a mock that uses `run_script`: ≤2 main requests; guard tests
for `run_script` (a script containing `Remove-Item -Recurse C:\` asks).
**User checklist:** ask for a repo overview → answered in 1–2 calls.

**Done 2026-11-14** ([progress](progress/phase-10.md)). Built as planned, with these notes for later
phases:
- `run_script` belongs to the `terminal` toolset; it asks as a whole script, a dynamic construct
  always asks and is never smart-approved or remembered (PITFALLS "run_script").
- The context pack quotes the README head only; `AGENTS.md` / `CLAUDE.md` are named with their
  sizes (the system prompt already carries the first one found). It is cached per repository +
  HEAD + a status fingerprint in `<data>/cache/context-pack/` (not in a session's scratch, which
  goes with the session), and does **not** taint the session (reasoning in PITFALLS "The first-turn
  context pack"). It is unconditional on the first message (also for a "hi"): about 3,500 tokens
  at most, once, cached after the first request. `budget.context_pack` has no Settings switch.
- `read_file` with `paths` returns `{files, files_read, files_failed}` (no top-level `content`):
  a later phase that reads `read_file` results (hints, delegation summaries, renderers) must handle
  both shapes.

---

## Phase 11 — Token mode: deferred tool schemas

**Coding: Sonnet.** Checkpoints:
- **CP11.1** (steps 1–2: sizes measured, four schemas shortened, `hermes-tools-guide` skill).
  Tests: `hermes_prompts`, `turns`. Review focus: no information the model needs to call the
  tool correctly was dropped (required params, enums, constraints).
- **CP11.2** (steps 3–4: `tool_search` and deferred tools in `tokens` only). Tests: `turns`
  (request body sizes per profile). Review focus: the tool list never changes in other profiles.

**Goal:** in the `tokens` profile, cut ~8k tokens of tool schemas per request.

1. Measure each schema's size (`prompts/tools/*.json`), record in this doc.
2. **Shorter schemas:** rewrite `skill_manage` (6.4 KB), `session_search` (3.9 KB), `memory`
   (2.8 KB), `delegate_task` (2.7 KB) descriptions to ~1 KB each for all profiles, keeping the
   detail in a skill the model can load (`hermes-tools-guide`).
3. **Deferred tools** (tokens profile): the session's tool list contains a `tool_search` tool and
   the core tools; rarely used ones (`skill_manage`, `session_search`, `delegate_task`, vision,
   MCP tools beyond the first N) are listed by name only in the system prompt. Calling
   `tool_search({names})` returns their schemas **and adds them to the tool list from the next
   request** — this changes the tool list mid-session, which breaks caching; so only offer this in
   the `tokens` profile, where cache misses cost tokens the user said they care about less than
   the ~8k saved per request. (Note it in PITFALLS.)
4. `skill_manage` only in the review's tool set in `tokens` (the main agent proposes skills via
   memory notes or the review picks them up).

**Tests:** request body size assertions in `turns.rs` per profile.

---

## Phase 12 — Provider fallback chain

**Coding: Sonnet.** Checkpoints:
- **CP12.1** (all four steps). Tests: `turns` (two mocks), ledger counts. Review focus: no
  fallback on ordinary 4xx; a half-streamed response isn't duplicated on the fallback; the
  context-length check runs before switching.

**Goal:** when the main provider's quota is gone or it fails, the turn continues on the next
provider (GAPS 3.8).

1. Config: `fallback: [ {provider, model}, … ]` per profile/session.
2. Triggers: daily-quota 429 (the "N of N" shape), governor at 0, repeated 5xx/timeouts after
   retries, model-not-found. Not on ordinary 4xx.
3. Switch at the **request** level inside `InferenceRunner` (one place); emit an event so the page
   shows "switched to X"; the ledger counts on the new provider. Caching restarts on the new
   provider (unavoidable; noted in the event).
4. Context length: if the fallback's window is smaller, run compression first.

**Tests:** mock A returns the daily 429 → request goes to mock B; the turn completes; ledger
counts both. **User checklist:** set a fallback, exhaust a fake limit (set limit to 3) → turn
continues on the fallback with a notice.

---

## Phase 13 — Performance refactors

**Coding: per sub-phase.** Each sub-phase is one checkpoint (CP13a … CP13f), reviewed and
committed on its own; tests for all: `turns`, `hermes-desktop`, `cargo build --workspace` (the
frozen GPUI app shares these crates).

| CP | Coding | Review focus |
|---|---|---|
| CP13a `Arc<Message>` | Sonnet | every mutation goes through `make_mut`; no remaining deep copies on the hot path |
| CP13b store writer | **Opus** | read-after-write points call `flush()`; nothing lost on exit or crash; errors reported, not swallowed |
| CP13c `spawn_blocking` | Sonnet | no blocking call left in async code (grep `std::fs`, `std::process`, `rusqlite` in async fns) |
| CP13d session actor | **Opus** | no deadlock or lost command; cancel still immediate; one map migrated per commit |
| CP13e explicit loop | **Opus** | every `turns.rs` scenario unchanged; retry/continuation limits identical |
| CP13f typed dispatch | Sonnet | unknown methods still -32601; error shapes unchanged for the page and ACP |

Independent of the request work; best done alongside the v0.5 GPUI work, since the gateway and
runtime are shared. Each sub-phase is its own commit, with `turns.rs` + desktop tests green and
no behaviour change.

**13a — `Arc<Message>` conversations.** `Conversation(Vec<Arc<Message>>)` in `hermes_llm`;
`snapshot()` (`session.rs:159`) becomes clones of `Arc`s (≈ copying a list of references in
Python instead of `deepcopy`). Mutation sites use `Arc::make_mut` (copy-on-write). Then remove
the extra copies in `inference.rs:259-337` (`messages_for_provider` computed once per request,
borrowed). *Check:* allocation count per step in a micro-benchmark (only if the user lifts the
"no perf checks" rule; otherwise rely on code review).

**13b — Store writer actor.** One thread owns the write `Connection`; `apply_effects` sends
`StoreOp`s over a channel; the writer batches into one transaction per ~5 ms or per turn
boundary. Reads keep a separate read connection (WAL allows concurrent readers). A `flush()`
awaitable for the places that need read-after-write (session switch, export).

**13c — `spawn_blocking` everywhere it's missing.** `skills.rs:222` git via
`tokio::process::Command`; `memory`, `session_search`, `skill_*` tool dispatch
(`tools/src/lib.rs:425-430`) wrapped in `spawn_blocking`. Add a debug-build check (a
`tokio::task::block_in_place`-free lint isn't available; instead a test with a 1-worker runtime
that runs a slow memory op alongside a timer and asserts the timer fires on time).

**13d — Per-session actor.** Replace `OpenSession`'s ~7 mutexes and the gateway's ~14
`HashMap<session_id, …>` maps with one `SessionHandle { tx: mpsc::Sender<SessionCmd> }` per
session and one task owning `SessionState` (≈ a C++ thread with a message queue owning its data).
Commands: `Submit`, `Steer`, `Cancel`, `Snapshot`, `Rewind`, `SetMode`, … The existing
`RunGuard` (RAII) stays for `running`; extend the same pattern to the other paths that reset
state by hand. Migrate one map at a time.

**13e — Explicit agent loop.** Replace Goose's `StateMachine` step chain with a plain loop in
`runtime` (`prepare → infer → validate → run tools → post-checks → maybe compress`), with the
retry/continuation logic as functions returning an enum `Next::{Continue, Retry(reason), Done}`
instead of string notes in message metadata. Keep the step structs' logic; only the plumbing
changes. `turns.rs` is the safety net: every scenario must pass unchanged.

**13f — Typed gateway dispatch.** `#[derive(Deserialize)] #[serde(tag = "method", content =
"params")] enum Request { … }` replacing `match method { "…" }` in `gateway.rs`; errors as a
typed `enum RpcError`. Unknown methods still answer -32601 (ground rule 5). Split `gateway.rs`
(3.8k lines) by area into `gateway/{sessions,config,skills,…}.rs`.

Optional: `mimalloc` as the global allocator (one line; measure first if perf checks are allowed).

---

## Phase 14 — Trim `hermes_llm`

**Coding: Sonnet.** Checkpoints:
- **CP14.1** (steps 1–2: delete unused code). Tests: everything, `cargo build --workspace`.
  Review focus: nothing deleted that ACP, the GPUI app or a test still uses.
- **CP14.2** (steps 3–4: model registry, notices). Tests: everything. Review focus: context
  lengths for the user's providers still resolve the same.

**Goal:** remove Goose code we don't use (22.9k lines).

1. `cargo bloat -p hermes-desktop --release --crates` and `cargo +nightly udeps` (if available)
   for a before picture.
2. Delete: Responses-API paths in `formats/openai.rs` (we only speak chat/completions), all
   `cfg(target_arch = "wasm32")` branches and `maybe_send.rs` wasm shims, `goose_mode.rs`,
   `permission.rs` (check users first), `mcp_utils.rs` if unused.
3. Model registry (`canonical/`, 5.7 MB embedded JSON): keep only the fields we read (context
   length, pricing?, capabilities) for the providers we support, or fetch from the provider's
   `/models` and cache in `data_dir`. Decide by checking what `context_limit.rs` and the UI use.
4. Update `THIRD_PARTY_NOTICES` and `PORTING.md` rows.

**Tests:** whole workspace builds (`cargo build --workspace`), all tests green; binary size before/after
recorded here.

---

## Phase 15 — OS sandbox for the shell

**Coding: Opus** (unsafe Win32 calls, ACLs on the user's folders). Checkpoints:
- **CP15.1** (low-integrity token, `CreateProcessAsUserW` inside the Job object, setting `off`
  by default). Tests: a sandboxed shell can't write to `%USERPROFILE%`. Review focus: handles
  closed on every path; `unsafe` blocks small and commented; no change when the setting is `off`.
- **CP15.2** (labelling workspace/scratch, the label record and removal, cache env vars, folder
  grants, tool error text, docs). Tests: the full test list below. Review focus: labels are only
  ever set on folders we recorded, and removal restores them exactly.

**Goal:** a command the agent runs can't write outside the workspace and scratch, whatever the
regexes miss.

**Approach (decision D4: the simplest one):** run the shell with a **low-integrity token** inside
the existing Job object (`job.rs`). Windows' integrity levels are a built-in "no write up" rule: a
Low process can't write to anything labelled Medium (by default, nearly the whole user profile,
`Program Files`, other repos), but can write to folders labelled Low. So:
1. Duplicate the app's token, set its integrity level to Low (`SetTokenInformation` with
   `TokenIntegrityLevel`), and start the shell with `CreateProcessAsUserW` (the `windows` crate
   already in `Cargo.lock`, or `windows-sys`).
2. Label the session's workspace and scratch Low (`SetNamedSecurityInfoW` with a mandatory label
   ACE, inheritable). Remember which folders we labelled, in a side file, so the label can be
   removed (Settings → "Remove sandbox labels") — a Low label only lowers who may write there, it
   doesn't expose anything, but it should be reversible.
3. Tools that need to write elsewhere (cargo's `~/.cargo/registry`, npm cache, `%TEMP%`) get their
   cache env vars pointed into scratch, or the user grants a folder (labelled Low on grant).
4. Setting `terminal.sandbox = off | low`, default `off` for the first release, then `low`.
   A blocked write surfaces as a clear tool error ("blocked by sandbox: <path>; grant the folder
   in Settings or move the work into the workspace").

Reads stay unrestricted (Low can read Medium), so this doesn't stop exfiltration of readable
files; that's covered by approvals + taint (Phase 6). AppContainer would also restrict reads and
network but needs per-tool grants and breaks more; not planned unless `low` proves too weak.

**Tests:** spawn a sandboxed shell, try to write to `%USERPROFILE%\x.txt` → denied; write in
workspace → works; `git status`, `git commit` in the workspace, and `cargo build` with
`CARGO_HOME`/`CARGO_TARGET_DIR` in scratch/workspace work.

---

## Cross-cutting

- **Docs per phase:** GAPS.md row (✅ + date), PORTING.md rows (`diverged` where we leave
  Hermes), ARCHITECTURE.md for new modules/crates, PITFALLS.md for surprises,
  `scripts/perf/README.md` for new checks.
- **Settings migration:** the budget profile defaults to `requests` (D1), so upgrades change
  behaviour on purpose; every other new key defaults to today's behaviour unless its phase says
  otherwise.
- **ACP** (`hermes-acp.exe`) shares the runtime, so it gets Phases 1–12 for free; check
  `crates/desktop/tests/acp.rs` after Phases 2, 5, 6, 13d, 13f.
- **GPUI app** (frozen) must still build after 13a/13b/13d (shared crates).
- **Naming:** if the project gets a new name, do it as its own phase after 9 (crate names,
  exe names, data folder migration from `%APPDATA%\hermes-rs`, installer ids). Not planned in
  detail until a name is chosen.

## Decisions (user, 2026-11-11)

- **D1** Default budget profile: `requests` (Phase 3).
- **D2** Yolo + untrusted content: warn only, never block (Phase 6).
- **D3** Pending items live in the existing memory and skills screens (Phase 6).
- **D4** Sandbox: the simplest option, a low-integrity token (Phase 15).
- **D5** Per-repo skills folder: `<git root>/.agents/skills/` (Phase 8). Tool-neutral, already
  read by other agents (Goose among them), and survives a rename of the app. We write only there;
  we also *read* `.hermes/skills/` and `.claude/skills/` if present (read-only, lower priority on
  name clashes), so skill folders already in a repo are picked up. Project *memory* (Phase 9)
  needs no folder: it lives in `memory.db`, keyed by the repo's path.

## Baselines

Filled in by Phase 0 (requests per scenario, per purpose, `balanced`), then a column per later
phase under `requests`.

| Scenario | Baseline | After 3 | After 5 | After 7 | After 10 |
|---|---|---|---|---|---|
| S1 read 3, edit 1 | 5 | 5 | 5 | 5 | 5 (a model that batches: 2 with one `run_script`, 3 with `paths` or parallel reads) |
| S2 empty reply after tools | 3 | 3 | 3 | 3 | 3 |
| S2b empty first reply | 2 | 2 | 2 | 2 | 2 |
| S3 truncated call | 3 | 3 | 3 | 3 | 3 |
| S4 ten turns (incl. review) | 11 | 11 | 11 | 10 (0 review) | 10 |
| S5 delegate two | 4 | 4 | 4 | 4 | 4 |
| S6 task a skill covers (CP5.2) | 2 (`skill_view`) | 2 | 1 | 1 | 1 |

Phase 0 baselines (2026-11-11, `balanced` = today's behaviour; all counted as `main` until Phase 2
labels requests). After 3 (`requests`, 2026-11-12): unchanged. Phase 3 only moves caps the
scenarios don't reach and adds prompt wording; the request-lowering `requests` values (review
`idle_once`, skills preload, heuristic titles) wait for Phases 4, 5 and 7, so the plan's "lower
than baseline for S2, S3, S5" is not met yet. Phase 0's breakdown: S1 = 4 tool rounds + answer; S2 = tool round + empty reply + answer after
`EmptyResponseRecovery`'s nudge; S2b = empty first reply + the inference step's own retry (it never
reaches `EmptyResponseRecovery`); S3 =
cut-off tool call + retry + answer; S4 = 10 turns + 1 background review (memory and skill counters
both due on the tenth turn, one combined review request); S5 = parent call + 2 children + parent
answer. The mock counts every request, so retries and background jobs show up in the totals.

After 4 (`requests`, 2026-11-12): S1-S5 unchanged (the scenarios make no title, approval or
summary request). What Phase 4 moved off the main provider, asserted in `turns.rs`
(`s4_makes_no_title_and_no_approval_requests`, `compression_via_the_aux_model_...`): title
requests per new session 1 -> 0 (heuristic) under `requests` without an aux title model; approval
judge requests per flagged command 1 -> 0 under `requests` without an aux approval model;
compression summaries with an aux model 1 -> 0 on the main provider (1 only when the aux server
fails); a review running when the next turn starts is no longer cancelled (its requests no longer
wasted).

After 5 (`requests`, 2026-11-12): S1-S5 unchanged (`baseline_s*` run under every profile; their
messages match no skill well enough to be attached). New scenario S6, a task a bundled skill
covers (`s6_*` in `turns.rs`, the `arxiv` skill): `balanced` 2 main requests (`skill_view`, then
the answer), `requests` 1 (the skill's body rides on the user message in a `<context>` block).

After 7 (`requests`, 2026-11-13): S4 (ten smooth turns) 11 -> 10: `idle_once` has no counters and a
smooth session leaves no learning signal, so no review, not at the tenth turn and not at idle
(`s4_ten_turn_session_with_review`). The same ten turns with two corrections: exactly 1 review
request (the evidence review: one request, no tools), made once the session goes quiet, none while
the turns run (`s4_requests_two_corrections_make_exactly_one_review_at_idle`); a session with no
signals: 0, idle or switched away from. An evidence review is 1 request (2 with its one repair, at
most 3); at most `review_per_hour` (6) start per hour. S1-S3, S5, S6 unchanged. `balanced` /
`tokens` (`nudges`): S1-S6 unchanged (S4 still 11, the counter review); a session whose signals
reach the threshold gets the agentic review at that turn's end instead of waiting for the counters.

After 8 (`requests`, 2026-11-13): S1-S6 unchanged (no scenario runs the curator). The curator, which
under `requests` sent nothing before (archiving only): one `curator` request per duplicate cluster
or low performer (two with its one repair), at most `curator_max_requests` (3) jobs a run, **0**
when there is nothing to merge or repair, below 20% of the day's quota, or while its back-off holds
(`turns.rs`, after the `Phase 8, CP8.3` banner). `balanced` / `tokens`: unchanged (the agent loop).

After 9 (all profiles, 2026-11-13): S1-S6 unchanged. New, exact (`turns.rs`, after the `memory
retrieval, dedupe, hygiene (Phase 9 CP9.3)` banner): turn 1 writes an archive entry (2 requests:
the call, the answer), turn 2 asks about it (1) = 3 under `requests`, `balanced` and `tokens`
(under `requests` the entry rides on turn 2's message instead of costing a tool call; there is no
archive search tool, so under the other two the entry is not reached at all); the dedupe note on
`add` costs 0 (it is in the same tool result); the curator's memory hygiene pass costs 0 (and runs
under `requests` only).

After 10 (`requests`, 2026-11-14): S1-S6 unchanged as scripted (the mock replays the same calls;
`baseline_s*` pass under every profile). What a model that batches gets (`turns.rs`, after the
`Phase 10, CP10.2` banner): S1 in **2** main requests with one `run_script` (reads three files and
edits one), **3** with one multi-path `read_file` or three parallel reads, then the edit (CP10.1:
three reads as one script = 2 instead of 4). "Give me an overview of this repository" as the first
message of a session in a git repository: **1** request, with the branch, commits, status, README
head and tree already on the message (the first-turn context pack; at most 14,000 characters,
cached by the prompt after the first request). The counts that need a model to choose the batched
form are what the mock is scripted to do; the tests assert that the batched calls work and what the
pack contains, not that a real model will batch.

Since Phase 2 the mock also splits them by purpose (the `x-hermes-rs-purpose` header): S1-S3 are
all `main`; S4 is 10 `main` + 1 `review`; S5 is 2 `main` + 2 `delegate`. The totals above are
unchanged.
