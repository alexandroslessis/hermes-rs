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

| hermes-rs | Hermes |
|---|---|
| **Desktop backend** (`crates/desktop`) | |
| `server.rs` | `hermes_cli/web_server_dashboard.py` (page globals, token), `tui_gateway/ws.py` |
| `gateway.rs` | `tui_gateway/server.py`, `methods_prompt.py`, `methods_session.py`, `ws.py`; `agent/context_breakdown.py`; the page's side: `apps/desktop/src/api/canonical-protocol.ts`, `apps/shared/src/gateway-contract.openrpc.json` |
| `hub.rs` | event fan-out: `apps/desktop/src/api/canonical-protocol.ts` |
| `transcript.rs` | `tui_gateway/methods_session.py`, `tui_gateway/prompt_turn.py` |
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
| `export.rs` | `session.export` (`tui_gateway/methods_session.py`), the page's `/save` |
| `maintenance.rs` | `hermes_cli/web_routers/status.py` (`/api/curator*`, `/api/actions/*`), `hermes_cli/web_routers/ops.py` (`/api/memory*`), `agent/curator.py` (the schedule) |
| `skills.rs` | `hermes_cli/web_routers/skills.py` (local routes), `hermes_cli/skills_config.py`; the hub (`tools/skills_hub*.py`, `tools/skills_guard.py`) not ported |
| **Runtime** (`crates/runtime`) | |
| `commands.rs` | `hermes_cli/cli_commands_mixin.py`, `tools/working_diff.py` |
| `compress.rs`, `ops.rs` | `agent/context_compressor.py`, `agent/turn_tool_validation.py`, `tools/budget_config.py` |
| `guards.rs` | `agent/repetition_guard.py`, `agent/tool_guardrails.py` |
| `review.rs` | `agent/background_review.py` |
| `curator.rs` | `agent/curator.py` |
| `subagents.rs`, `delegate.rs` (events) | `tools/delegate_tool_registry.py`, `tui_gateway` `subagent.*` and `delegation.status/pause`; `spawn_tree.*` not ported |
| **Tools** (`crates/tools`) | |
| `shell.rs` | `tools/terminal_tool.py` |
| `process.rs` | `tools/process_registry.py` |
| `search.rs` | `tools/file_tools.py`, `tools/file_operations_search.py` |
| `lint.rs` | `tools/file_operations_lint.py` |
| `checkpoints.rs` | `tools/checkpoint_manager.py` |
| `approval.rs` | `tools/approval_detection.py`, `tools/approval_smart.py` |
| `clarify.rs` | `tools/clarify_tool.py` |
| `todo.rs` | `tools/todo_tool.py` |
| `recall.rs` | `tools/session_search_tool.py` |
| `vision.rs` | `tools/vision_tools.py`, `agent/image_routing.py` |
| `web.rs` | `plugins/web/keyless_mcp.py` |
| `redact.rs` | `agent/redact.py` |
| `references.rs` | `agent/context_references.py` |
| `hints.rs` | `agent/subdirectory_hints.py` |
| `skills.rs` | `tools/skills_tool.py`, `agent/skill_commands.py`, `agent/skill_utils.py`, `agent/prompt_builder.py` |
| `skill_manage.rs` | `tools/skill_manager_tool.py`, `tools/skill_manager_batch.py` |
| `bundles.rs` | `agent/skill_bundles.py` |
| `curator.rs` | `agent/curator.py`, `tools/skill_usage.py` |
| **Memory** (`crates/memory`) | |
| `store.rs` | `tools/memory_tool_store.py` |
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
