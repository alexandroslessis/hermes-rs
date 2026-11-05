# Gaps playbook: how to implement each remaining gap

[GAPS.md](../GAPS.md) says **what** is missing; this file says **how** to build each item, in
the order recommended. Every recipe assumes the procedure in [WORKFLOW.md](WORKFLOW.md)
(investigate → implement → test → document → commit) and the conventions in
[CONVENTIONS.md](CONVENTIONS.md). Line numbers drift; search for the names.

State as of 2026-11-05 (Hermes `c419cf7c`): 69 of the contract's 257 JSON-RPC methods answered
(`tools/node/node.exe scripts/contract.mjs --missing` lists the rest; it matches names as strings
in `gateway.rs`, so a method answered only through another one, like `session.save` via
`session.export`, may show as missing).

## The generic recipe (every gap)

1. `scripts/contract.mjs <prefix>.` for RPC shapes; grep `target/hermes-src/apps/desktop/src` for
   the REST paths and how the screen uses the answers.
2. Read Hermes's handler and behaviour module (paths below). Decide what to port; anything about
   profiles, Nous accounts, hosted rooms or messaging platforms is dropped.
3. New module `crates/desktop/src/<feature>.rs` with a header (`Hermes:` line) and unit tests; thin
   arms in `gateway.rs` (`dispatch` for RPC, a prefix block in `rest` for REST).
4. Settings keys: add Hermes's default under `config_defaults()` in `rest.rs` (and a description
   or select override in `config_schema()` if the generated label is poor), read with
   `self.rest.config_value("a.b")`, push into the runtime in `Gateway::apply_settings()`.
5. Unhide the UI: delete the selector in `crates/desktop/plugins/hermes-rs/hide.css`, or the name
   in `plugin.js`'s `hide` list, or extend patch 0005's `SHOWN_MODES` (Capabilities tabs).
6. Live check `scripts/perf/<feature>-check.mjs`; user checklist; GAPS/PORTING/READMEs; commit.

---

## 1.8 MCP servers (Capabilities ▸ Connectors) — M

**What the user gets:** add a local (command) or remote (URL) MCP server, test it, switch it on and
off, see its tools; the agent can call them.

**Already here:** `crates/runtime/src/mcp.rs` — `McpManager` with `configure(BTreeMap<String,
McpServerConfig>)`, `reconnect(name)`, `statuses()` (`Disabled | Connecting | Connected{tools} |
Failed(msg)`), `tools()`, `tool_names()`, `call()`, `parse_servers(text)` (Hermes's
`mcp_servers` or a pasted `{"mcpServers": …}`), name prefixing `mcp__<server>__<tool>`.
`Runtime::mcp()` returns it; turns already include MCP tools (`lib.rs`, where `self_mcp.tool_names()`
is chained in). The GPUI app keeps servers in `config.json` `mcp_servers`
(`crates/app/src/config.rs`) and calls `runtime.mcp().configure(...)` on start and on save.

**What the page calls** (`apps/desktop/src/api/mcp.ts`, `app/capabilities/connectors/`):
- REST: `GET /api/mcp/servers` → `{servers: McpServerSummary[]}` (`name, transport, command,
  args, url, enabled, tools`); `PUT /api/mcp/servers` (replace all, body from `saveMcpServers`);
  `POST /api/mcp/servers` (add one); `DELETE /api/mcp/servers/{name}`;
  `POST /api/mcp/servers/{name}/test` → `{ok, error?, tools:[{name, description}]}`;
  `PUT /api/mcp/servers/{name}/enabled` `{enabled}`; `GET /api/mcp/catalog`,
  `POST /api/mcp/catalog/install`.
- RPC: `mcp.servers.list`, `mcp.servers.status`, `mcp.servers.set_api_key`; also in the contract:
  `mcp.servers.add/remove/test`, `mcp.catalog`, `mcp.servers.oauth.*`, `reload.mcp`.

**Hermes:** `hermes_cli/web_routers/mcp.py` (routes), `hermes_cli/web_server_mcp.py`
(`_mcp_server_summary`, `_normalize_mcp_server_create`), `hermes_cli/mcp_config.py` (config
read/write, 1,170 lines), `tui_gateway/mcp_rpc_helpers.py`, `tui_gateway/methods_tools*.py`.

**Steps:**
1. Storage: keep servers in `config.json` `mcp_servers` (same place and shape as the GPUI app, so
   both apps share them). `providers::Settings` doesn't round-trip unknown keys — write a small
   read-modify-write of that one key on the raw JSON (`serde_json::Value`) so providers and the
   rest of `config.json` are untouched. Secrets (`env`, `headers` values the user marks as keys):
   Credential Manager via `keyring` like provider keys, referenced from the config.
2. `crates/desktop/src/mcp.rs`: `list`, `save_all`, `add`, `remove`, `set_enabled`, `test`
   (connect a throwaway manager or `connect_transport`, list tools, drop it; with a timeout),
   `summary(name, config, status)` building `McpServerSummary`.
3. On startup (`Gateway::new` or `apply_settings`) and after every change: `runtime.mcp().configure(servers)`.
   Status for `mcp.servers.status` from `runtime.mcp().statuses()`.
4. Catalog: answer `GET /api/mcp/catalog` with `{entries: []}` (or the page's empty shape) unless
   you port Hermes's curated list (`mcp_catalog*.json` in Hermes) — it's Nous-curated; an empty
   catalog is fine. OAuth flows: leave unanswered (-32601) at first; remote servers with a bearer
   header cover most local use.
5. Unhide: patch 0005 `SHOWN_MODES = ['skills', 'toolsets', 'connectors']` (edit the patch,
   rebuild the webapp from PowerShell). Make sure the Connectors tab doesn't show SaaS connector
   accounts (`connectors.*`, not ours): hide those rows in `hide.css` if they appear.
6. Tool switches: MCP tools should appear in Capabilities ▸ Tools as their own toolset per
   server, or at least not break `toolsets.rs` (check `disabled_toolsets` handles unknown names).
7. Tests: unit (config round-trip keeps other keys; summary shapes), runtime (an MCP tool call
   through a test server — `mcp.rs` already has tests that spawn one; reuse that pattern), live
   check `mcp-check.mjs` (add a stdio server — a tiny Node MCP server script in `scripts/perf/` —
   test it, list, disable, enable, delete; a turn calls its tool via the embedded provider).

## 2.7 `/goal`, `/subgoal`, `/loop` — M (L with gates and judge tuning)

**What the user gets:** a standing goal the agent keeps working on turn after turn until an
auxiliary "judge" model says it's done (or a turn budget runs out); `/loop <interval> <prompt>`
re-runs a prompt on a timer while the app is open.

**Hermes:** `hermes_cli/goals.py` (1,780 lines: `GoalState`, `GoalManager.set/pause/resume/clear/
add_subgoal/evaluate_after_turn/next_continuation_prompt`, `judge_goal`, gates, wait barriers),
`hermes_cli/goal_command.py` (the slash command parser), `hermes_cli/loops.py` (738:
`LoopState`, `LoopManager`, `parse_loop_args`, `LOOP_COMPLETE` marker, self-paced backoff),
`tui_gateway/methods_session_control.py` (421: `session.control` actions
`goal.pause/resume/clear/unwait`, `subgoal.add/remove/clear`, `loop.pause/resume/stop`,
`heartbeat.*`; `session.control.read` snapshot; `session.control.update` event). Page:
`apps/desktop/src/store/session-control.ts`,
`app/session/hooks/use-message-stream/gateway-event/session-control.ts`.

**Current state:** `gateway.rs` answers `slash.exec` for commands starting with `goal` with an
empty output, because the page polls each session's goal. Replace that stub.

**Steps:**
1. Port the state machine, not the storage: `crates/runtime/src/goals.rs` (and `loops.rs`) with
   `GoalState`/`LoopState` as serde structs saved in a side file in the data folder
   (`session-control.json`, keyed by session id) — Hermes uses SessionDB `state_meta`, which our
   frozen schema doesn't have.
2. Prompts: add `hermes_cli/goals.py` and `hermes_cli/loops.py` to the extractor's `FILES`, run it,
   load `CONTINUATION_PROMPT_TEMPLATE`, the contract template and the judge prompts from
   `prompts/hermes/…` in `crates/prompts`.
3. The loop: after a turn finishes (`TurnFinished` handling in `Gateway::submit`), if the session
   has an active goal, call the judge (the aux model `auxiliary.goal_judge`, else the session's
   model; a one-shot request like `Runtime::side_question`) with the goal and the last response
   snippet (`_JUDGE_RESPONSE_SNIPPET_CHARS` = 4000). Continue → submit the continuation prompt as a
   normal user message (keeps prompt caching, as Hermes does). Done → mark done. Fail-open on
   judge errors; auto-pause after `DEFAULT_MAX_CONSECUTIVE_PARSE_FAILURES` (3) / transport
   failures (5); stop at `max_turns` (20).
4. `/loop`: a tokio timer per session (in the gateway; cancelled on session delete / app close);
   on tick, submit the prompt if the session is idle (queue otherwise); stop on `LOOP_COMPLETE`
   on its own line, `--times N`, or `loops.max_ticks` (100). Minimum interval 30 s.
5. RPC: `session.control.read` → snapshot shape from the contract
   (`scripts/contract.mjs session.control.read`); `session.control {action, …}` → run it, return
   the snapshot, emit `session.control.update`. Slash: `/goal <text>`, `/goal pause|resume|clear|status`,
   `/subgoal …`, `/loop …` via `slash.exec` (port `goal_command.py`'s parsing).
6. Leave out first: gates (shell commands that must pass), wait barriers (pid/session waits),
   heartbeats, contract drafting. Add later in that order.
7. Tests: runtime/unit tests for the state machine with a scripted judge (wiremock: judge replies
   `{"done": false}` then `{"done": true}`); live check with an embedded provider that answers
   both chat and judge requests (tell them apart by the system prompt).

## 2.9 Auxiliary model routing in Settings — S–M

**What the user gets:** Settings ▸ Model ▸ Auxiliary: pick which model writes titles, compression
summaries, background reviews, curator runs, approval judgements (and the goal judge, later).

**Already here:** runtime `AuxModels { compression, review, approval }` in `TurnOptions.aux`
(`crates/runtime/src/lib.rs`), each falling back to the turn's model; desktop
`providers::AuxModels { titles }` read from `config.json` `aux_models`. The GPUI app's
`AuxModelSettings` (`crates/app/src/config.rs`) has compression, background_review, curator,
approval, title_generation — the complete set, same file.

**Page:** sub-page `auxiliary` shows every key with prefix `auxiliary.` (Hermes's
`auxiliary.<task>.provider/model/...`), `app/settings/config-subpages.ts`.

**Steps:**
1. Decide the storage: the page writes `desktop-config.json` `auxiliary.<task>.{provider,model}`.
   Read those (falling back to `config.json` `aux_models` so GPUI settings still count) in one
   helper `aux_models(&self) -> hermes_runtime::AuxModels`.
2. Fill `TurnOptions.aux` in `submit` (today it's left default) and use the titles choice in
   `title_session`. Curator: pass its model where `runtime::curator` is started.
3. `config_defaults()`: add `auxiliary: {compression:{provider:"",model:""}, background_review:…,
   curator:…, approval:…, title_generation:…}`; in `config_schema()` make `provider` a select of
   configured provider ids and `model` free text (or follow Hermes's field component if it renders
   a model picker for these keys — check `config-field.tsx`).
4. Unhide `[data-tour='nav-config:model:auxiliary']` in `hide.css`. Keep only the tasks we run;
   hide the others if the page lists them.
5. Tests: unit for the merge/fallback; turn test that compression uses the aux model (the wiremock
   records `model` in the request); live check sets a key and reads `/api/config` back.

## 3.3 Archive / hide sessions — S

**Page:** `PATCH /api/sessions/{id}` with `{archived: bool}` (`setSessionArchived`), RPC
`session.archive` (keybinding "Archive current session"), list calls with
`archived=exclude|include|only` (`api/sessions.ts`), `session.set_hidden`.
**Current state:** `session_route` refuses `archived` with a 400 explaining it's unavailable.
**Hermes:** `tui_gateway/methods_session.py` (`session.archive`, `set_hidden`), SessionDB columns.

**Steps:**
1. Side file `session-flags.json` (`{id: {archived: bool, hidden: bool}}`) with a small `Flags`
   struct like `branch.rs` `Links` (load, set, get, remove on delete).
2. `session_route` PATCH: set the flag. `session.archive` / `session.set_hidden` RPC: same, plus
   lineage (branch children from `branches.json`) as Hermes does.
3. Filters: `session_list`/`session_rows` honour `archived=` query (default `exclude`) and leave
   hidden ones out of the default list. Check the sidebar's "Archived" view uses `only`.
4. `sessions.auto_archive` setting (see 3.4) can reuse this.
5. Delete the flag when a session is deleted.

## 3.4 Settings ▸ Sessions — S–M

**Page:** section `nav-sessions` (hidden in `hide.css`); read its component under
`app/settings/` to list the keys and routes (retention/auto-prune, export all, import).
**Steps:** serve its keys via `config_defaults()`; retention = a startup + daily task in the
gateway that archives (3.3) or deletes sessions older than N days (only if the user set it, off by
default); export = reuse `export.rs` per session into a folder chosen with the native save dialog
(patch 0008 bridge); import = our own export format, and point to Ctrl+K ▸ "Import session" for
Hermes's sessions (`hermes_import.rs`). Do 3.3 first.

## 3.5 Settings ▸ About — S

**Page:** `nav-about` (hidden); it reads `/api/health` (we serve `version`, `commit`) and the
update store (update checks, uninstall). **Steps:** check what the sub-pages call (wire log),
serve version/commit/data-folder info, answer update checks as "no updates" or hide that row,
hide uninstall (the NSIS uninstaller lives in Windows' Apps list). Then unhide the section.

## 3.6 Settings ▸ Workspace — S

**Page keys** (`config-subpages.ts`): Projects `terminal.cwd`, `desktop.repo_scan_*`; Shell
`terminal.persistent_shell`, `terminal.env_passthrough`; Files `code_execution.mode`,
`file_read_max_chars`. `/api/fs/default-cwd` exists already.
**Steps:** serve only what the runtime can honour: `terminal.cwd` (the default folder for new
sessions; use it in `create_session` when the page sends none), `file_read_max_chars` (pass to the
file tools' read cap in `hermes_tools::files`), `terminal.env_passthrough` (env vars passed to
the terminal tool). Leave `code_execution.mode` and `persistent_shell` hidden (no
`execute_code`; our shell isn't persistent). Unhide the section with per-sub-page selectors for
the rest.

## 3.7 Artifacts gallery — S

**Page:** sidebar entry hidden by `plugin.js` (`hide: ['messaging', 'artifacts', 'cron']`). The
gallery uses `getSessionMessages` (`/api/sessions/{id}/messages`, served by `stored_messages`)
and `listAllProfileSessions` (`/api/profiles/sessions`, served).
**Steps:** remove `'artifacts'` from `plugin.js`, rebuild the exe (it's `include_str!`'d),
restart your instance, open it (`location.hash` to its route; find the route in
`app/` routes), and read the wire log for unanswered calls. Likely nothing else is needed; if
it is, serve it from the store. Check that file links open through `/api/fs/*` (side panel).

## 3.8 Model fallbacks — M

**What the user gets:** when a request to the main model fails (rate limit, server error,
unreachable endpoint), the turn retries on the next model in a chain.
**Page:** sub-page `fallbacks`, field `fallback_providers` (`fallback-models-field.tsx`); Hermes's
config key is `fallback_model` (single dict or list; `hermes_cli/config.py`) read by
`hermes_cli/fallback_config.py` (167 lines: chain normalisation, same-backend dedup).
**Steps:**
1. Store the chain in `desktop-config.json` under the key the page writes; entries
   `{provider, model}` referring to our configured providers.
2. Runtime: classify errors (`crates/llm` already has retry/classification from Goose: find
   where `ProviderError` kinds are decided). On a retryable-but-exhausted or connection error for
   the main model, rebuild the provider for the next chain entry and retry the same request. Do
   it inside the turn (not by starting a new turn), and emit a visible notice (Hermes shows a
   status line) — a `message.delta` of a short note or a `status` event if the contract has one.
3. Don't fall back on 4xx caused by the request (context too long, bad tool schema): those fail
   on any model.
4. Tests: wiremock with two servers: the first returns 503 three times, the second answers; assert
   the turn completes and the second server got the request.

## 4. Known small issues

- **Steering a queued card** shows the message only after the reply. Patch 0001 area
  (`crates/desktop/patches/README.md` notes it). Find how Hermes's own backend echoes a steer
  (`tui_gateway/methods_prompt.py` steer path) and emit the same event when we accept it.
- **Tokens/s for old sessions:** needs stats we never stored; leave it (documented).
- **`/timeline`** (conversation rail): `/api/sessions/{id}/timeline` is read by the page
  (`api/client.ts`). Port Hermes's `hermes_cli/web_routers/sessions.py` timeline route from
  stored messages (user turns with offsets).
- **Maximize glyph** doesn't switch to "restore": the caption buttons (patch 0007) need the
  window's maximized state. Emit it from Tauri's window events through the window bridge, or
  answer a state query in `/api/hermes-rs/window`; then patch 0007 reads it.
- **Projects RPCs** `projects.archive`, `remove_folder`, `set_primary`, `for_cwd`, `get`:
  `projects.rs` has the store side; port each from `tui_gateway/methods_projects.py`.
  `for_cwd` also returns the git branch (`git.rs`).
- **`/api/profiles/sessions/sidebar`** (`api/sessions.ts:337`): a sidebar-sized session list;
  answer it from `session_rows()` with the query's limit. (The `/api/sessions/:id/messages` half
  of the GAPS line is done: `stored_messages`.)
- **Context window fallback 256k** (`FALLBACK_CONTEXT` in `gateway.rs`): when the provider's model
  list has no context length, ask the endpoint (Ollama `/api/show`, llama.cpp `/props`,
  LM Studio `/api/v0/models`) in `model_facts`, cache it, else keep 256k.
- **Other unanswered session RPCs** worth a look: `session.undo` (alias of rewind),
  `session.cwd.set` (change an idle session's folder), `session.most_recent`, `session.list`.

## 5. Later, if wanted

| Gap | How to approach it |
|---|---|
| **Skill hub install** (Capabilities ▸ Skills ▸ Discover) — L | Port `tools/skills_hub*.py` and `tools/skills_guard.py` (~4,800 lines). Start with GitHub-hosted skills (download a folder into `skills/`), run the guard's static scan (dangerous patterns, size limits) before installing, record origin in `.usage.json`. Hub search needs Hermes's index URL; check its terms. |
| **Browser automation** (`browser_*` tools over CDP) — L | Hermes: `tools/browser_*.py` (`browser_cdp_tool.py`, `browser_launcher.py`, `browser_session*.py`). Launch Edge/Chrome with `--remote-debugging-port` and a private profile, drive with CDP (copy `scripts/perf/cdp.mjs` ideas into Rust, `tokio-tungstenite`). Settings ▸ Browser is hidden; unhide with it. |
| **`execute_code`** — M | Hermes: `tools/code_execution_tool.py`. Run Python through `uv run` in a temp folder with a timeout; return stdout/stderr. Gate behind approvals. |
| **Tool search (deferred tools)** — M | Matters once MCP adds many tools: offer a `tool_search` tool and only the names, load schemas on demand. Hermes: grep `tool_search`. Breaks caching if done naively: keep the offered set stable within a session. |
| **Git worktrees per session** — M | Routes exist (`git_review.rs` worktree list/add). Missing: creating a session in a fresh worktree (`session.workspace.move`, project settings). |
| **`/cron` while the app runs** — M | Hermes: `cron/` (scheduler, jobs file). A tokio scheduler in the gateway, jobs in a side file, each run a new session (or a fixed one); unhide the sidebar `cron` entry in `plugin.js`. |
| **Voice** — L | STT/TTS providers; whisper.cpp / Piper locally. Unhide voice controls in `hide.css`. |
| **Local model management** — L | Ollama covers it; skip unless asked. |
| **LSP** — L | Diagnostics for edited files fed back to the agent; Zed's LSP client code is a source to copy. |
| **Multiple windows / pop-out session** — M | Tauri can open more windows on the same server; the Hub already routes events by subscription. Needs the page's pop-out route and a window-bridge action. |

## Other deferred items (from earlier sessions)

- **Claude Code-style per-command permissions** (allow `git status` always, ask for `rm`): extend
  `approvals` with a persisted allowlist (`command_allowlist` key, Settings ▸ Safety ▸ Approvals
  already lists it) checked in `hermes_tools::approval`.
- **PDF pages as images**: today PDFs attach as files (text via the read tool). Rendering pages
  needs a PDF rasteriser (pdfium binary): only if asked.
- **External memory providers** (Honcho, mem0…): Hermes's `memory_providers` router; out of scope
  for a local harness unless asked.
- **`spawn_tree.*` and `agents.list`**: the page doesn't call them; skip.
- **Reasoning text on imported Hermes sessions**: not carried over (`hermes_import.rs`); Hermes
  stores it in a column we could map to our reasoning content if wanted.
- **First-run without any provider**: untested path; the onboarding flow is off
  (`onboarding.state` → `run: false`). Check that a fresh data folder lands on Settings ▸ Providers
  with a clear message.
- **Dropped:** bundle trimming and Model-tab slowness (perf work, paused by the user).
