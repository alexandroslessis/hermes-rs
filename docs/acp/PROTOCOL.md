# The ACP we speak

ACP **protocol version 1** (the stable one; Zed 1.26 sends `protocolVersion: 1` and its minimum is
V1). This file is the spec for the implementation: exact JSON, and how each message maps onto our
gateway. Source: agentclientprotocol.com (schema page, 2026-11-10), checked against Zed's client
(`../zed/crates/agent_servers/src/acp/v1.rs`) and Hermes's adapter (`../hermes-agent/acp_adapter/`).

When something here disagrees with what Zed actually sends, **Zed wins**: capture the real traffic
(Zed: command palette → `dev: open acp logs`, see [ZED.md](ZED.md)) and fix this file in the same
commit.

## 1. Framing and JSON-RPC

- Transport: the agent's **stdin** (client → agent) and **stdout** (agent → client), UTF-8.
- **One JSON-RPC message per line**, terminated by `\n`. A message must not contain a raw newline:
  serialize with `serde_json::to_string` (compact; newlines inside strings are escaped as `\n`).
  Accept `\r\n` on input (trim the `\r`).
- **stderr** is free-form logging; Zed shows it in its log. Never write anything but protocol
  lines to stdout (README decision D6).
- Messages:
  - Request: `{"jsonrpc":"2.0","id":<number|string>,"method":"…","params":{…}}` → must be answered
    with exactly one response with the same `id`.
  - Response: `{"jsonrpc":"2.0","id":…,"result":…}` or `{"jsonrpc":"2.0","id":…,"error":{"code":…,"message":"…","data":…}}`.
  - Notification: `{"jsonrpc":"2.0","method":"…","params":{…}}` (no `id`, no answer).
- **Both sides send requests.** Our outgoing request ids are our own counter (use numbers starting
  at 1; the client's ids are a separate namespace). A line with `id` and `result`/`error` but no
  `method` is a response to *our* request.
- Field names are **camelCase**; enum values (discriminators like `sessionUpdate`, `kind`,
  `status`, `stopReason`) are **snake_case**.
- Unknown fields must be ignored. Every params/result object may carry `_meta` (ignore it on
  input; we never need to send it).
- Paths are absolute; lines are 1-based.

Error codes:

| Code | When we use it |
|---|---|
| -32700 | A line that isn't JSON (answer with `id: null`, keep reading) |
| -32600 | JSON that isn't a JSON-RPC message |
| -32601 | A method we don't serve (log it under target `hermes_desktop::acp::missing`) |
| -32602 | Missing/invalid params (e.g. no `sessionId`, unknown session) |
| -32603 | Internal error; also a turn that failed (message = the error, written for a human) |
| -32000 | Authentication required (we never require auth; not used) |
| -32002 | Resource not found (a `session/load` of an unknown id) |
| -32800 | Request cancelled (`$/cancel_request`, not needed in v1) |

## 2. Methods we serve (client → agent)

### `initialize` (request)

Client sends (Zed, abridged):
```json
{"jsonrpc":"2.0","id":0,"method":"initialize","params":{
  "protocolVersion":1,
  "clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true,
    "auth":{"terminal":true},"elicitation":{"form":{},"url":{}},
    "_meta":{"terminal_output":true,"terminal-auth":true}},
  "clientInfo":{"name":"zed","title":"Zed","version":"1.26.2"}}}
```
We answer:
```json
{"jsonrpc":"2.0","id":0,"result":{
  "protocolVersion":1,
  "agentCapabilities":{
    "loadSession":true,
    "promptCapabilities":{"image":true,"audio":false,"embeddedContext":true},
    "mcpCapabilities":{"http":false,"sse":false},
    "sessionCapabilities":{"list":{}}
  },
  "agentInfo":{"name":"hermes-rs","title":"Hermes (rs)","version":"<CARGO_PKG_VERSION>"},
  "authMethods":[]}}
```
- Always answer `protocolVersion: 1`. If the client asks for a lower version than 1, still
  answer 1 (the client decides whether to disconnect).
- Remember from the request: `clientCapabilities.elicitation.form` present (→ clarify uses forms,
  §4.2) and `clientInfo.name` (log it).
- Advertise a capability only from the phase that implements it: Phase 1 sends
  `loadSession: false`, `image: false`, `embeddedContext: false`, `sessionCapabilities: {}`;
  Phase 3 turns them on. A capability advertised but not served breaks the editor.

### `session/new` (request)

```json
{"method":"session/new","params":{"cwd":"C:\\Users\\alexandros\\repos\\proj","mcpServers":[]}}
```
→ gateway `session.create {"cwd": <cwd>}` (provider and model: the desktop's default). Answer:
```json
{"result":{"sessionId":"<our session id>",
  "modes":{"currentModeId":"accept_edits","availableModes":[…]},      // Phase 4, omit before
  "configOptions":[…]}}                                          // Phase 4, omit before
```
- ACP `sessionId` **is** our store's session id (no mapping table).
- `mcpServers`: log their names at info and ignore (README "left out").
- No provider/model configured → error -32603, message:
  `"Hermes has no model set up. Open the Hermes desktop app ▸ Settings ▸ Providers, add one, then start a new thread."`
- After answering, send `available_commands_update` (Phase 3).

### `session/load` (request, Phase 3; needs `loadSession: true`)

```json
{"method":"session/load","params":{"sessionId":"…","cwd":"C:\\…","mcpServers":[]}}
```
1. gateway `session.resume {"session_id": …}` (also subscribes our outbox to the session).
   Unknown id → error -32002 `"Session not found: <id>"`.
2. **Before answering**, replay the whole transcript as `session/update` notifications (§3,
   [history](#history-replay)). Zed registers the thread before awaiting the response and expects
   the replay inside the request's lifetime (Hermes `server.py` `_session_response_fields`
   comment).
3. Answer `{"result":{"modes":…,"configOptions":…}}` (or `{}` before Phase 4).
- The `cwd` may differ from the stored one (the user opened the thread in another folder). Phase 3:
  keep the stored cwd, log the difference (Hermes moves the workspace; we don't, because the frozen
  system prompt names the folder).

### `session/list` (request, Phase 3; needs `sessionCapabilities.list: {}`)

```json
{"method":"session/list","params":{"cwd":"C:\\…","cursor":null}}
```
```json
{"result":{"sessions":[{"sessionId":"…","cwd":"C:/…","title":"…","updatedAt":"2026-11-10T09:30:00Z"}],
           "nextCursor":"<last sessionId of this page>"}}
```
- Source: `runtime.store().list_sessions(…)` (in `spawn_blocking`), newest first, archived ones
  left out (`archive.rs`).
- `cwd` filter: compare normalized (both to forward slashes, trailing slash trimmed,
  case-insensitive — Windows paths). Our store keeps `/` separators.
- Paging as Hermes (`server.py` `list_sessions`): page size 50; the cursor is the last
  `sessionId` of the previous page; an unknown cursor gives an empty page; `nextCursor` only when
  more remain.
- `updatedAt`: ISO 8601 UTC from the record's `updated_at` (ms since epoch).

### `session/prompt` (request)

```json
{"method":"session/prompt","params":{"sessionId":"…","prompt":[
  {"type":"text","text":"Fix the failing test"},
  {"type":"resource_link","uri":"file:///C:/Users/…/src/lib.rs","name":"lib.rs"},
  {"type":"resource","resource":{"uri":"file:///C:/…/a.rs","text":"fn main(){}","mimeType":"text/x-rust"}},
  {"type":"image","data":"<base64>","mimeType":"image/png"}]}}
```
1. Build the text and images ([prompt blocks](#prompt-blocks)).
2. If the text starts with `/` and the prompt is text-only → slash command path (Phase 3, §6).
3. Otherwise gateway `prompt.submit {"session_id", "text", "submission_id": <new uuid>}`
   (images first, Phase 3: `image.attach_bytes`, see `attachments.rs` for its params).
4. Stream every gateway event of this session as updates (§3) until `message.complete`.
5. Answer `{"result":{"stopReason": …}}`:

| `message.complete` `status` | Answer |
|---|---|
| `complete` | `{"stopReason":"end_turn"}` |
| `interrupted` | `{"stopReason":"cancelled"}` |
| `error` | error -32603 with `message` = the payload's `error` (after flushing open tool calls as `failed`) |

- Only one prompt per session at a time. If a prompt arrives while one is running (Zed cancels
  first, but don't rely on it), wait until the running one has answered, then submit.
- Every agent message chunk of one reply carries the same `messageId` (a uuid made per reply);
  a new reply after tool calls gets a new one. (Hermes `events.py` `AssistantMessageIdAllocator`.)

### `session/cancel` (notification)

```json
{"method":"session/cancel","params":{"sessionId":"…"}}
```
→ gateway `session.interrupt {"session_id"}`. No answer (it's a notification). The running
`session/prompt` then ends with `stopReason: "cancelled"`. Any of our permission requests still
open for that session will be answered by the client with `{"outcome":"cancelled"}` → answer the
gateway with `deny` / skip (§4).

### `session/set_mode` (request, Phase 4) and `session/set_config_option` (request, Phase 4)

See §5.

### Anything else

-32601. Liveness probes some clients send (`ping`, `health`) also get -32601 (Hermes `entry.py`:
clients treat that as "alive"); log them at debug, not as missing.

## 3. `session/update` notifications we send (agent → client)

Envelope: `{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"…","update":{"sessionUpdate":"<kind>", …}}}`

| `sessionUpdate` | Fields | Sent when |
|---|---|---|
| `agent_message_chunk` | `content: {type:"text", text}`, `messageId` | `message.delta` |
| `agent_thought_chunk` | `content: {type:"text", text}`, `messageId` | `reasoning.delta` |
| `user_message_chunk` | `content`, `messageId` | history replay only |
| `tool_call` | `toolCallId`, `title`, `kind`, `status`, `rawInput`, `locations`, `content` | `tool.start` |
| `tool_call_update` | `toolCallId` + changed fields (`status`, `content`, `rawOutput`, `title`, `locations`) | `tool.complete`; turn end for still-open calls; permission outcome |
| `plan` | `entries: [{content, priority, status}]` (whole plan each time) | `tool.complete` of `todo_list` |
| `available_commands_update` | `availableCommands: [{name, description, input?: {hint}}]` | after new/load (Phase 3) |
| `session_info_update` | `title?`, `updatedAt?` | `session.info` with a new title (Phase 3) |
| `usage_update` | `used`, `size` (tokens), `cost?` | after each turn (Phase 3) |
| `current_mode_update` | `currentModeId` | mode changed by us (Phase 4) |
| `config_option_update` | `configOptions` | model/effort changed by us (Phase 4) |

Enums: **ToolKind** `read | edit | delete | move | search | execute | think | fetch | switch_mode | other`;
**ToolCallStatus** `pending | in_progress | completed | failed`;
**PlanEntryStatus** `pending | in_progress | completed`; **PlanEntryPriority** `high | medium | low`.

**ToolCallContent** (array items in `content`):
- `{"type":"content","content":{"type":"text","text":"…"}}`
- `{"type":"diff","path":"C:\\abs\\file.rs","oldText":"…" | null,"newText":"…"}` (`oldText` null = new file)
- `{"type":"terminal","terminalId":"…"}` — not used (D4).

**ToolCallLocation**: `{"path":"C:\\abs\\file.rs","line":12}` (`line` optional).

### Gateway event → ACP update (translate.rs)

The gateway's events are frames on our Outbox:
`{"jsonrpc":"2.0","method":"event","params":{"type":"<event>","session_id":"…","payload":{…}}}`.
Ignore frames whose `method` isn't `event`. Payload shapes: `transcript.rs` (`TurnStream::agent_event`)
and `gateway.rs` `submit`; verify each against the code before relying on it.

| Gateway event | Payload (ours) | ACP |
|---|---|---|
| `message.start` | — | start a new `messageId` for the reply |
| `message.delta` | `{text}` | `agent_message_chunk` (skip empty text) |
| `reasoning.delta` | `{text}` | `agent_thought_chunk` |
| `tool.start` | `{tool_id, name, args}` | close the current `messageId` (next text is a new message); `tool_call` with `toolCallId = tool_id`, `status: "in_progress"`, title/kind/locations/rawInput from §3.1; for `write_file`/`patch` also the diff's old text (§3.2) |
| `tool.complete` | `{tool_id, name, result, result_text}` | `tool_call_update` `status: completed` or `failed` (§3.1), `content`, `rawOutput: result`; for `todo_list` also a `plan` |
| `approval.request` | `{prompt_id, command, description, choices, kind, execution_generation}` | `session/request_permission` (§4.1), on its own task |
| `clarify.request` | `{prompt_id, question, choices, multi_select, …}` | `elicitation/create` or a permission card (§4.2) |
| `approval.settled` / `clarify.settled` (if we ever emit them) | `{prompt_id}` | abort our pending card task for that prompt |
| `status.update` | `{kind: compacting/compacted, text}` | `agent_thought_chunk` with the text (Zed has no status line; Hermes shows it as a thought). Phase 3: if the client advertised `session.compaction`, send `compaction_update` instead (optional) |
| `session.info` | session info incl. title | `session_info_update` when the title changed (Phase 3) |
| `message.complete` | `{text, status, error, usage, persisted_turn}` | close open tool calls (`failed`, Hermes `flush_open_tool_calls`), `usage_update` (Phase 3), then answer the prompt (§2) |
| `subagent.*`, `background.complete`, `btw.complete`, others | — | ignore in v1 (log at debug). Phase 5 may show subagent progress as tool-call content |

### 3.1 Tool calls (tools.rs)

Kinds (port of Hermes `tools.py` `TOOL_KIND_MAP`, with **our** tool names — list them from
`prompts/tools/` and `crates/tools` before you write the table; unknown → `other`):

| Kind | Our tools |
|---|---|
| `read` | `read_file`, `skill_view`, `skills_list`, `vision_analyze`, `video_analyze` |
| `edit` | `write_file`, `patch`, `skill_manage` |
| `search` | `search_files`, `session_search` |
| `execute` | `terminal`, `process_manage` (Zed draws an execute card as a command; one with text instead of a terminal can't be expanded) |
| `fetch` | `web_search`, `web_extract` |
| `think` | — |
| `other` | `delegate_task` (its log and result must be expandable), `todo_list`, `memory`, `clarify`, everything else (MCP tools) |

Titles (Hermes `build_tool_title`; keep them short, the editor shows them as the card header):
- `terminal`: the command (first line, max 80 chars), e.g. `cargo test -p foo`
- `read_file`: `Read <path>` (+ ` (lines a–b)` when offset/limit given)
- `write_file`: `Write <path>`; `patch`: `Edit <path>`
- `search_files`: `Search "<pattern>"` (+ ` in <path>`)
- `web_search`: `Web search "<query>"`; `web_extract`: `Fetch <url>`
- `delegate_task`: `Delegate: <first task's goal, max 60 chars>` (+ ` (+N more)`); the call is `tasks: [{goal, context}]`
- anything else: the tool name with `_` → space.

Locations (Hermes `extract_locations`): from args keys `path` / `file_path` / `file` (resolved
against the session cwd when relative, made absolute with `\` separators), with `line` /
`offset` when present.

Status on complete: `failed` when the result says so (Hermes `_tool_result_failed`: the result
JSON has `"error"` or `"success": false`, or the text starts with `Error`); else `completed`.

Content on complete (Hermes `_build_tool_complete_content`):
- `write_file` / `patch`: the `diff` item (§3.2) and nothing else on success.
- `todo_list`: a short text summary; the plan goes in its own `plan` update.
- everything else: one text item with the result text, truncated to 5000 chars with
  `… (truncated)`; for `terminal` wrap it in a ```` ``` ```` fence.

### 3.2 Diffs for file edits

Zed renders a `diff` item as a reviewable diff. To build one we need the file before and after:
- On `tool.start` of `write_file` / `patch`: read the target file (in `spawn_blocking`, max 1 MB,
  `None` if missing or binary) and keep it in the turn state under `tool_id`.
- On `tool.complete` (success): read the file again → `{"type":"diff","path":abs,"oldText":before,"newText":after}`.
- `patch` with several files (check the tool's args schema): one diff item per file.
This is our own way (Hermes snapshots inside its edit tool); note it in `tools.rs`'s header.

### 3.3 Plan from the todo list

`todo_list` results hold the full list (check `crates/tools/src/todo.rs` for the JSON). Map each
item: `content` → `content` (prefix `[cancelled] ` for cancelled ones), status `pending` →
`pending`, `in_progress` → `in_progress`, `completed`/`cancelled` → `completed`; priority
`medium`. Send the whole list (Hermes `events.py` `_build_plan_update_from_todo_result`).

### History replay

For `session/load`, from `session.resume`'s `messages` (rows built by `transcript::messages`):

| Row | Updates |
|---|---|
| `role: user` | `user_message_chunk` with the text |
| `role: assistant` | `agent_thought_chunk` with its reasoning if any, then `agent_message_chunk` with the text (new `messageId` per row) |
| `role: tool` | `tool_call` (`status: completed`, title/kind/locations/rawInput from name+args) then `tool_call_update` with the result content (no diffs: we don't have the old text) |

Skip empty texts. A failed row must not fail the load (log and continue; Hermes does the same).

### Prompt blocks

(Hermes `content.py`.) Text parts are joined with blank lines:

| Block | Becomes |
|---|---|
| `text` | its text |
| `resource_link` with a `file://` uri | `@file:<absolute path>` on its own line (our `references.rs` expands it, like the page's `@` mentions); other schemes: `[<name>](<uri>)` |
| `resource` with `text` | ```` <name or path>:\n```<lang>\n<text>\n``` ```` (fenced, so the model sees the editor's unsaved content) |
| `resource` with `blob` | `[attached <name>: binary, not included]` |
| `image` | Phase 3: decoded bytes → `image.attach_bytes` before `prompt.submit`; Phase 1/2: rejected (we advertise `image: false`) |
| `audio` | not advertised; ignore with a log line |

`file://` URIs on Windows: `file:///C:/Users/x/a.rs` → `C:\Users\x\a.rs` (percent-decode; drop the
leading `/` before a drive letter). Use the `url` crate (already in Cargo.lock): `Url::to_file_path`.

## 4. Requests we send (agent → client)

### 4.1 Approvals → `session/request_permission`

(Hermes `permissions.py`, `gateway_server.py` `_answer_permission`.)
```json
{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{
  "sessionId":"…",
  "toolCall":{"toolCallId":"perm-<prompt_id>","title":"<description>: <command>","kind":"execute",
              "status":"pending","content":[{"type":"content","content":{"type":"text","text":"<description>\n$ <command>"}}],
              "rawInput":{"command":"…","description":"…"}},
  "options":[
    {"optionId":"allow_once","name":"Allow once","kind":"allow_once"},
    {"optionId":"allow_session","name":"Allow for session","kind":"allow_always"},
    {"optionId":"allow_always","name":"Allow always","kind":"allow_always"},
    {"optionId":"deny","name":"Deny","kind":"reject_once"}]}}
```
Client answers `{"result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}}` or
`{"result":{"outcome":{"outcome":"cancelled"}}}`.

| Outcome | gateway `approval.respond {"prompt_id", "choice"}` |
|---|---|
| selected `allow_once` | `once` |
| selected `allow_session` | `session` |
| selected `allow_always` | `always` |
| selected `deny`, unknown option, `cancelled`, an error response, or the client went away | `deny` |

Offer only the choices the payload's `choices` lists (ours: `APPROVAL_CHOICES` in `hub.rs`).
After the answer, send `tool_call_update {toolCallId: "perm-<prompt_id>", status: completed|failed}`
so the card doesn't stay pending (Hermes `await_permission`'s `send_update`).
Don't time out on our side: the runtime's own approval timeout applies.

### 4.2 Clarify questions

(Hermes `elicitation.py`, `gateway_server.py` `_answer_clarify`.)

If the client advertised `clientCapabilities.elicitation.form`, ask with `elicitation/create`:
```json
{"method":"elicitation/create","params":{"sessionId":"…","mode":"form","message":"<question>",
  "requestedSchema":{"type":"object","properties":{"answer":{"type":"string","enum":["a","b"]}},"required":["answer"]}}}
```
(no `enum` for a free question; `{"type":"array","items":{…enum…}}` for multi-select). Response
`{"action":"accept","content":{"answer":…}}` → that answer (an array joined with `, `, which
`hub.rs` splits again); `decline`/`cancel` → `""` (skip).

Otherwise (or if `elicitation/create` returns an error), a permission card: one option per choice
(`optionId: "choice-<i>"`, kind `allow_once`, name = the choice) plus `{"optionId":"skip","name":"Skip","kind":"reject_once"}`;
a free-text question without choices can only be skipped this way (say so in the card text:
`"Answer in your next message."`). Then gateway `clarify.respond {"prompt_id","answer"}`.

> Elicitation is newer than the rest of v1. If Zed 1.26 rejects or ignores `elicitation/create`,
> drop to the permission card for every client and note it here.

## 5. Modes and config options (Phase 4)

Modes (our own, not Hermes's edit-approval modes — we have no edit approvals):
```json
"modes":{"currentModeId":"accept_edits","availableModes":[
  {"id":"auto"},{"id":"manual"},{"id":"accept_edits"},{"id":"plan"},{"id":"yolo"}]}
// also as configOptions {"id":"mode","category":"mode"}: Zed ignores `modes` when it gets those
```
`session/set_mode {sessionId, modeId}` → `runtime.toggle_yolo` when it differs from
`runtime.yolo(id)` (check the gateway's `/yolo` path and reuse it); answer `{}`; unknown mode → -32602.

Config options (Zed renders `category: "model"` as its model picker, `thought_level` as effort):
```json
"configOptions":[
  {"id":"model","name":"Model","category":"model","type":"select","currentValue":"<provider>/<model>",
   "options":[{"value":"<provider>/<model>","name":"<model>","description":"<provider name>"}]},
  {"id":"effort","name":"Reasoning effort","category":"thought_level","type":"select","currentValue":"none",
   "options":[{"value":"none","name":"Default"},{"value":"low","name":"Low"},{"value":"medium","name":"Medium"},{"value":"high","name":"High"}]}]
```
- Model list: gateway `model.options {"session_id"}` (what the page's picker shows). Value format
  `<provider id>/<model id>` can be ambiguous when a model id contains `/` (OpenRouter): split at
  the **first** `/` only, and provider ids never contain `/` (check `providers.rs`).
- `session/set_config_option {sessionId, configId:"model", value}` → the same call the page makes
  to switch a session's model (read `gateway.rs` `switch_model` / `session.mutate`); answer with the
  full `configOptions` again. Effort → the page's effort call (`efforts` in the gateway).
- Switching model rebuilds the frozen system prompt (`Runtime::set_session_route`): applies from
  the next turn (ground rule 8). Refuse with -32603 while a turn is running.

## 6. Slash commands (Phase 3)

- After `session/new` / `session/load`: gateway `commands.catalog` → `available_commands_update`
  with `{name (without "/"), description, input: {hint}}` for each command that takes arguments.
  Leave out commands that only make sense in the page (anything opening a panel); keep the list in
  `server.rs` as a const with a comment.
- A text-only prompt starting with `/`: gateway `slash.exec {"session_id","command": <text without "/">}`
  (check the arm for the exact params). If it answers with output text, send it as one
  `agent_message_chunk` and end with `end_turn`. If the command started a turn (e.g. `/plan`,
  `/review`), the events stream as for a prompt: wait for `message.complete` the same way.
