# Tool schemas

The JSON schemas hermes-rs sends to the model. Each starts from the Hermes schema in
`../hermes/tools.*/` and is edited only where our implementation behaves differently, so the
model is never promised something the tool doesn't do. When a tool gains a feature, restore the
Hermes wording for it.

**Short schemas (Phase 11 CP11.1).** `skill_manage`, `session_search`, `memory` and `delegate_task` are the exceptions: their `description` strings (top level and parameters) are shortened, because the tool list is sent with every request, and end with `Details: skill_view hermes-tools-guide`. The long text lives in the bundled skill `assets/skills/autonomous-ai-agents/hermes-tools-guide/SKILL.md` (a section per tool: when to use, every parameter, examples, gotchas, and the old description word for word). Rules for these four: change only `description` strings (the shape is pinned by `crates/prompts/tests/fixtures/tool_shapes.json`); anything that decides how a call must be *formed* (a required parameter, an enum, a limit, a recovery step) and every rule that keeps junk out of memory and skills (what not to store, no logs in skills, don't repeat a finished call) stays in the short text; rationale, examples and rare edge cases go in the skill; every parameter must be named in the skill (a test checks). Byte budgets are in `crates/prompts/src/tools.rs` (`SHORTENED`) and fail when exceeded. A schema that grows past its budget is a decision: raise the number and say why here.

**Deferred schemas (Phase 11 CP11.2).** In the `tokens` profile only the core tools' schemas ride in a request; `session_search`, `delegate_task`, `web_search`, `web_extract`, `vision_analyze`, `process_manage` and `clarify` (list: `hermes_prompts::deferred::BUILTIN`) are loaded on demand by `tool_search`, whose schema is `tool_search.json` here (not in `tool_names()`: only a deferred session has it). Nothing about these schemas changes; keep the descriptions as short as CP11.1 made them, because a load puts the whole schema into the context once. `skill_manage` is not offered to the main agent in `tokens` (the review keeps it); any description that tells the model to call it must be handled in `hermes_prompts::deferred` (today: `memory`'s "via skill_manage" clause).

| Tool | Changes from Hermes |
|---|---|
| `todo_list` | none |
| `clarify` | none |
| `read_file` | Phase 10: `paths` (several files in one call, at most 20, 160,000 characters in all, one `files` entry each with its own error) is ours, not Hermes's; `path` is no longer required (the single-path shape and its errors are unchanged). No document extraction (Office, PDF, notebooks, SQLite) yet: feature C24. `vision_analyze` mention restored (M5). `limit` states no number (no `default`/`maximum`): the line limit follows the budget profile, and a schema is fixed for a session while the profile can change. |
| `write_file` | Lint covers JSON/YAML/TOML in-process and .py/.js when python/node are installed (no LSP). "this task" → "this session". |
| `patch` | Fuzzy matching is exact → whitespace/indentation/line-ending tolerant instead of Hermes's 9 strategies. Syntax checks as `write_file`. |
| `search_files` | Phase 10: `group_by_file` (matches grouped per file as `N:text` / `N-text`, merged context) is ours. Walks with the `ignore` crate instead of ripgrep; wording about ripgrep removed, gitignore/hidden/binary behavior stated. |
| `terminal` | No `pty`, `heartbeat`, `persist_on_release`; `notify` is boolean only (no line patterns); no automatic promotion to background above 600 s. Timeout restart behavior stated. |
| `run_script` | Not from Hermes: `execute_code` is Python that calls Hermes's tools over RPC in a persistent kernel; ours is one PowerShell, Python (through uv) or bash script, once, with the script approved as a whole (see `crates/tools/src/script.rs`). The description tells the model what asks the user. |
| `process_manage` | No `handoff` (subagents arrive later); no cross-conversation result retention. |
| `web_search` | "configured backend" → "search backend" (the keyless tier only, no backend choice yet). |
| `web_extract` | "use a browser tool instead" → "say so rather than guessing" (no browser tools). |
| `vision_analyze` | none (native path only: on a model without vision the tool says so instead of using an auxiliary vision model). |
| `delegate_task` | Joined mode only (results return in the call; no background delivery, so no `action` list/steer/stop or `group`); no `output_schema` or `images` per task; flat (children never delegate); execute_code and cronjob references replaced; the parallel limit is not stated as a number (it follows the budget profile; the refusal names it). |
| `skill_manage`, `session_search`, `memory`, `delegate_task` (Phase 11 CP11.1) | Descriptions shortened to the call-forming facts plus a pointer to the `hermes-tools-guide` skill; the Hermes text is kept in the skill. `session_search`'s `session_id` description now says what the code does (alone = read the whole session; with `around_message_id` = scroll), where Hermes's said it "must be paired". Earlier changes to these four (Phase 3 and 9) stand: `delegate_task` joined mode only and no stated parallel number, `memory`'s `tier`/`tags`/`links`/`note`. |
| `tool_search` (Phase 11 CP11.2) | Not from Hermes. The `tokens` profile's loader: `names` (exact, at most 8) loads tools and returns their schemas, `query` lists matches. About 735 bytes. |
