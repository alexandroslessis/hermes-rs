# Porting from Hermes

hermes-rs re-implements [Hermes](https://github.com/NousResearch/hermes-agent) in Rust and runs
Hermes's own desktop frontend on top of it (`crates/desktop`). This file says where each piece
came from and how to move to a newer Hermes.

## The pinned commit

[`HERMES_COMMIT`](HERMES_COMMIT) at the repository root is the Hermes commit this tree ports, and
the one place it's set. Reading it:

- `scripts/build-hermes-webapp.ps1` builds the frontend from that commit;
- `crates/desktop` reports it in `/api/health` (`commit`);
- `scripts/hermes-update-report.mjs` diffs from it.

The Hermes clone lives next to this repository (`..\hermes-agent`). It's never modified; builds
export from it with `git archive`.

## Citations: `Hermes:` lines

A module ported from Hermes names its source in its header doc comment, with paths relative to the
Hermes repository root:

```rust
//! Background processes started by the terminal tool.
//!
//! Hermes: `tools/process_registry.py` (registry, output buffer, `process` tool actions).
```

These lines are the port map. The update report reads them (any Hermes path mentioned in a `.rs`
file counts) to list the code each Hermes change touches. A new port gets its line. When a function
follows a specific Hermes function closely, name it too (`` `agent/redact.py` `redact_text` ``) so the
next diff can be read side by side.

Not from Hermes, so not in the map:

- `crates/llm` and `crates/agent`: from [Goose](https://github.com/block/goose) (provider, messages,
  agent loop); see `THIRD_PARTY_NOTICES.md`.
- `crates/app`, `crates/themes`, `crates/highlight`, `crates/markdown`, `vendor/zed`: the GPUI app
  and Zed's UI code. The GPUI app is frozen (kept as a fallback, no new features).

## Map

A row marked `diverged` is backend behaviour we deliberately changed (AGENTS.md rule 1, plan in
`docs/backend/PHASES.md`): its module doc says how and why, and a Hermes update must not re-port it
(read the Hermes diff for ideas only). Protocol shapes are never `diverged`.

| hermes-rs | Hermes |
|---|---|
| **Desktop backend** (`crates/desktop`) | |
| `server.rs` | `hermes_cli/web_server_dashboard.py` (page globals, token), `tui_gateway/ws.py` |
| `gateway.rs` | `tui_gateway/server.py`, `methods_prompt.py`, `methods_session.py`, `ws.py`; `agent/context_breakdown.py`; the page's side: `apps/desktop/src/api/canonical-protocol.ts`, `apps/shared/src/gateway-contract.openrpc.json` |
| `hub.rs` | event fan-out: `apps/desktop/src/api/canonical-protocol.ts` |
| `transcript.rs` | `tui_gateway/methods_session.py`, `tui_gateway/prompt_turn.py`, `agent/conversation_loop.py` (`_close_durable_failed_turn`), `agent/turn_failure_copy.py` (`failed_turn_notice`) |
| `rest.rs` | `hermes_cli/config.py` (config schema), `hermes_cli/web_routers/*.py`, `apps/desktop/src/types/hermes.ts` |
| `commands.rs` | `tui_gateway/command_discovery.py`, `tui_gateway/methods_complete.py` |
| `paths.rs` | `tui_gateway/methods_complete.py` (`@` paths), `/api/fs/list` |
| `projects.rs` | `tui_gateway/project_tree.py` |
| `git.rs` | `hermes_cli/web_git.py` (`repo_status`, `review_rev_parse`) |
| `terminal.rs` | `hermes_cli/web_host_terminal.py`, `web_host_terminal_sessions.py`, `pty_session.py` (`/api/host-terminal`); single viewer per shell (a newer one supersedes with 4409), no input queue limits |
| `git_review.rs` | `hermes_cli/web_git.py` (the rest: review pane, commit, branches, worktrees, `gh` ship flow), `hermes_cli/web_routers/git.py`; a configured `init.defaultBranch` counts only when that branch exists |
| `attachments.rs` | `hermes_cli/web_routers/files.py` (`/api/chat/image-upload`, `/api/fs/read-data-url`), `hermes_cli/web_routers/uploads.py` (`/api/chat/file-upload`), `tui_gateway/methods_prompt.py` (`image.attach*`, `image.detach`, `file.attach`), `agent/image_routing.py` (`agent.image_input_mode`) |
| `files.rs` | `hermes_cli/web_routers/files.py` (`/api/fs/read-text`, `/api/fs/write-text`, `/api/files/mkdir`, `/api/fs/git-root`, the sensitive-path guard; `/api/fs/download`, `/api/files/stream`, `/api/files/ticket` are in `server.rs` and `gateway.rs`) |
| `endpoints.rs` | `hermes_cli/web_routers/models.py` (`/api/providers/custom-endpoints*`), `hermes_cli/custom_providers.py`; Chat Completions only |
| `commands.rs` (coding) | `agent/plan_prompt.py`, `hermes_cli/init_command.py`, `agent/learn_prompt.py`, `agent/review_engine.py` (prompts in `crates/prompts/src/coding.rs`), `tui_gateway/methods_tools.py` (`send` dispatch), `methods_slash.py` (`/review`); the reviewer runs in the turn (a scripted `delegate_task`), not as a background delegation |
| `open_file.rs` | `apps/desktop/electron/` opener rule (`openExternal` for `file:`), `apps/shared/src/external-url.ts`; with patch 0013, since Hermes's browser build drops `file:` links |
| `archive.rs` | `hermes_cli/web_routers/sessions.py` (`archived=` filter, PATCH `archived`), `hermes_state_maintenance.py` (`maybe_auto_archive`, `archive_stale_sessions`), `hermes_cli/config_defaults.py` (`sessions.auto_archive*`); the set lives in `archived-sessions.json` (the shared schema has no archived column) |
| `branch.rs` | `tui_gateway/methods_session_branch.py`; copies stored messages (tools included) where Hermes copies visible text; parent links in `branches.json` (the shared schema has no parent column) |
| `workspace.rs` | `gateway/session_mutation_policy.py` (`prepare_workspace` — the folder probe), `hermes_state_mutation_prepared.py` (`workspace_in_transaction` — the store move), `tui_gateway/git_probe.py` (`branch`, `common_repo_root`); the git identity lives in `session-workspaces.json` (the shared schema has no git columns), and a live session's cached state is dropped (`Runtime::evict`) where Hermes evicts its cached agent |
| `profiles.rs` | `hermes_cli/web_routers/profiles.py` (`get_profile_soul`, `update_profile_soul`), `hermes_cli/web_server_profiles.py` (`_resolve_profile_dir`); one profile ("default", home = the data folder) — the multi-profile routes (create, rename, delete, export, import) aren't ported: no second profile and no relaunch to switch to one |
| `acp/` (`--acp`) | `acp_adapter/entry.py` (`mod.rs`: stdio only, logs to stderr), `gateway_server.py` (`server.rs`: `GatewayACPAgent` — initialize, new/load/list, prompt, cancel, the event projection, approvals and clarify), `server.py` (`list_sessions` paging, `_replay_session_history`), `gateway_reply.py` + `events.py` (`translate.rs`), `tools.py` (`tools.rs`: kinds, titles, locations, diffs), `content.py` (`prompt.rs`), `permissions.py` + `elicitation.py` (`permissions.rs`), `model_catalog.py` + `modes.py` (`controls.rs`; our modes are the session's `ApprovalMode`: Auto, Manual, Accept edits, Plan, YOLO, not Hermes's edit-approval modes; efforts are Hermes's frontend list), `gateway_commands.py` (`/undo [N]`, `/retry` in `server.rs` `rewind_command`, over `session.rewind`). `hermes-acp.exe` is `src/bin/hermes-acp.rs`. Differences: the editor's MCP servers, `fs/*` and `terminal/*` are not used; a `write_file` diff's "before" is read by the server (our result carries no diff); no `session/fork`, no auth methods |
| `aux_models.rs` | `hermes_cli/web_routers/models.py` (`get_auxiliary_models`), `hermes_cli/web_server_config.py` (`_apply_aux_assignment_sync`, `_AUX_TASK_SLOTS`); four slots, kept in `config.json` `aux_models` with the GPUI app's names. **Extended, diverged** (Phase 4 CP4.2): `GET/POST /api/model/auxiliary/local` (`local_list`, `use_local`) are ours, not Hermes's: one click finds a local server (a local provider in Settings, else Ollama :11434 / LM Studio :1234, added as a provider) and sets every slot to one of its chat models; the page reaches it through patch 0023 |
| `toolsets.rs` | `hermes_cli/web_routers/tools.py` (`/api/tools/toolsets*`, `/api/tools/terminal/backends`), `hermes_cli/tools_config.py` (`CONFIGURABLE_TOOLSETS`); keeps the disabled toolsets (`disabled_toolsets`) where Hermes keeps the enabled ones per platform |
| `learning.rs` | `agent/learning_graph.py`, `agent/learning_mutations.py` (`/api/learning/*`, served by `hermes_cli/web_routers/status.py`); memory edits through `MemoryStore::edit_entry` (the store's lock and drift guard), skill deletes through `archive_by_user` |
| `hermes_import.rs` | `tui_gateway/methods_session_foreign.py` (`session.foreign.list/preview/import`, Hermes's Claude Code / Codex import screen, here fed from Hermes's own `state.db`), `hermes_state_common.py` (the schema), `hermes_constants.py` (`get_hermes_home`) |
| `export.rs` | `session.export` (`tui_gateway/methods_session.py`), the page's `/save` |
| `maintenance.rs` | `hermes_cli/web_routers/status.py` (`/api/curator*`, `/api/actions/*`), `hermes_cli/web_routers/ops.py` (`/api/memory*`), `agent/curator.py` (the schedule) |
| `skills.rs` | `hermes_cli/web_routers/skills.py` (local routes), `hermes_cli/skills_config.py`; the hub (`tools/skills_hub*.py`, `tools/skills_guard.py`) not ported |
| `staging.rs` | **diverged**: not from Hermes. `staging.list` / `staging.apply` / `staging.reject` and `GET/POST /api/staging*`: the writes a tainted session's agent left for the user to review. Ours, outside Hermes's contract (the Memory and Skills screens call them through patches 0024-0026); the logic is `hermes_tools::staging` |
| **Runtime** (`crates/runtime`) | |
| `commands.rs` | `hermes_cli/cli_commands_mixin.py`, `tools/working_diff.py` |
| `compress.rs`, `ops.rs` | `agent/context_compressor.py`, `agent/turn_tool_validation.py`, `tools/budget_config.py`. **`Summarizer.fallback` diverged** (Phase 4 CP4.2): Hermes's compressor has `summary_model_fallback` only on a model-not-found error; here a failed summary from the auxiliary compression model (server down, error, empty reply) is retried once on the session's own model, and the aux provider itself retries at most once (`summarizer_for`) within a 300 s deadline (`AUX_SUMMARY_DEADLINE`; Hermes: 120 s idle / 600 s total, `agent/conversation_compression_deadline.py`) |
| `guards.rs` | `agent/repetition_guard.py`, `agent/tool_guardrails.py` |
| `review.rs` | `agent/background_review.py` |
| `curator.rs` | `agent/curator.py` |
| `Runtime::side_question`, `Runtime::background` | `agent/side_question.py` (one-shot path only; the cache-parity fork isn't ported), `tui_gateway/methods_prompt.py` (`prompt.btw`, `prompt.background`; `/bg` runs as a subagent rather than a full agent) |
| `subagents.rs`, `delegate.rs` (events) | `tools/delegate_tool_registry.py`, `tui_gateway` `subagent.*` and `delegation.status/pause`; `spawn_tree.*` not ported |
| `ledger.rs` (+ `crates/llm/src/observer.rs`) | **diverged**: not from Hermes (Hermes doesn't count requests). Counts every model request per provider, day and purpose in `requests.json`; `ApiClient` reports each HTTP attempt to the task's `RequestContext` |
| `governor.rs` | **diverged**: not from Hermes. From today's ledger count and the provider's daily limit (`requests.limits` in config, or learned from a 429), defers background jobs below 20% left, delegation and the approval judge below 5%, and refuses the main turn only at zero |
| `budget.rs` | **diverged**: not from Hermes (it has one fixed set of limits). `requests` / `balanced` / `tokens` profiles set a bundle of knobs (iteration budget, retry caps, compression thresholds, result/read/search limits, delegation width); `balanced` is the constants Hermes had. Replaces `max_iterations`, `MAX_*_RETRIES`, `MAX_LENGTH_CONTINUATIONS`, `CHILD_MAX_ITERATIONS`, `CURATOR_MAX_ITERATIONS` and the compression percents, so those lines no longer match Hermes. The profile reaches the system prompt (a short budget paragraph in the stable tier, none for `balanced`; the skills block is full under `balanced`, categories under `requests`, names under `tokens`), chosen when a session is made and frozen with it; `read_file`'s and `delegate_task`'s schemas state no number, since a schema is fixed for a session and the profile is not (`prompts/tools/README.md`) |
| `signals.rs` (runtime), `TurnState::calls`/`steers` in `ops.rs`, the `begin_turn` / `rewind_as` wiring | **diverged** (Phase 7 CP7.1): not from Hermes, whose review runs on fixed counters (`memory.nudge_interval`, `skills.creation_nudge_interval`) with no notion of what happened. After each main turn (never the review, curator, subagents, `/bg`, `/btw`), with no request and nothing the model sees, the user's typed text and `/steer`s are scored for corrections and rules (cleaned of code, quotes, logs, pasted e-mails; English only; context rules for idioms, questions, first messages and answers to the assistant's questions), and the turn's tool calls for denials, a tool that failed twice then worked with other arguments, and long successful turns; `/learn` and "remember this" are explicit; an undo / rewind / `/retry` / checkpoint restore that drops something the agent did is strong. Signals with captured evidence go to `signals/<id>.json` (bounded) through one writer thread; `Runtime::signals` / `mark_signals_reviewed` are what CP7.2/7.3 read. The review itself is unchanged until CP7.2 |
| `review_once.rs` + `review_once/` (runtime: `packet.rs`, `contract.rs`, `apply.rs`, `udiff.rs`), `Runtime::review_once`, `evidence_review_prompt` / `evidence_review_repair` in `prompts/src/review.rs`, `OpenAiCompatibleProvider::with_transient_only` (llm) | **diverged** (Phase 7 CP7.2): not from Hermes, whose review (`agent/background_review.py`, still ported in `review.rs`, the one `nudges` and `agentic` run) replays the whole conversation to a forked agent that writes through the memory and skill tools (up to 16 requests). Here one request, no tools: an evidence packet (the unreviewed signals with their excerpts, the skills the session used and the top 3 related ones from the index in full, MEMORY.md / USER.md; secrets masked; within a token budget, 24k by default, at most half the review model's window and what it leaves after the prompt, schema, repair turn and reply; counted pessimistically: an ASCII character a third of a token, CJK 1.5) goes to the review model (the aux review model when set; ledger purpose `review`; the governor asked first) with `response_format: json_schema`; a server that refuses that field (400/422) is asked once more in text and, when that goes through, remembered per endpoint+model; each request has a 10-minute limit; an unreadable reply gets exactly one repair request. The reply's operations (`memory`: add/replace/remove; `skills`: create/patch/none) are validated one by one (unknown ops or fields, wrong types, oversize, bad skill names refused with a logged reason) and applied through `MemoryStore::apply` and `SkillStore::apply_staged` (limits, threat scans, git commit), each on its own; a skill is patched only if the packet showed it, only while its file is unchanged, with a diff that applies exactly (no fuzz) or, if shown whole, a full body; never deleted. A tainted session, `require_approval`, or a store's `write_approval` stages every write in `staging/` instead; a memory replace or remove is staged in every session (as Hermes's `_background_delete_gate` does for its review). Signals are marked reviewed only after a reply was read |
| `review_schedule.rs` (runtime), `ReviewMode` (`idle_once` / `nudges` / `agentic`) and the `review_threshold` / `review_idle_minutes` / `review_per_hour` knobs in `budget.rs`, the score trigger in `ReviewSetup` (`lib.rs`) and `review.rs`, `Gateway::viewing` / `install_review_route` (desktop), `budget.review_mode` select in `rest.rs` | **diverged** (Phase 7 CP7.3): Hermes reviews on counters only (every 10 user turns / 10 model calls without `skill_manage`). `idle_once` (the `requests` profile): the evidence review runs once when a session has been idle `review_idle_minutes` (10) or was switched away from (a `session.resume`/`session.create` of another id on the same socket, after 3 s) or closed (ACP), if its signals reached `review_threshold` (3.0); per-session timers reset by activity, cancelled on delete, aborted when the runtime drops; one review of any kind per session at a time (a trigger meanwhile is dropped, `/refine` refused); evidence and its max `seq` fixed at the start; back-off after a failure (24 h when nothing was sent, else 1 h doubling to 24 h); at most `review_per_hour` (6) starts per hour across sessions; a review cut off twice (exit, crash) or failing after its request waits 1 h doubling to 24 h across restarts too (`review_tries` in the signals file); governor as for any review; no startup sweep; a socket switching away from a session another window still shows does not leave it. `nudges` (`balanced`, `tokens`): Hermes's counters plus the score, both running Hermes's agentic review, which marks the signals it saw reviewed unless it ended on an error; a score-started review that did not cover them holds the score trigger back for an hour. A "remember this" the agent saved with `memory` in the same turn is a weak signal, not an explicit one. Settings ▸ Model ▸ Main shows "Review mode" via patch 0027. `agentic`: Hermes's behaviour exactly, kept for comparison (to be deleted a few weeks after Phase 7). The evidence prompt no longer pushes for output ("Return no ops unless...", the packet is untrusted data); the agentic review keeps Hermes's prompt. The outcome is logged through `UserBridge::review_finished` (no page event) |
| `titles.rs` (runtime), `review.rs`/`session.rs` (`ReviewHandle`), `SmartGuard` wiring in `lib.rs` | **diverged** (Phase 4 CP4.1): Hermes always asks a model for a title, cancels a running background review when a turn starts, and lets the smart-approval judge fall back to the main model. Here, under `requests` with no auxiliary title model, `heuristic_title` names the session from its first message (no request; an aux title model, or another profile, keeps `generate_title`); under `requests` with no auxiliary approval model a flagged command just asks the user; a turn starting never cancels a review: one that has not taken its snapshot yet waits for the turn to end, one that has finishes on its own copy, and a newly due review is skipped while one runs (`/refine` replaces it); a paused review is replaced by a newly due one, which takes over what it covered, except a paused `/refine`, which stays. The review's `read_file` doesn't count as the session's read for the write guard (`files::SIDE_READ`) |
| **Tools** (`crates/tools`) | |
| `path_policy.rs` | **diverged**: not from Hermes (Hermes has no path policy). Classifies where file tools and terminal commands may write: workspace, app data, skills/memory, persistence places (startup folders, shell profiles, git hooks, ssh), outside. `check_edit` and the terminal guard use it |
| `taint.rs` | **diverged**: not from Hermes (it has no taint tracking). Which sources make the session careful (web tools, browser, `@url`, MCP results unless the server is `trusted`, files read outside the working folder, web images, subagents that read any of those), the two levels (this turn / earlier in the session), and `taint.json` in the data folder that keeps the session level. `check_command` / `check_edit` in `lib.rs` read it: flagged commands always ask while tainted, Yolo only warns (`AgentEvent::Status`, shown as a `status.update` line) |
| `staging.rs` (tools), `FileBase` / `plan` / `apply_staged` in `skill_manage.rs`, `MemoryStore::parse_call`, the `stale` flag on `MemoryStore::apply`'s refusal | **diverged** (Phase 6 CP6.2): not from Hermes, whose `write_approval` queues (`tools/write_approval.py`, kept as they are in `memory/tool.rs` and `skill_manage.rs`, switched on by a setting) are per store. A tainted session's `memory` / `skill_manage` calls are checked and kept as `staging/<id>.json` (`{v, id, kind, op, diff, reason, session_id, sources, created, status, error}`, versioned, atomic, bounded) instead of written; Apply goes through the stores' own write paths after checking that what the write was reviewed against is unchanged. Phases 7 and 8 stage through the same store. CP6.3: foreground `memory` / `skill_manage` calls of a session whose `write_approval` is on stage here too (reason "write approval is on"), so Hermes's old queues (`pending/memory/`, `skills/.pending/`), which its page has no screen for, receive nothing new from the page's sessions; review writes (`memory` unattended, `skill_manage` from the review) still use them until Phase 7 |
| `shell.rs` | `tools/terminal_tool.py` (**diverged** in `command()`: the shell starts from the parent environment minus secret-looking variables, `terminal.env_passthrough` keeps some; Hermes passes the whole environment); the live-output relay is `agent/tool_progress_relay.py` (Hermes relays MCP tools only; ours covers the terminal, throttled the same), and stopping a mid-run command keeps its output, the spirit of `tool_executor.py`'s sequential interrupt grace (ours ends the command instead of waiting 3 s) |
| `process.rs` | `tools/process_registry.py` |
| `search.rs` | `tools/file_tools.py`, `tools/file_operations_search.py` |
| `lint.rs` | `tools/file_operations_lint.py` |
| `checkpoints.rs` | `tools/checkpoint_manager.py` |
| `approval.rs` | `tools/approval_detection.py`, `tools/approval_smart.py` (**diverged**: `check_command` also flags commands naming protected paths and skill scripts the agent wrote; persistence places ask even in Yolo) |
| `clarify.rs` | `tools/clarify_tool.py` |
| `todo.rs` | `tools/todo_tool.py` |
| `recall.rs` | `tools/session_search_tool.py` |
| `vision.rs` | `tools/vision_tools.py`, `agent/image_routing.py` |
| `web.rs` | `plugins/web/keyless_mcp.py` |
| `redact.rs` | `agent/redact.py` |
| `references.rs` | `agent/context_references.py` |
| `hints.rs` | `agent/subdirectory_hints.py` |
| `skills.rs` | `tools/skills_tool.py`, `agent/skill_commands.py`, `agent/skill_utils.py`, `agent/prompt_builder.py` |
| `skill_manage.rs` | `tools/skill_manager_tool.py`, `tools/skill_manager_batch.py`. **Partly diverged** (Phase 8 CP8.1): every SKILL.md write can carry a `SkillOp.source` (set by our code, never read from the model's call) that is appended to the skill's frontmatter `sources`; a rewrite by the agent keeps the file's `sources` (see `provenance.rs`) |
| `project_skills.rs` (tools), `SkillOp.project` / `scope` and `SkillStore::*_for` in `skills.rs` / `skill_manage.rs`, `SessionTools::project_root`, the project script flag in `path_policy.rs` | **diverged** (Phase 8 CP8.2): not from Hermes. Skills of a git repository: `<git root>/.agents/skills/<name>/SKILL.md` (decision D5; the only place we write) plus read-only `.hermes/skills/` and `.claude/skills/`, for sessions whose working folder (fixed when the session's tools are made; subagents take their parent's) is inside that repository. Precedence on a name: the user's library, `.agents`, `.hermes`, `.claude`. Untrusted: threat scan, 100k size, valid names, no symlink or junction followed; a project skill put in front of the model taints the session; a command running one of its `scripts/` asks like an agent-written skill script. Module doc has the rules |
| `bundles.rs` | `agent/skill_bundles.py` |
| `curator.rs` | `agent/curator.py`, `tools/skill_usage.py`. **Partly diverged** (Phase 8 CP8.1): `.usage.json` records also keep `outcomes` (per use `{session, turn, outcome, at}`, newest 50) and a `clean_rate`, unknown fields are kept on rewrite, an unreadable file is copied to `.usage.json.corrupt` before being replaced, a field of the wrong type is coerced or dropped alone, a locked file is not overwritten, and a skill the local index attached is counted in `attached` / `last_attached` (activity for the curator; `uses` stays a whole-skill load) |
| `curator/` (runtime: `cluster.rs`, `contract.rs`, `propose.rs`, `per_cluster.rs`, `gate.rs`, `seen.rs`), `crates/prompts/src/curator.rs`, `SkillOp` `archive` and `SkillStore::records` / `check_archive` in `skill_manage.rs` / `curator.rs` (tools), `budget.curator_mode` / `curator_max_requests`, `curator.auto_apply`, `maintenance::curator_model` | **diverged** (Phase 8 CP8.3): not from Hermes, whose curator is one agent loop over the agent-created skills (`curator.rs`, kept as the `balanced` / `tokens` behaviour). Under `requests` (or `budget.curator_mode: per_cluster`) the curator picks jobs locally: duplicate clusters among the user's own library skills (Jaccard of 5-word shingles, threshold 0.35, none of the bundled skills clusters; a cluster with more than one skill that has its own files is left out) and low performers (at least 5 judged uses, under half clean; they need evidence from the signals), and sends **one request per job** (at most `curator_max_requests`, 3, a run) with the skills' full text; the answer is strict JSON (one repair retry), validated against the job, and becomes **one staged proposal** (an `edit` of the kept skill plus `archive` of the absorbed ones, with a diff, in Capabilities / Skills / Pending), or is applied through the same checks when `curator.auto_apply` is on and every skill it touches was created by the background review; a skill edited while the model read it refuses the proposal. Governor-gated (nothing below 20% of the day's requests), hourly cap and back-off of its own. A Hermes update that changes `agent/curator.py` changes only the `Loop` path |
| `provenance.rs` (tools) | **diverged** (Phase 8 CP8.1): not from Hermes. SKILL.md frontmatter `sources: [{session, date, why}]`, appended as a text edit of that block only (no YAML round trip: user-written skills keep their comments, order, quoting and line endings), at most 20 entries, `why` one line of at most 160 characters through the threat scan |
| `skill_outcomes.rs` (runtime), the `begin_turn` / `rewind_as` wiring | **diverged** (Phase 8 CP8.1): not from Hermes. After each main turn, no request: the skills used in it (attached by the local index, or loaded whole with `skill_view`) get `clean` / `errored` / `corrected` in their records; a correction in the next message makes the previous turn's uses `corrected`, a rewind makes the dropped turns' `undone` (rule in the module doc) |
| `index.rs` | **diverged**: not from Hermes (Hermes lists every skill in the system prompt; no local index). SQLite FTS5 file `index.db` (skill_doc, memory_doc; BM25, `porter unicode61`) over SKILL.md files and MEMORY.md / USER.md, rebuilt incrementally (mtime + hash) and after every skill / memory write; query escaping turns user text into quoted tokens. CP5.1 of Backend v2 Phase 5. Phase 8 CP8.2 (schema version 3): every skill row has a `scope` (`global` or `project:<root>`) and a query selects only `global` plus its own repository's rows (`IndexQuery::project_root`); a repository is indexed once a query names it and re-checked by `stat` at each such query |
| `preinject.rs` (runtime), `IndexStyle` in `skills.rs`, the `Runtime::begin_turn` / `user_message` wiring | **diverged** (Phase 5 CP5.2): not from Hermes, which lists every skill in the system prompt and has the model load them (`skill_view`). Under `requests` (`budget.skills_preload` / `memory_preload`) `begin_turn` asks the local index with the message, the last assistant message and the project name, and prepends a `<context>` block (`<relevant-skills>` with full SKILL.md bodies, `<relevant-memory>` with entry ids) to the **user message**, stored with it (a `preinject` note holds what was attached; the `references` note's `typed` holds what the user wrote); the transcript, `/undo`, rewind and the `/review` briefing show the text without it (`preinject::strip`), and so do two Hermes ports that otherwise match: session search's rows (`store/src/search.rs`, `without_attached_context`) and the compaction summarizer's input (`compress.rs` `serialize`). The skills block of the system prompt has three shapes (`IndexStyle`): `Full` (Hermes's text), `Categories` (category names and counts), `Names`; the last two drop "Err on the side of loading" |
| **Memory** (`crates/memory`) | |
| `store.rs` | `tools/memory_tool_store.py` (the operations, limits, drift guard and answers; since Phase 9 CP9.1 backed by `memory.db`, see the next row, with the public API and every answer unchanged) |
| `db.rs`, `sync.rs` | **diverged** (Phase 9 CP9.1): not from Hermes, which keeps memory in MEMORY.md / USER.md only (a lock file and atomic writes). Ours: `data_dir/memory.db` (SQLite, own `user_version`, a newer one is refused and never written; a damaged one is moved aside, with its `-wal`/`-shm`, to `memory.db.corrupt-<time>` and rebuilt from the files) holds the entries (`entry(id, tier, scope, kind, text, tags, links, source_session, created, last_confirmed, hits, superseded_by, position, removed, removed_why)` + FTS5); the two files are the exported view of its global `core` entries. First use: a backup copy `<file>.bak.pre-memory-db.<time>` (create-new, read back, kept), then the import; no copy, no import. Every access compares file and database (hash of the last export + the entries) and takes a changed file over (manual edit, older build, crash after our rename): added → new or revived rows, removed → erased (the user removed it; when every entry left at once, deleted or emptied file, they are first copied to `<file>.bak.dropped.<time>`, no copy no take-over). The agent's removals and rewrites stay as marked rows (`removed`, `superseded_by`); the user's (Settings ▸ Memory ▸ reset via `MemoryStore::reset`, a Star map delete or rewrite, a hand edit) are erased with their older versions and FTS text (`secure_delete`, WAL checkpoint). A read never writes a `.md` file; a write runs under the database's write lock and re-reads the file just before its rename (changed → start again; holds an entry the database doesn't know → refused). Database trouble → the files alone, as before |
| `tool.rs` | `tools/memory_tool.py`, `tools/write_approval.py` |
| `threats.rs` | `tools/threat_patterns.py` |
| **Prompts** (`crates/prompts`, text in `prompts/hermes`) | |
| `system.rs` | `agent/system_prompt.py`, `agent/prompt_builder.py` |
| `compress.rs` | `agent/context_compressor.py` |
| `review.rs` | `agent/background_review.py`, `agent/curator.py`, `agent/learn_prompt.py` |
| `titles.rs` | `agent/title_generator.py` |
| **Store** (`crates/store`) | |
| `projects.rs` | `hermes_cli/projects_db.py` |

The list of which modules cite what, straight from the source:

```bash
grep -rnoE '\b(apps/(desktop|shared)/src|tui_gateway|hermes_cli|agent|gateway|tools|plugins)/[A-Za-z0-9_./-]+\.(py|tsx?)' crates --include=*.rs
```

### Copied as data, not ported

| hermes-rs | Hermes | Refresh with |
|---|---|---|
| `prompts/hermes/` | prompt strings and tool schemas in `agent/`, `tools/` | `scripts/extract_hermes_prompts.py` (see `prompts/hermes/SOURCE.md`) |
| `skills/` | `skills/` | per `skills/SOURCE.md` |
| the webapp (`target/hermes-src/apps/desktop/dist-webapp`) | `apps/desktop`, `apps/shared` | `scripts/build-hermes-webapp.ps1` |

### Changes to Hermes's frontend

Hermes's frontend runs as is, with two kinds of changes, both kept outside Hermes's tree:

- `crates/desktop/patches/*.patch`: small source patches (fixes and performance changes),
  applied in name order by the webapp build. Each is described in `crates/desktop/patches/README.md`. Keep them few and small: every
  patch is a merge to redo when Hermes changes that file.
- `crates/desktop/plugins/hermes-rs/` (`plugin.js`, `hide.css`): a Hermes desktop plugin that hides
  what this backend doesn't implement. Prefer this over a patch when hiding is enough. The CSS
  targets Hermes's `data-tour` handles and icon classes, so check it after an update.

## Updating Hermes

1. **Fetch.** The clone may be shallow:
   ```bash
   git -C ../hermes-agent fetch --deepen=1000
   ```
   (or `--unshallow`), then pick the target commit or tag.

2. **Report.** Run:
   ```bash
   tools/node/node.exe scripts/hermes-update-report.mjs --to <commit>
   ```
   It writes `target/hermes-update-report.md` and changes nothing. It lists:
   1. every changed Hermes file that a hermes-rs module cites, with the module and the `git diff`
      command to read;
   2. JSON-RPC methods added, removed or changed in `gateway-contract.openrpc.json`, marking the
      ones `gateway.rs` serves;
   3. REST routes added or removed in `hermes_cli/web_routers`;
   4. how much of the frontend changed, and which `patches/*.patch` no longer apply;
   5. changed prompt and tool-schema sources, and changed skills.

3. **Port.** Work through section 1 of the report: read each Hermes diff and carry the change into
   the cited module. Port behavior changes; skip pure Python refactors. Update citations if Hermes
   moved code (a renamed file shows as `R` in the report).

4. **Protocol.** For changed methods that `gateway.rs` serves, match the new params and result
   shapes. New methods are features: implement them, or leave them answering "not here" (-32601).

5. **Patches.** Redo any patch that no longer applies against the new file, then regenerate it.

6. **Data.** Re-run `scripts/extract_hermes_prompts.py` if section 5 lists prompt files. Review
   the diff of `prompts/hermes/`, and refresh `skills/` if skills changed.

7. **Pin.** Write the new commit to `HERMES_COMMIT`. Update the commit named in
   `prompts/hermes/SOURCE.md`, `skills/SOURCE.md` and `vendor/SOURCES.md` if those were refreshed.

8. **Build and check.**
   ```powershell
   powershell -File scripts/build-hermes-webapp.ps1
   cargo test
   ```
   Then run the app with the missing-call log:
   ```powershell
   $env:RUST_LOG = "hermes_desktop::missing=info"; cargo run -p hermes-desktop
   ```
   Use the app and read the `hermes_desktop::missing` lines. Each is a call the new frontend makes
   that this backend doesn't answer.

9. **Hide.** Check that the features hidden by `plugins/hermes-rs` are still hidden. Hide any new
   feature the backend doesn't support.

One commit per update: the new `HERMES_COMMIT`, the ports, the patches and the data, so
`git log -- HERMES_COMMIT` lists every update with everything it took.
