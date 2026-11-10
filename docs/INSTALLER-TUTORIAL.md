# Tutorial: making a release

A step-by-step guide to turning the code on `main` into a GitHub Release with
`Hermes_<version>_x64-setup.exe` attached: the file you download and run on any computer to install
or update the app. It takes about 20 minutes, most of it waiting for the build. For the background
(what each setting does, how agents publish), see [RELEASING.md](RELEASING.md).

All commands are for **PowerShell**, run from the repository folder (for example
`C:\Users\<you>\repos\hermes-rs`) unless a step says otherwise.

---

## Part 1: one-time setup (on the computer that builds and publishes)

1. **Git**, and the repository cloned with your GitHub access working (`git push` must work).
2. **Rust:** install [rustup](https://rustup.rs). The repository pins the exact Rust version
   (`rust-toolchain.toml`), and rustup downloads it by itself on the first build.
3. **Visual Studio Build Tools** with the "Desktop development with C++" workload and the
   Windows 11 SDK (Rust needs Microsoft's linker on Windows).
4. **Hermes next to the repository** (the build reads Hermes's frontend from `..\hermes-agent` and
   never changes it):
   ```powershell
   cd C:\Users\<you>\repos
   git clone https://github.com/alexandroslessis/hermes-rs
   git clone https://github.com/NousResearch/hermes-agent
   ```
5. **Portable Node.js 26** in `hermes-rs\tools\node` (the `tools` folder isn't in git): download
   the "Windows x64 zip" of Node 26 from nodejs.org and unpack it so that
   `hermes-rs\tools\node\node.exe` exists. Check:
   ```powershell
   .\tools\node\node.exe --version     # v26.x
   ```
6. **The GitHub CLI** (`gh`), signed in as the account that owns the repository:
   ```powershell
   winget install GitHub.cli
   ```
   Close and reopen PowerShell so `gh` is found, then:
   ```powershell
   gh auth login        # GitHub.com ▸ HTTPS ▸ log in with a web browser, follow the prompts
   gh auth status       # should say "Logged in to github.com"
   ```

## Part 2: make a release

### Step 1: get the code you want to ship

```powershell
git checkout main
git pull
git status           # "nothing to commit, working tree clean"
```

The release is built from the commit you are on, and it must be pushed (the script checks).

### Step 2: make sure Hermes's checkout has the pinned commit

The pinned Hermes commit is in the file `HERMES_COMMIT`. The build exports exactly that commit,
so the Hermes clone only needs to contain it:

```powershell
git -C ..\hermes-agent cat-file -t (Get-Content HERMES_COMMIT)
```

It should print `commit`. If it prints an error instead, fetch Hermes's history and try again:

```powershell
git -C ..\hermes-agent fetch --deepen=1000
```

### Step 3: raise the version

Every release is a new version (the tag `v0.3.0` can only be used once). Windows also shows the
version in "Installed apps". One command updates the three places that must agree and commits it:

```powershell
powershell -File scripts\bump-version.ps1 -Version 0.3.0 -Commit
git push
```

Use `0.2.4` for a small fix, `0.3.0` for a new feature, `1.0.0` when you call it stable.

### Step 4: write the release notes (optional)

Without a notes file, GitHub lists what changed since the last release by itself. For something
nicer, write a short Markdown file outside the repository, for example `C:\temp\notes.md`: what's
new in plain words, one bullet each.

### Step 5: build and publish

```powershell
powershell -File scripts\release.ps1 -Notes C:\temp\notes.md
```

(Leave out `-Notes C:\temp\notes.md` to use GitHub's generated list. Add `-Draft` to create the
release unpublished and look at it on github.com first; you publish it with its "Publish release"
button.)

What it does, and where it stops with a plain message if something is wrong:

1. Checks `gh` is installed and signed in, the working tree is clean, you are on `main`, `main` is
   pushed, the three version numbers agree, and the tag doesn't exist yet. All of this happens
   *before* the long build.
2. Builds Hermes's frontend (`scripts\build-hermes-webapp.ps1`), then the app and the installer.
   The first build takes 10+ minutes; later ones are faster.
3. Writes `Hermes_<version>_x64-setup.exe.sha256` next to the installer, and creates the release
   `v<version>` on GitHub with both files attached.

It ends by printing the release's address. To rehearse the checks without building or publishing:
add `-DryRun`.

If the build worked but the upload failed (network, sign-in), fix that and run the same command
again with `-SkipBuild`: it publishes the installer that is already built.

### Step 6: try it before telling anyone

1. Download `Hermes_<version>_x64-setup.exe` from the release page (it's under "Assets").
2. Close the app if it's running, double-click the installer and click through it. It installs for
   your user only (no administrator prompt) and has no desktop-shortcut checkbox.
3. Start **Hermes** from the Start menu and check: your sessions are there, Settings ▸ Providers
   lists your endpoint, a short message gets an answer.

Installing over an existing copy updates it in place. Your settings and sessions live in
`%APPDATA%\hermes-rs`, outside the install folder, so installing, updating and uninstalling never
touch them.

## Part 3: install it on another computer

Open the repository's **Releases** page on GitHub (the right-hand column of the repository's front
page, or `https://github.com/alexandroslessis/hermes-rs/releases`), pick the newest release, and
download `Hermes_<version>_x64-setup.exe` under "Assets". Run it. If the computer lacks WebView2
(rare on Windows 10/11), the installer downloads it. For a private repository, the computer needs
a GitHub login with access to it.

Optional integrity check: download the `.sha256` file too, then in PowerShell
`(Get-FileHash .\Hermes_<version>_x64-setup.exe).Hash` should equal the hash in that file
(compare ignoring upper/lower case).

## Part 4: building without publishing

To try an installer without making a release (for example on a branch), do by hand what the script
does:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-hermes-webapp.ps1
$env:CARGO_TARGET_DIR = "$PWD\target\release-build"
$env:PATH = "$PWD\tools\node;$env:PATH"
Push-Location crates\desktop
npx -y @tauri-apps/cli@2 build
Pop-Location
```

The installer is `target\release-build\release\bundle\nsis\Hermes_<version>_x64-setup.exe`. (Any
`CARGO_TARGET_DIR` works; use a folder other than the one a running copy of the app was built into,
or the build can't overwrite its exe.)

## If something goes wrong

| Symptom | Fix |
|---|---|
| `release.ps1`: "The GitHub CLI is not installed" or "not signed in" | Part 1, step 6. |
| `release.ps1`: "main and origin/main differ" | `git push` (or `git pull`); the release is built from what GitHub has. |
| `release.ps1`: "The tag v… already exists" | You already released that version: raise it (Step 3). |
| `release.ps1`: "tauri.conf.json says …, Cargo.toml says …" | Use `scripts\bump-version.ps1`, don't edit the numbers by hand. |
| The frontend build: `Node 26+ is expected in tools\node` | Part 1, step 5. |
| The frontend build: an error mentioning `tar` | You ran it from Git Bash; run it from PowerShell. |
| The frontend build: a patch "does not apply" | `HERMES_COMMIT` and the patches disagree (someone updated one without the other). Ask an agent to follow PORTING.md's "Updating Hermes". |
| The installer build: `npx` isn't recognized | Run the commands from `release.ps1`, or Part 4, in one PowerShell window (they set `PATH`). |
| The installer build: `linker 'link.exe' not found` | Part 1, step 3 (Visual Studio Build Tools). |
| The installer build: "Access is denied" writing the exe | A copy of the app built into that same folder is running; close it, or use `-TargetDir` with another folder. |
| The installed app opens a blank window | The frontend build's output was missing when the installer was built; run `release.ps1` again (it always rebuilds the frontend first). |
| Windows SmartScreen warns about an unknown publisher | The installer isn't code-signed. Click "More info" ▸ "Run anyway". |
| A release was published with a mistake | On the release's page, "Edit" ▸ fix the notes, or "Delete" it, then also `git push origin --delete v<version>` so the tag is free, and release again (raise the version if you had already told anyone). |
