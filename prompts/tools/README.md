# Tool schemas

The JSON schemas hermes-rs sends to the model. Each starts from the Hermes schema in
`../hermes/tools.*/` and is edited only where our implementation behaves differently, so the
model is never promised something the tool doesn't do. When a tool gains a feature, restore the
Hermes wording for it.

| Tool | Changes from Hermes |
|---|---|
| `todo_list` | none |
| `clarify` | none |
| `read_file` | No document extraction (Office, PDF, notebooks, SQLite) yet: feature C24. `vision_analyze` mention restored (M5). |
| `write_file` | No post-write syntax checks / lint yet. "this task" → "this session". |
| `patch` | Fuzzy matching is exact → whitespace/indentation/line-ending tolerant instead of Hermes's 9 strategies. No post-edit syntax checks yet. |
| `search_files` | Walks with the `ignore` crate instead of ripgrep; wording about ripgrep removed, gitignore/hidden/binary behavior stated. |
| `terminal` | No `pty`, `heartbeat`, `persist_on_release`; `notify` is boolean only (no line patterns); no automatic promotion to background above 600 s. Timeout restart behavior stated. |
| `process_manage` | No `handoff` (subagents arrive later); no cross-conversation result retention. |
| `web_search` | "configured backend" → "search backend" (the keyless tier only, no backend choice yet). |
| `web_extract` | "use a browser tool instead" → "say so rather than guessing" (no browser tools). |
| `vision_analyze` | none (native path only: on a model without vision the tool says so instead of using an auxiliary vision model). |
| `delegate_task` | Joined mode only (results return in the call; no background delivery, so no `action` list/steer/stop or `group`); no `output_schema` or `images` per task; flat (children never delegate); execute_code and cronjob references replaced; limits stated as fixed (10 parallel). |
