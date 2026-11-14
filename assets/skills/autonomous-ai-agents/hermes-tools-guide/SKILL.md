---
name: hermes-tools-guide
description: Use when memory/skill_manage/session_search/delegate fails.
version: 1.0.0
author: hermes-rs (text adapted from Hermes Agent)
license: MIT
metadata:
  hermes:
    tags: [tools, reference, memory, skills, delegation, session-search]
    category: autonomous-ai-agents
---

# Hermes tools guide

The tool list keeps the descriptions of `skill_manage`, `session_search`, `memory` and `delegate_task` short
(they are part of every request). This guide has the rest: when to use each tool, every parameter, examples and
the gotchas. The schemas themselves (names, types, required parameters, enums) are exactly what the tool list
shows; nothing here changes how a call is formed, it explains it. The original long descriptions are kept
below word for word (they come from Hermes: `tools/skill_manager_tool.py`, `tools/session_search_tool.py`,
`tools/memory_tool.py`, `tools/delegate_tool.py`, adapted by hermes-rs).

Load it with `skill_view hermes-tools-guide` when a call of one of these four tools was rejected, or before a
non-trivial call (several operations, the `project` / `archive` memory tiers, scrolling a past session, several
parallel subagents). Do not load it for a routine call.

## skill_manage

### When to use

- After a task that took trial and error, a user correction, or a non-obvious workflow worth keeping: write it as a
  skill (`create`) or fix the skill that was wrong or incomplete (`patch`). A skill is procedural memory that loads
  only when it is relevant; a fact true for every session belongs in `memory` instead.
- A skill you loaded that was wrong, outdated or missing a step: `patch` it right then, before the task ends.
- Do not record one-off task progress, dates, PR numbers or incident stories: those rot. Write the rule and why.

### Parameters

The call has one parameter, `operations` (array, required). Each item names its skill in `name` (lowercase, hyphens/underscores, at most 64 characters), has an `action`, and carries only the fields of that action's shape. A field of another action's shape is invalid. A batch holds at most 20 operations.

**`create`**: required `name`, `action`, `content`.

- `content` (string): Full SKILL.md text (YAML frontmatter + markdown body).
- `category` (string): Optional category subdir (e.g. 'devops').

**`patch (old_string + new_string)`**: required `name`, `action`, `old_string`, `new_string`.

- `old_string` (string): Text to find (same matching semantics as the patch tool).
- `new_string` (string): Replacement; empty string deletes the match.
- `replace_all` (boolean): Replace all occurrences (default false).
- `file_path` (string): Optional supporting file (write_file's shape); default SKILL.md.

**`patch (whole-file content)`**: required `name`, `action`, `content`.

- `content` (string): Full SKILL.md rewrite (REPLACES the whole file; last resort).

**`write_file`**: required `name`, `action`, `file_path`, `file_content`.

- `file_path` (string): Path RELATIVE to the skill's own directory, e.g. 'references/api.md' — no leading slash, never absolute; first segment references/, templates/, scripts/, or assets/.
- `file_content` (string): Full text of the supporting file.

**`remove_file`**: required `name`, `action`, `file_path`.

- `file_path` (string): Supporting file (write_file's shape).

**`delete`**: required `name`, `action`.

- `absorbed_into` (string): Consolidation only: umbrella skill that absorbed this one (must exist).

A `patch` has two shapes: `old_string` + `new_string` (a targeted fix; preferred) or `content` alone
(replaces the whole SKILL.md; read it with `skill_view` first, last resort). `new_string` may be an empty string
(deletes the match). `replace_all` is only for `old_string`/`new_string`. `file_path` on a patch edits a supporting
file instead of SKILL.md; for a write it follows `write_file`'s shape.

### Examples

```json
{"operations": [{"name": "deploy-staging", "action": "patch",
  "old_string": "run build.ps1", "new_string": "run build.ps1 -Clean (a stale cache broke the last two deploys)"}]}
```

```json
{"operations": [
  {"name": "release-notes", "action": "create", "category": "devops",
   "content": "---\nname: release-notes\ndescription: Use when writing release notes. Groups commits by area.\n---\n# Release notes\n..."},
  {"name": "release-notes", "action": "write_file", "file_path": "references/format.md", "file_content": "..."}
]}
```

```json
{"operations": [{"name": "old-duplicate", "action": "delete", "absorbed_into": "release-notes"}]}
```

### Gotchas

- The whole call is atomic: one failing operation rolls every touched skill back; fix the one named in the error and
  resend the whole batch.
- `create` for a skill must come before any other operation on that skill in the same call. `delete` must be the
  only operation of its call (`absorbed_into`, when the skill's text was merged into an umbrella skill, must name a
  skill that exists).
- The skill description (in the frontmatter you write) is at most 60 characters and has the shape "Use when
  <trigger>. <behavior>." Never use the category name as the description: the index shows the category already.
- Write lessons, not logs: an imperative rule plus the reason, one rule per lesson, no PR numbers, dates or incident
  narration. Name `references/` files by topic and extend an existing one before adding another.
- `write_file` / `remove_file` paths are relative to the skill's directory, never absolute, with no leading slash, and
  the first segment is `references/`, `templates/`, `scripts/` or `assets/`.
- Existing skills are modified wherever they live (a skill in a read-only project folder is refused with a message that says what to do).
  `skill_view` of any skill shows the format conventions to copy.
- A session that fetched untrusted content (web pages, outside files) has its skill writes kept for the user's
  review ("Saved for review by the user"): that is not a failure, do not repeat the call.

### Original description (Hermes wording, as it was before it was shortened)

> Create, update or delete skills: your procedural memory for recurring tasks. The call is an operations array (a single edit is a list of one); it applies atomically — any failure rolls every touched skill back. Ops: create (full SKILL.md; lands in the skills directory; must precede that skill's other ops), patch (targeted old_string/new_string fix — preferred; content alone REPLACES the whole file, read it via skill_view() first), write_file/remove_file (supporting files), delete (sole op only). Existing skills are modified wherever they live. Description: max 60 chars, 'Use when <trigger>. <behavior>.', never the category name (the index shows it). Write lessons, not logs: imperative rule + why, no PR numbers/dates/incident narration, one rule per lesson, references/ named by topic (extend before adding). skill_view() shows format conventions.

The `operations` description was: Ordered ops; each names its target skill (lowercase, hyphens/underscores, max 64 chars). Each action is its own shape; another action's text slot is invalid.

## session_search

### When to use

- The user refers to a past conversation: "what did we do about X", "where did we leave Y", "that bug from last week".
- Before saying you do not remember something about the user's own earlier work.
- Not for facts the user gave you a direct source for (URL, file, contact, live system): inspect that first. Never
  conclude "not found" from history alone.

### The four shapes (picked by which arguments you pass)

| Shape | Arguments | Result |
|---|---|---|
| Discovery | `query` (plus optional `limit`, `sort`, `detail`, `after`, `before`, `exclude_session_ids`, `role_filter`) | the top matching sessions; the top one fully hydrated |
| Scroll | `session_id` + `around_message_id` (optional `window`) | the messages around an anchor |
| Read | `session_id` alone | the whole session (head and tail when it is long) |
| Browse | no arguments | recent sessions |

### Parameters (all optional; none is required)

- `query` (string): Search query (discovery shape). Keywords, phrases, or boolean expressions to find in past sessions. Omit to browse recent sessions. Ignored when `session_id` is set (scroll and read shapes).
- `limit` (integer; default `3`): Discovery shape only. Max sessions to return (default 3, max 10). Bump to 5–10 when the topic likely spans several sessions and you want to pick the right one to scroll into.
- `sort` (string; one of `newest`, `oldest`): Discovery shape only. Temporal bias on top of FTS5 ranking: omit for relevance-only (exploratory recall), 'newest' for "where did we leave X", 'oldest' for "how did X start".
- `detail` (string; one of `adaptive`, `full`; default `adaptive`): Discovery shape only. 'adaptive' (default) fully hydrates the top-ranked result and returns only the exact anchor message for lower-ranked results. 'full' returns bookends and the complete anchored window for every result.
- `after` (string): Discovery shape only. Inclusive lower bound on session start time. ISO date/datetime (e.g. 2026-06-01) or relative duration (7d, 24h, 2w = within the last N). Use only when the user names a time frame. sort is a ranking bias, not a bound.
- `before` (string): Discovery shape only. Exclusive upper bound on session start time. ISO date/datetime (a date-only value is midnight UTC that day) or relative duration (7d = older than a week). Use only when the user names a time frame.
- `exclude_session_ids` (array of string): Discovery shape only. Session ids already inspected this task. Those sessions are omitted so a later query explores instead of repeating the same hit. Cap 20.
- `session_id` (string): Scroll or read shape. Session to read inside. Use the session_id returned from a prior discovery call. With `around_message_id` it scrolls; alone it reads the whole session (head and tail when it is long). (Hermes's text said it must be paired with around_message_id; here it need not be.)
- `around_message_id` (integer): Scroll shape. Message id to center the window on — use match_message_id from a discovery result, or any id from a prior window.
- `window` (integer; default `5`): Scroll shape only. Messages to return on each side of the anchor (anchor itself always included). Clamped to [1, 20]. Default 5.
- `role_filter` (string): Optional. Comma-separated roles to include. Discovery defaults to 'user,assistant' (tool output is usually noise). Pass 'user,assistant,tool' to include tool output (debugging tool behaviour) or 'tool' to search tool output only.

### Examples

```json
{"query": "migration rollback", "sort": "newest"}
{"query": "deploy OR release", "after": "7d", "limit": 5}
{"session_id": "<id from a result>", "around_message_id": 1234, "window": 8}
{"session_id": "<id from a result>"}
{}
```

### Gotchas

- Several words are ANDed. No hits? Broaden with `OR` (`alpha OR beta`), quote an exact phrase, exclude with `NOT`,
  or prefix-match with `deploy*`.
- `sort` is a ranking bias, not a bound; use `after` / `before` for a time window, and only when the user named one.
- A compact (lower-ranked) result shows only the anchor message: to read more of it, scroll with its `session_id` and
  `match_message_id`. To scroll forward, call again with the LAST message's id; backward, the FIRST message's id (the
  boundary message repeats as an orientation marker).
- Tool output is left out unless `role_filter` includes `tool`. The current session is never returned.
- `exclude_session_ids` (at most 20) keeps a second query from returning the sessions you already inspected.
- Results are stored messages, not summaries; nothing here calls a model.

### Original description (Hermes wording, as it was before it was shortened)

> Search or read past Hermes conversations (FTS5), or scroll inside one. Four shapes, picked by args: `query` = discovery (top-N matching sessions, top result fully hydrated); `session_id` + `around_message_id` = scroll (window of messages around an anchor); `session_id` alone = read a whole session; no args = browse recent sessions. Results are actual DB messages, no LLM. Searches conversation history ONLY — when the user gave a direct source (URL, file, contact, live system), inspect that first; never conclude 'not found' from history alone. Use for questions about past conversations: 'what did we do about X', 'where did we leave Y'.

## memory

### When to use

- A fact that applies to every future session: who the user is, a stable environment fact, a standing convention with
  no task home. Memory is injected into every turn, so it stays small (the `memory` target has a limit of 2,200
  characters, `user` 1,375, a project 1,500 by default).
- What you learn while doing a task (procedures, pitfalls, the user's preferences and corrections for that kind of
  work) belongs in that task's skill via `skill_manage`, where it loads only when relevant.
- Skip: trivial or obvious facts, easily rediscovered facts, raw data dumps, task progress, completed-work logs,
  temporary TODO state (use `session_search` for those). Reusable procedures belong in a skill, not in memory.

### Parameters

Top level: `target`, `operations` are required.

- `target` (string; one of `memory`, `user`): Which memory store: 'memory' for personal notes, 'user' for user profile.
- `operations` (array of object): Changes applied atomically in one call against the final char budget; a single change is a one-item list.

Each item of `operations` (`action` required):

- `operations[].action` (string; one of `add`, `patch`, `remove`, `note_read`)
- `operations[].content` (string): Required for 'add' and 'patch'. For 'add', the new entry. For 'patch', the text that replaces old_text; the rest of the entry is kept.
- `operations[].old_text` (string): REQUIRED for 'patch' and 'remove': text from one entry. 'patch' replaces exactly this span (it must occur once in the entry); 'remove' deletes the whole entry. Omit for 'add'.
- `operations[].tier` (string; one of `core`, `project`, `archive`): Where the entry lives (default core). Applies to every action, including finding the entry for patch/remove/note_read.
- `operations[].tags` (array of string): Up to 8 short labels, for add or patch.
- `operations[].links` (array of string): Up to 8 of 'skill:<name>', 'file:<path>', 'session:<id>', 'memory:<id>', for add or patch.
- `operations[].note` (string): Long-form body (project/archive tiers only), for add or patch.

Per action: `add` needs `content`; `patch` needs `old_text` and `content`; `remove` needs `old_text`;
`note_read` needs `old_text` (and `tier` when the entry is not core). `tags`, `links` and `note` go on `add` or `patch`.

### Tiers

- `core` (the default; omit `tier`): MEMORY.md / USER.md, in every prompt, with the limits above.
- `project`: notes for the current git repository only, shown in chats in that repository (small limit).
- `archive`: kept without a size limit, not shown in every chat; entries stay one-line summaries. Use it for what is
  worth keeping but not worth a prompt slot. A long body goes in `note`, stored in a file linked from the entry; read
  it with `action: "note_read"` + `old_text` + `tier`, as a call by itself.
- One call writes one tier. `tags` and `links` (`skill:<name>`, `file:<path>`, `session:<id>`, `memory:<id>`) are
  optional labels, at most 8 each.

### Examples

```json
{"target": "user", "operations": [{"action": "add", "content": "Prefers short answers; strong in Python, new to Rust."}]}
```

```json
{"target": "memory", "operations": [
  {"action": "remove", "old_text": "Python 3.9 on PATH"},
  {"action": "add", "content": "Python is run through uv (the PATH one is the Store stub)."}
]}
```

```json
{"target": "memory", "operations": [{"action": "add", "tier": "archive",
  "content": "Release process for the app (2026)", "note": "Step by step ...", "tags": ["release"], "links": ["skill:release-notes"]}]}
```

```json
{"target": "memory", "operations": [{"action": "note_read", "tier": "archive", "old_text": "Release process for the app"}]}
```

### Gotchas

- Make ALL changes to one target in ONE call. The batch is atomic and the character limit is checked on the final
  result, so a single call can remove or patch stale entries to free room AND add the new ones.
- If a store is full, an add is rejected with the current entries shown. Reissue ONE batch that removes or shortens
  enough stale entries and adds the new one together. The response reports current/limit characters and confirms
  completion: one batch call finishes the update, do not repeat it.
- `old_text` must identify one entry: for `patch` it must occur exactly once in the entry; a text matching several
  different entries is refused: quote more of the one you mean.
- `note_read` must be a call by itself (it reads, it does not write).
- Adding a near-duplicate of an entry is answered in the same result with the existing entry: patch that one instead.
- A session that fetched untrusted content has its memory writes kept for the user's review; that is not a failure.

### Original description (Hermes wording with the hermes-rs tier paragraph, as it was before it was shortened)

> Save durable facts to persistent memory that survive across sessions. Memory is injected into every future turn, so keep entries compact and high-signal.
>
> HOW: make ALL your changes to one target in ONE call via an 'operations' array (each item: {action, content?, old_text?}). The batch applies atomically and the char limit is checked only on the FINAL result — so a single call can remove/patch stale entries to free room AND add new ones, even when an add alone would overflow. The response reports current/limit chars and confirms completion; one batch call finishes the update, so don't repeat it.
>
> WHEN: only for facts that apply to EVERY session regardless of task: who the user is, stable environment facts, standing conventions with no task home. Anything learned while doing a task (procedures, pitfalls, and the user's preferences and corrections for that kind of work) belongs in the task's skill via skill_manage, where it loads only when relevant; memory is injected into every turn and must stay small.
>
> IF FULL: an add is rejected with the current entries shown. Reissue as ONE batch that removes or shortens enough stale entries and adds the new one together.
>
> TARGETS: 'user' = who the user is (name, role, preferences, style). 'memory' = your notes (environment, conventions, tool quirks, lessons).
>
> SKIP: trivial/obvious info, easily re-discovered facts, raw data dumps, task progress, completed-work logs, temporary TODO state (use session_search for those). Reusable procedures belong in a skill, not memory.
>
> TIERS (optional; omit for 'core', as above): 'project' = notes for the current git repository only, shown in chats in that repository (small limit); 'archive' = stored without a size limit, not shown in every chat (entries stay one-line summaries). 'note' (project/archive only) on an add or patch stores a long body in a file linked from the entry; read it with action 'note_read' + old_text + tier, as a call by itself. 'tags' and 'links' ('skill:name', 'file:path', 'session:id', 'memory:id') are optional labels. One call writes one tier.

## delegate_task

### When to use

- Reasoning-heavy subtasks, work that would flood your own context (reading many files, long searches), or independent
  workstreams that can run in parallel. Only each child's final summary returns to you.
- Not for mechanical multi-step work with no reasoning (do it yourself with `terminal` or `run_script`), a single tool
  call (call the tool), tasks that need the user (children cannot ask questions), or durable work that must survive the
  session (`terminal` with `background` and `notify`).

### Parameters

One parameter, `tasks` (array, required, at least one entry). Each entry needs `goal`; `context` is optional.

- `tasks` (array of object; required): The task(s), run in parallel (how many at once follows the user's budget profile: a longer list is refused, and the error states the limit). Each entry spawns one subagent with isolated context and terminal session; a single task is a one-entry array. Required.
- `tasks[].goal` (string): What this subagent should accomplish. Be specific and self-contained — it knows nothing about your conversation history.
- `tasks[].context` (string): Background THIS child needs: file paths, error messages, constraints. Each child sees only its own context — repeat shared background in every task that needs it.

### Examples

```json
{"tasks": [
  {"goal": "Find every caller of parse_config and list file:line", "context": "Repo at C:\\work\\app; Rust; ignore target/."},
  {"goal": "Read docs/DESIGN.md and summarise the caching rules in 10 lines", "context": "Only that file."}
]}
```

### Gotchas

- Children know nothing of this conversation: pass everything needed in `context` (file paths, error messages,
  constraints), including any required output language, tone or style (for example "respond in Chinese"). Each child sees
  only its own `context`: repeat shared background in every task that needs it.
- A child's summary is a SELF-REPORT, not a verified fact: a child claiming "uploaded successfully" or "file written" may
  be wrong. For external side effects (uploads, remote writes, publishing), require a verifiable handle (URL, ID,
  absolute path) and verify it yourself before telling the user the operation succeeded.
- Children cannot call `delegate_task`, `clarify` or `memory`, and inherit your model.
- Parallel children are joined: their results return in this tool call. Stop, or closing the session, halts running
  subagents; each returns an "interrupted" completion with partial output.
- How many tasks run at once follows the user's budget profile; a longer list is refused and the error states the
  limit.

### Original description (Hermes wording, as it was before it was shortened)

> Spawn isolated subagents for parallel or context-heavy work. Each gets its own conversation, terminal session, and toolset, and only its final summary returns to you. Pass every task in `tasks` — one entry spawns one subagent, several run in parallel (limit in the tasks description).
>
> Parallel children are joined and their results return in this tool call.
>
> USE FOR: reasoning-heavy subtasks, work that would flood your context, or independent parallel workstreams.
> DO NOT USE FOR (use these instead):
> - Mechanical multi-step work with no reasoning needed -> do it yourself with terminal
> - A single tool call -> call the tool directly
> - Tasks needing user interaction -> subagents cannot ask questions
> - Durable work that must survive this session -> terminal(background=True, notify=True); Stop or closing the session halts running subagents; each returns an 'interrupted' completion with partial output.
>
> RULES:
> - Children know nothing of this conversation: pass everything needed via 'context', including any required output language, tone, or style (e.g. "respond in Chinese").
> - Child summaries are SELF-REPORTS, not verified facts: a child claiming "uploaded successfully" or "file written" may be wrong. For external side effects (uploads, remote writes, publishing), require a verifiable handle (URL, ID, absolute path) and verify it yourself before telling the user the operation succeeded.
> - Children cannot close tracked work: a child asked to close it returns findings instead; the parent applies the transition.
> - Children cannot call delegate_task, clarify, or memory.
> - Children inherit the parent model.
