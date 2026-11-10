# Releasing: the Windows installer

(For people: the step-by-step version is [INSTALLER-TUTORIAL.md](INSTALLER-TUTORIAL.md). Keep the
two in step when the build changes.)

The app ships as an NSIS installer built by Tauri's CLI and published as a **GitHub Release**
(`v<version>`, with `Hermes_<version>_x64-setup.exe` and its `.sha256` attached). The old
`installer` branch is gone; installers are no longer stored in git history. Build and publish only
when the user asks for it.

**Publishing needs the GitHub CLI (`gh`), which is not installed on the machine agents usually work
on** (no `gh` here, no GitHub credentials for it). The user publishes from a computer where `gh` is
signed in, with `scripts/release.ps1`; the step-by-step is
[INSTALLER-TUTORIAL.md](INSTALLER-TUTORIAL.md). An agent's part is to leave `main` pushed, the
version bumped (`scripts/bump-version.ps1`) and the docs true, and to build an installer for a smoke
test when asked.

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

**Custom NSIS template** (`nsis/installer.nsi`, set by `bundle ▸ windows ▸ nsis ▸ template`): the
desktop shortcut is removed from the installer — no "Add desktop shortcut" checkbox on the finish
page, and silent/passive installs don't create one (the Start menu shortcut is unchanged; the
uninstaller still cleans up a leftover desktop shortcut from an older install). The file is
Tauri's own template (copied from `tauri-bundler` at the CLI's version, `tauri-cli-v2.12.4`) with
those pieces taken out. When the Tauri CLI updates, refresh the copy against the new version's
template — a stale one can break the bundle — and re-check the changes still read honestly.

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

## Publish (a GitHub Release)

```powershell
powershell -File scripts\bump-version.ps1 -Version 0.3.0 -Commit    # the three version numbers
git push
powershell -File scripts\release.ps1 [-Notes notes.md] [-Draft]      # checks, builds, publishes
```

`scripts/release.ps1` refuses to start unless: `gh` is installed and signed in, the working tree is
clean, you are on `main` and it equals `origin/main`, `tauri.conf.json`, `crates/desktop/Cargo.toml`
and `Cargo.lock` carry the same version, and the tag `v<version>` is free. Then it builds the
frontend and the installer into `target\release-build` (`-TargetDir` changes it), writes the
SHA-256 file and runs `gh release create v<version> <installer> <sha256> --target <HEAD>` with
either `-Notes <file>` or GitHub's generated notes. `-DryRun` runs the checks and prints the `gh`
command; `-SkipBuild` publishes an installer that is already built (a rerun after a failed upload).

Notes:
- A release is made from a pushed `main`, so "built from" is always a commit anyone can check out.
- Versions are used once: a mistaken release is edited or deleted on GitHub (delete its tag too:
  `git push origin --delete v<version>`), then the next one gets a new number.
- Binaries live in the release's assets, not in git: the repository stops growing by ~22 MB per
  installer.
- Not done yet: building the release in GitHub Actions on a tag push (would need the Hermes clone at
  `HERMES_COMMIT`, Node, Rust 1.98.1 and the same two build commands), and Tauri's in-app updater.

## Updating Hermes before a release

See [PORTING.md](../PORTING.md#updating-hermes): report, port, patches, prompts, pin, rebuild,
check the missing-call log, check hiding. One commit per Hermes update.
