# Changelog

## 0.4.0 (2026-11-17): Backend v2

A new agent backend: it uses far fewer model requests, improves itself without being asked every
time, keeps its memory in a database, and can run shell commands in a low-privilege sandbox.
Still work in progress: a few parts are new and lightly tested in the real app (marked below).
The chat database (`sessions.db`) keeps its shape, so 0.3.x and 0.4.0 read the same chats.

### Saving requests (for providers that limit requests per day)
- **Request gauge and daily limit.** The status bar shows requests used / limit; Settings > Providers
  has a "Daily request limit" per provider.
- **Budget profiles** (Settings > Model > Main): `requests` (default, fewest calls), `balanced`
  (as in 0.3), `tokens` (fewest tokens: rarely used tools are loaded on demand).
- **Fewer round trips.** The first message in a git repository carries a repository overview
  (branch, recent commits, changes, file tree, README); the agent can run a whole PowerShell, Python
  or bash script in one call (`run_script`), read several files at once, and batch independent tool
  calls.
- **Background jobs off your quota.** Titles are made locally; "Use a local model for background
  jobs" (Settings > Model > Auxiliary) keeps review and curator calls on an Ollama / LM Studio model.
- **Provider fallback.** Settings > Model > Fallbacks: when a provider's daily quota is used up (or it
  is down), the turn continues on a second provider, with a note that its cache starts empty.

### Self-improvement
- **Review rewritten:** one background request after a session in which you corrected the agent
  (or went idle), not a review every few turns. Settings > Model > Main > Review mode.
- **Skill curator:** finds near-duplicate or failing skills and proposes a merge or repair (Command
  Center > Maintenance > Run now). Proposals wait for your Apply / Reject.
- **Skill outcomes and sources:** skills record where each change came from and how they performed.
- **Project skills:** `<repo>/.agents/skills/` is read for chats in that repository.
- **Safer memory and skills:** after the agent read untrusted content (web pages, search results), its
  memory and skill changes wait under "Waiting for your review" in Settings > Memory and Capabilities >
  Skills instead of being saved. Yolo mode warns and never blocks.

### Memory
- Memory lives in `memory.db` with tiers: core (MEMORY.md / USER.md as before), per-project notes,
  and an archive found by a local index. Duplicates are caught ("similar to an existing entry").
  Old memory files are backed up next to the originals on first start.
- A local full-text index finds relevant skills and notes for each message.

### Safety
- Path rules stop the agent writing into the app's own data folder; secrets are removed from the
  environment of the commands it runs.
- **Shell sandbox (new, off by default):** `"terminal": { "sandbox": "low" }` in
  `desktop-config.json` runs commands with a low-integrity token: they can write only to the workspace,
  scratch and folders you grant, and cannot write to your profile. Git Bash is replaced by PowerShell
  while it is on. `/sandbox` shows the state, `/sandbox remove-labels` undoes every folder label.
  Not yet run end to end in the app.

### Under the hood
- Conversations share messages instead of copying them; the store has one writer thread; blocking work
  is off the async workers; each session runs as its own actor; the agent loop is a plain loop; the
  gateway's methods are one typed list.
- Removed about 8,000 lines of unused Goose code and cut the embedded model catalog from 5.8 MB to
  335 KB (context lengths and prices resolve the same).
- Fixed: test runs used to leave hundreds of `.tmpXXXXXX` folders in `%TEMP%` (300 GB on one
  machine). Tests now clean up after themselves; `scripts/clean-test-temp.ps1` sweeps old ones.

### Known gaps
- The desktop app ignores `background_review: false` in `config.json`.
- A sandboxed command can still edit workspace files you later run yourself (`build.rs`, git hooks).
- Performance was not measured during this work.

## Earlier versions
See the GitHub releases (v0.1.0 to v0.3.3).
