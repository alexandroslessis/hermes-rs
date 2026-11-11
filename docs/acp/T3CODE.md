# Using hermes-rs from t3code

t3code (pingdotgg/t3code, the "T3 Code (Beta)" desktop app) drives agents over ACP like Zed does.
Its data is in `%USERPROFILE%\.t3` (settings in `userdata\settings.json`, logs in
`userdata\logs\server.trace.ndjson`).

## Setup

In t3code: Settings ▸ Providers ▸ add an **ACP Registry** provider, source **Local**, and set the
command to `hermes-acp.exe`:

- installed app: `C:\Users\<you>\AppData\Local\Hermes\hermes-acp.exe`
- a build of your own: `...\hermes-rs\target\<dir>\release\hermes-acp.exe`

No arguments. In `settings.json` that is:

```jsonc
"providerInstances": {
  "hermes": {
    "driver": "acpRegistry",
    "displayName": "Hermes",
    "enabled": true,
    "config": {
      "source": "local",
      "commandPath": "C:\\Users\\<you>\\AppData\\Local\\Hermes\\hermes-acp.exe"
    }
  }
}
```

Leave **custom models** empty: the agent lists the desktop app's models itself, as
`<provider id>/<model id>` (for example `openrouter/openrouter/free`). A custom `openrouter/free`
would mean provider `openrouter`, model `free`.

## How t3code differs from Zed (what the agent does about it)

| t3code | hermes-rs |
|---|---|
| Sends `protocolVersion: 2` | Answers 1 (ACP v1); t3code speaks v1 to such agents |
| Starts a fresh agent process per thread (and for every readiness check) | The model list is kept in `<data>\acp-model-options.json`, so even a fresh process's `session/new` answer has the whole picker (t3code reads models only from that answer) |
| Checks the agent every ~15 min by creating a test session (`clientInfo.name` `t3-code-provider-test`) | A thread made by this connection that is closed, or still empty when the editor leaves, is deleted |
| No terminal cards (`_meta.terminal_output` not offered) | Terminal output is a text card at the end of the call |
| Elicitation: `url` only, no forms | Questions (clarify) come as a permission card with the choices |
| Reads `configOptions` (model, effort), slash commands from `available_commands_update` | Same as Zed |
| Has its **own** mode picker (approval required / auto-accept edits / full access) and answers our permission requests itself, by tool kind (reads allowed; edits by its rule; the rest ask or are allowed) | Its threads start in our **Manual** mode (`clientInfo.name` `t3-code…`), so every edit and command reaches t3code's policy; edit cards carry the file as `locations` (its workspace-write rule needs it). Our mode picker isn't shown there |
| Its **plan** mode switches the agent to a mode with id `plan` and back afterwards | Our Plan mode (edits refused, a plan under `.hermes/plans/` allowed) |
| **Revert** to an earlier turn: restores the files from its own git checkpoints, then starts a fresh agent session and hands it the conversation up to that turn | Nothing to do: it never asks the agent to rewind. The old Hermes thread stays (with the turns after the revert point); the new one continues |

A `spawn EFTYPE` in t3code's log means the command path is not a program (an empty file, a
folder, a `.cmd`): point it at a real `hermes-acp.exe`.

## Checklist (t3code)

1. Settings ▸ Providers: Hermes shows as ready (no "could not create a test session").
2. New thread with Hermes → the model picker lists the desktop app's models; pick one, send
   `Say hello in five words.` → streamed answer from that model.
3. `List the files in this folder.` → a tool call with the output.
4. t3code's own mode picker: **Approval required** + `Create hello.txt containing hi` → t3code
   asks; **Auto-accept edits** → the file is written without asking, `List the files` asks;
   **Full access** → nothing asks. Its **Plan** mode → no files change, a plan is presented.
5. `Ask me which language I prefer (Rust or Python) before answering.` → the question with choices.
6. `/` → Hermes's commands; `/usage` answers without a model call; `/undo` drops the last turn.
7. Revert: after two turns that change files, revert to the first → the files are back as they
   were, and the next message continues from that point (the agent doesn't know the second turn).
8. Close t3code; open the Hermes desktop app → the threads you used are there; no empty
   "Untitled" threads from t3code's checks pile up.
