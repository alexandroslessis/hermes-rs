# Trying the new version: a checklist (Backend v2, phases 0-15)

Written 2026-11-17 for the build of `main` at `0b92ce4`. Each section says what is new, how to
switch it on if it is off, and what you should see. The long per-phase versions are in
`docs/backend/progress/phase-N.md` (search "User checklist").

## 0. Before the first start

1. **Back up your data** (cheap insurance; the app also keeps its own backups of memory files):
   copy `%APPDATA%\hermes-rs` to somewhere else, e.g. `%APPDATA%\hermes-rs-backup-2026-11-17`.
   The new version moves MEMORY.md / USER.md into `memory.db` on first start (Phase 9) and adds
   side files (`requests.json`, `sandbox-labels.json`, ...). Nothing in `sessions.db` changes shape.
2. **Close the old app** (it locks its exe), then run the new one:
   `target\claude\release\hermes-desktop.exe` (built from `main` with the webapp from
   `HERMES_COMMIT`, so Settings shows the new fields: Budget profile, Review mode, Fallbacks).
3. **First look:** the app opens, your old chats are listed, a chat answers. Settings > Memory
   shows your old MEMORY.md / USER.md entries unchanged (Phase 9, step 1).
4. Defaults you get without doing anything: budget profile **Requests** (Phase 3), the repository
   overview in the first message (Phase 10), the local index (Phase 5), deferred tool schemas only
   under the **tokens** profile (Phase 11), sandbox **off** (Phase 15).

## 1. Safety rails (Phases 1 and 6)

1. Ask the agent: "write hello to `%APPDATA%\hermes-rs\x.txt`". It should refuse. Asking it to
   edit a file in your repo works as before.
   (Approval modes live in **Settings > Safety > approvals.mode**, not in the chat: `manual` = every
   risky action asks, `smart` = a reviewer model decides (what I call "Auto" below), `off` = never
   asks (what I call "Yolo" below). They apply from the next message. The "Waiting for your review"
   box only appears while something is waiting; with nothing pending it is hidden, so an empty
   Memory page is normal until step 2 has run.)
2. Set approvals.mode to `smart`, ask: "Search the web for the latest Rust release and tell me the version."
   Then: "Remember that I prefer short answers." Settings > Memory > Persistent memory shows
   "Waiting for your review (1)" with the diff; MEMORY.md is unchanged. **Apply** puts it in
   (a new chat knows it); **Reject** drops it. Same for a skill the agent wants to save after a web search. **USER**: No "Waiting for review" text shown. "Saved to memory" rainbow text appeared in chat and instruction was added to memory.md.
3. In Yolo after a web search, ask it to "delete the build folder": it runs, and a line
   "ran after reading untrusted content" appears (warn only, never blocks). In Auto, the approval
   card says "(this turn read web search results)" and doesn't offer "always allow". **USER**: Need better instructions here.
4. In a fresh chat that read nothing untrusted, "Remember that I like tea" saves at once.

## 2. Request budget (Phases 2, 3, 4)

1. The status bar shows **Requests used/limit**; it rises by the number of model calls in a turn. **USER**: Don't see it.
2. Settings > Providers > your provider has a **Daily request limit** field. Settings > Model >
   Main has **Budget profile**: `requests` (default, saves calls), `balanced` (as before),
   `tokens` (saves tokens, see section 6).
3. Settings > Model > Auxiliary > "Use a local model for background jobs": with it on (and a local
   Ollama / LM Studio model set), the request counter moves only on your own turns.

## 3. Self-improvement without burning requests (Phases 7 and 8)

1. Chat, correct the agent twice in plain words ("No, that's wrong." / "No, use pnpm."), then leave
   the chat idle 10 minutes or switch to another chat (starts after about 3 s). Exactly **one**
   background request is made; `%APPDATA%\hermes-rs\logs\desktop.log` says
   `review of ses_...: Done, 1 request(s)`. The lesson appears in Memory/Skills (or under
   "Waiting for your review"). A chat with no corrections makes **no** review request. **USER**: No request was made.
2. Settings > Model > Main > **Review mode**: empty = the profile's choice; `nudges` / `agentic` are
   the older Hermes behaviours for comparison.
3. **Skill curator:** create two near-duplicate skills (Capabilities > Skills), then Command Center >
   Maintenance > **Run now** next to *Skill curator*. One proposal "skill: edit A, archive B"
   appears under "Waiting for your review" with a diff. Apply merges (B goes to
   `%APPDATA%\hermes-rs\skills\.archive\B`); Reject changes nothing. A library without duplicates:
   "nothing to merge or repair; no request".
4. **Project skills:** skills in `<repo>/.agents/skills/<name>/SKILL.md` are picked up for chats in
   that repo and are never proposed or archived by the curator.

## 4. Memory v2 (Phase 9)

1. In a repo A session: "remember in this project's memory that release builds need the lto
   profile". A new session in A knows it; a session in repo B or outside a repo does not.
2. "Keep a note in the archive that the deploy password rotates on the first Monday." Later, ask
   "when does the deploy password rotation happen?": answered without a memory tool call (needs at
   least three real words in the question).
3. Remembering something it already has, reworded, says "similar to an existing entry #N"; the
   exact same thing again says "already stored".
4. Backups from the migration are next to your files (`MEMORY.md.bak.pre-memory-db.<time>`).
   The Star map joins a memory to the skill it links (`skill:name`).

## 5. Fewer round trips (Phase 10) and faster local lookups (Phase 5)

1. New chat in a **git repository**: "give me an overview of this repository". It answers from the
   first-turn context (branch, commits, status, tree, README) with the gauge going up by one. Your
   message shows only what you typed. A non-git folder gets no overview.
2. "Read Cargo.toml, README.md and AGENTS.md and summarise how they fit": the reads arrive together
   in one step. With approvals.mode `smart` a harmless `run_script` runs without asking (in `manual`
   every script asks; "Accept edits" exists only in ACP/Zed); a destructive script
   shows a `run_script (powershell)` card, and Deny leaves the folder alone.
3. Ask about something you did in an older chat: the local index finds it (relevant skills and
   past-session snippets are added automatically under `requests`).
4. To switch the overview off: quit, add `"context_pack": false` inside `"budget": { ... }` in
   `%APPDATA%\hermes-rs\desktop-config.json`.

## 6. Token mode (Phase 11)

1. Set Budget profile to **tokens**, start a new chat, ask a coding question. It works the same,
   but the request carries far fewer tool schemas (the agent loads rarer tools on demand through
   `tool_search`). Switch back to `requests` afterwards.

## 7. Provider fallback (Phase 12)

1. Have two providers set up (the second can be a local Ollama / LM Studio). Settings > Model >
   **Fallbacks** > Add fallback: pick the second provider and a model.
2. Set your main provider's Daily request limit to 3, send messages until the gauge shows 3/3, then
   send one more. The answer comes from the second provider and a grey line says it switched and
   that its cache starts empty. A wrong API key never switches (you see the key error).
3. Clean up: remove the fallback row, clear the daily limit, restart if the provider still says
   "used up".

## 8. Plumbing changes (Phases 13 and 14): nothing should look different

One sanity pass: start the app, open and switch between several chats while one is answering,
stop a running answer (Stop keeps the output so far), send a prompt while another session is busy,
quit with the app mid-turn and reopen: the chat is intact. Settings pages and the model picker
(context lengths, `:free` models showing cost 0) behave as before. If anything feels slower or
different, note which action; that is a regression.

## 9. Shell sandbox (Phase 15, off by default)

1. Close the app. In `%APPDATA%\hermes-rs\desktop-config.json` add
   `"terminal": { "sandbox": "low" }` (optionally `"sandbox_grants": ["D:\\datasets"]`). Start the app.
2. In a chat in a throw-away repo, type `/sandbox`: it says `terminal.sandbox: low` and that nothing
   is labelled yet.
3. Ask the agent to run `C:\Windows\System32\whoami.exe /groups`: the result carries
   "sandbox=low: ran in PowerShell, not bash" and the groups list "Mandatory Label\Low Mandatory Level".
4. `/sandbox` again lists the workspace, scratch, `sandbox-cache` and your grants as labelled.
5. Create a file in the repo (works) and `Set-Content $env:USERPROFILE\sandbox-test.txt x`
   (fails with "blocked by sandbox: ...; grant the folder..."). Check the file does not exist.
6. `git status`, `git commit` and a `cargo build` / `npm install` in a project there work (caches
   go to `%APPDATA%\hermes-rs\sandbox-cache`). Git Bash scripts and git hooks that need `sh` fail at Low.
7. Turn it off: close the app, set `"sandbox": "off"`, start, type `/sandbox remove-labels`
   ("Sandbox labels removed: N folder(s) put back as they were"). Optional: `icacls <repo>` before
   step 3 and after step 7 prints the same lines.

## 10. Known gaps to keep in mind

- The desktop app still ignores `background_review: false` in config.json (GAPS.md section 4).
- The sandbox has not been run end to end in the app or in ACP; treat step 9 as the first real test.
- Under `low`, a sandboxed command can still edit workspace files you later run yourself
  (`build.rs`, git hooks).
- Performance checks and profiling were on hold for the whole of this work; nothing was measured.

## 11. If something is wrong

Close the app, restore `%APPDATA%\hermes-rs` from the backup in step 0.1, and run the old exe.
`sessions.db` was never changed in shape, so old and new builds can read it. Send me the line
from `%APPDATA%\hermes-rs\logs\desktop.log` around the time it happened.
