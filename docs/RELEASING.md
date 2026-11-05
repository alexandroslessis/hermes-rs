# Releasing: the Windows installer

The app ships as an NSIS installer built by Tauri's CLI and published on the `installer` branch
(no `gh` on this machine, so no GitHub Releases from here). Build and publish only when the user
asks for it.

## Build

Prerequisites: the webapp built at the current `HERMES_COMMIT` (`scripts/build-hermes-webapp.ps1`,
from PowerShell), Node in `tools/node`.

```powershell
# from the repo root, in PowerShell
$env:CARGO_TARGET_DIR = "$PWD\target\claude"      # absolute: the CLI runs cargo from crates/desktop
$env:PATH = "$PWD\tools\node;$env:PATH"
Push-Location crates/desktop
npx -y @tauri-apps/cli@2 build
Pop-Location
```

Output: `target/claude/release/bundle/nsis/Hermes_<version>_x64-setup.exe` (≈22 MB for 0.1.0).

What `crates/desktop/tauri.conf.json` sets:
- `bundle.targets: ["nsis"]`, `installMode: "currentUser"` (no admin prompt, installs under
  `%LOCALAPPDATA%`), `webviewInstallMode: downloadBootstrapper` (fetches WebView2 if missing).
- `resources`: `../../target/hermes-src/apps/desktop/dist-webapp/` → `webapp/` next to the exe;
  `main.rs` `webapp_dist()` looks there first. If the webapp isn't built, the bundle fails or ships
  without a page — always build it first.
- Icons: `icons/icon.ico` (made by `scripts/make-app-icon.py` from Hermes's `icon-dark.png`).
- `version`: bump it here (and in `crates/desktop/Cargo.toml` to keep them equal) for a new
  release; the file name follows it.

The installed app uses the same data folder as a developer build (`%APPDATA%\hermes-rs`), so an
installed copy and a built copy share settings and sessions on one machine.

`scripts/build-msi.ps1` and `installer/hermes-rs.wxs` are the old WiX MSI for the frozen GPUI
app; not used for hermes-desktop.

## Smoke test before publishing

You can't run the installer against the user's data. Check what you can:
1. Run the built release exe as your perf instance (`$env:HERMES_PERF_EXE` = the release exe, or
   rely on perf.ps1 picking `target\claude\release`) and run `smoke.mjs` plus a couple of feature
   checks.
2. Confirm the bundle contains the webapp: the NSIS script lists `webapp\index.html`
   (`target/claude/release/nsis/x64/installer.nsi`).
3. Give the user a checklist for installing on the other computer (install, start from the Start
   menu, set up the endpoint, import Hermes sessions, send a prompt).

## Publish (the `installer` branch)

The branch holds only the installer and a README, never code. It was created as an orphan commit
with git plumbing so nothing in the working tree changes:

```bash
cd /c/Users/alexandros/repos/hermes-rs
exe=target/claude/release/bundle/nsis/Hermes_0.1.0_x64-setup.exe
# a README in the scratchpad: what it is, built from which commit, how to install, first run
blob_exe=$(git hash-object -w "$exe")
blob_readme=$(git hash-object -w /path/to/installer-README.md)
tree=$(printf '100644 blob %s\t%s\n100644 blob %s\tREADME.md\n' \
  "$blob_exe" "$(basename "$exe")" "$blob_readme" | git mktree)
git fetch origin installer
# -p keeps the branch's history (a fast-forward push, no force needed)
commit=$(git commit-tree "$tree" -p origin/installer \
  -m "Hermes <version> installer (built from main at $(git rev-parse --short HEAD))" \
  -m "Co-Authored-By: <the trailer the session gives you>")
git push origin "$commit:refs/heads/installer"
```

Notes:
- Each published installer adds ~22 MB to the repository's history. If that grows too much, ask
  the user whether to replace the branch (orphan commit + force push) instead; never force-push
  without asking.
- The README says which `main` commit it was built from; build from a committed, pushed `main`.
- The user downloads it from GitHub (the file's page ▸ "Download raw file").

## Updating Hermes before a release

See [PORTING.md](../PORTING.md#updating-hermes): report, port, patches, prompts, pin, rebuild,
check the missing-call log, check hiding. One commit per Hermes update.
