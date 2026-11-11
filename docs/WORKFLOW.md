# Workflow: how to implement a feature or fix a bug

This is the procedure every feature in this repo was built with. Follow it in order; skipping the
investigation is how wrong shapes and invented behaviour get in.

**Two rules decide how closely to follow Hermes** (AGENTS.md rule 1): the *protocol* the page
speaks (methods, routes, event shapes) is always ported exactly; the *backend behaviour* (loop,
tools, review, curator, memory, prompts) is ours and may deviate. A deviation is written in the
module doc (what and why) and the `PORTING.md` row is marked `diverged`. The plan for the
deviations in progress is [backend/PHASES.md](backend/PHASES.md).

## 0. Orient (5 minutes)

- `git log --oneline -15` and `git status`: what changed lately, is the tree clean.
- Find the feature in [GAPS.md](../GAPS.md) and its section in
  [GAPS-PLAYBOOK.md](GAPS-PLAYBOOK.md).
- Make sure `target/hermes-src` exists (else run `scripts/build-hermes-webapp.ps1`); it's the
  frontend source at the pinned commit with our patches applied, and the contract is in it.

## 1. Investigate: what does the page call, and what does Hermes answer?

Work outside-in: the page decides the protocol, Hermes's server decides the behaviour. Do this
even when GAPS-PLAYBOOK.md has a recipe: recipes are starting points written before the code was
read (2.9's guessed the wrong routes and storage).

1. **The page.** Find the screen's component and the calls it makes:
   ```bash
   grep -rn "methodName\|/api/route" target/hermes-src/apps/desktop/src --include=*.ts --include=*.tsx
   ```
   The API layer is `apps/desktop/src/api/` (REST helpers, `canonical-protocol.ts` for RPC), the
   screens are under `apps/desktop/src/app/` and `src/components/`. Note what it does with the
   answer: which fields it reads, what it does on error / -32601 / 404 (often: hides the feature).
2. **The contract.** For JSON-RPC methods and events:
   ```bash
   tools/node/node.exe scripts/contract.mjs mcp.servers.list     # params + result, $refs resolved
   tools/node/node.exe scripts/contract.mjs --missing session.    # what we don't answer yet
   tools/node/node.exe scripts/contract.mjs --events subagent     # event payload shapes
   ```
   REST routes have no contract: the page's TypeScript types (`apps/desktop/src/types/`,
   `apps/shared/src/`) and Hermes's FastAPI routers are the spec.
3. **Hermes's implementation.** In `../hermes-agent`:
   - JSON-RPC handlers: `tui_gateway/methods_*.py`, `tui_gateway/server.py`.
   - REST: `hermes_cli/web_routers/*.py`, `hermes_cli/web_server*.py`.
   - Behaviour: `agent/`, `tools/`, `hermes_cli/` (config defaults in `hermes_cli/config.py`).
   Read the whole handler, including its error cases and defaults. Note what you will port and
   what you will leave out (anything hosted/profile/Nous-account related).
4. **Our side.** What already exists: `grep -rn` in `crates/` for the concept. Many features had
   the runtime part already (see the "Already in hermes-rs" column of GAPS.md); the work was wiring.
5. **Live evidence (optional but cheap).** Start your instance with the wire log (perf.ps1 does
   `RUST_LOG=info,hermes_desktop::wire=debug`), open the screen through `cdp.mjs`
   (`location.hash='#/…'`), and read `target/perf/app.out`: every frame in (`→`) and out (`←`),
   and `hermes_desktop::missing` lines for calls nobody answered.

Write down (in your head or a scratch file) the list of methods/routes, their shapes, and the
decisions. If a decision changes what the user experiences in a way Hermes doesn't (dropping a
sub-feature, a different default), it goes into the module doc and the commit body.

## 2. Implement

1. Logic in a feature module (`crates/desktop/src/<feature>.rs`) or the runtime/tools crate; the
   gateway gets thin arms. See [CONVENTIONS.md](CONVENTIONS.md#where-new-code-goes).
2. Match the contract's shapes exactly (field names, nesting, `ok` flags). When the contract and
   Hermes's Python disagree, the page's TypeScript decides (that's what will read it).
3. Errors as readable sentences (`SERVER_ERROR` / `{"detail": …}`).
4. Blocking work in `spawn_blocking`; long work spawned with progress as events.
5. Unhide the UI if it was hidden (`hide.css` selector, `plugin.js` list, patch 0005's
   `SHOWN_MODES`), and only the parts you now serve.
6. Prompts: if the feature sends a Hermes prompt, extract it (below), don't retype it.
7. Module header with the `Hermes:` line; PORTING.md map row.

### Prompts

Hermes's prompt strings and tool schemas are copied mechanically:

```powershell
~/.local/bin/uv.exe run --no-project python -I scripts/extract_hermes_prompts.py ../hermes-agent
```

Add the Hermes file to `FILES` in the script if it isn't there, run it, and load the text from
`prompts/hermes/<module>/<NAME>.txt` in `crates/prompts` (see `coding.rs`, `side.rs` for
`include_str!` use). **Gotcha:** if you run it against `target/hermes-src` instead of
`../hermes-agent`, line 1 of `prompts/hermes/SOURCE.md` records this repo's commit; set it back to
the value in `HERMES_COMMIT`.

### Frontend patches (last resort)

Only when the page can't work otherwise (it needs a native capability, or has a bug that breaks
us). Process in `crates/desktop/patches/README.md`: edit a copy of the file under
`target/hermes-src`, generate with
`diff -u --label a/<path> --label b/<path> orig new > crates/desktop/patches/NNNN-name.patch`,
rebuild the webapp from PowerShell, document the patch in the README.

## 3. Test

Unit tests → runtime test if the loop is involved → live check script → user checklist. Details
and templates: [TESTING.md](TESTING.md). Run the full set listed under "Before you commit".

## 4. Document

In the same commit:
- `GAPS.md`: mark the row `✅ **Done YYYY-MM-DD** (what exactly works; what doesn't)`.
- `PORTING.md`: a map row for each new module (hermes-rs path → Hermes files).
- `scripts/perf/README.md`: a sentence for the new check script.
- `crates/desktop/README.md` if a user-visible capability or the Status list changed.
- `crates/desktop/patches/README.md` for a new or changed patch.
- This `docs/` folder if you learned a pitfall (PITFALLS.md) or changed how something works.

## 5. Commit and report

Commit (format in [CONVENTIONS.md](CONVENTIONS.md#commits)); push only when the user asked. Report
to the user: what they can do now, what was left out and why, the checks that passed, and a
numbered checklist for their own app.

---

## When something goes wrong

**First read [PITFALLS.md](PITFALLS.md).** Then:

1. **Reproduce on your own instance**, never the user's. Get the exact frames from the wire log
   (`target/perf/app.out`) or write a 20-line check script that reproduces it.
2. **Find which layer is wrong** by comparing at each boundary: what the page sent (wire log `→`),
   what the gateway answered (`←`), what the provider received (an embedded mock provider in a
   check script logs every request), what the store holds (read `sessions.db` with a test or
   `rusqlite` snippet), what the side files say.
3. **Compare with Hermes.** Most "bugs" are a shape that differs from what Hermes sends. Diff our
   answer against the contract schema and Hermes's handler.
4. **Fix at the root**, not with a page-side workaround: if the page needs a field, send it; if a
   Tauri/WebView2 behaviour interferes, fix it in `main.rs`/a patch and document why.
5. **Add a test that fails before the fix.**
6. **Don't** loosen a check or delete a failing test to get green; don't `cargo fmt` the world;
   don't change the store schema; don't kill processes you didn't start.

Reports from the user come as observed behaviour in the app ("clicking X does nothing"). Map each
to a call (wire log), reproduce it, fix it, and give them a checklist line per symptom.

## When to ask the user

Ask (briefly, with a recommendation) when:
- A decision changes what they experience and Hermes's behaviour doesn't settle it (e.g. keep or
  drop a sub-feature, a default that costs tokens or money).
- Something needs their machine or accounts (signing in, a real API key, their data).
- The work would touch the frozen GPUI app, the store schema, or add a heavy dependency.

Don't ask about things the code, Hermes or these docs answer; pick the conventional option and
say so in the report.

## Performance

Out of scope unless asked ("no performance checks for the time being", 2026-11-04). Don't add work
to startup or the per-token streaming path; `scripts/perf/README.md` has the tools and past
findings if you're asked.
