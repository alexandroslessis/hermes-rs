# Tutorial: making a new installer

A step-by-step guide to turning the current code into `Hermes_<version>_x64-setup.exe`, the file
you run on another computer to install or update the app. It takes about 15 minutes, most of it
waiting for the build. For the background (what each setting does, how agents publish), see
[RELEASING.md](RELEASING.md).

All commands are for **PowerShell**, run from the repository folder
(`C:\Users\alexandros\repos\hermes-rs`) unless a step says otherwise.

---

## Part 1 — one-time setup (only on a computer that has never built the app)

This computer is already set up; skip to Part 2. On a new build computer you need:

1. **Rust:** install [rustup](https://rustup.rs). The repository pins the exact Rust version
   (`rust-toolchain.toml`), and rustup downloads it by itself on the first build.
2. **Visual Studio Build Tools** with the "Desktop development with C++" workload and the
   Windows 11 SDK (Rust needs Microsoft's linker on Windows).
3. **Git.**
4. **The two repositories side by side:**
   ```powershell
   cd C:\Users\<you>\repos
   git clone https://github.com/alexandroslessis/hermes-rs
   git clone https://github.com/NousResearch/hermes-agent
   ```
   The build reads Hermes's frontend from `..\hermes-agent`; it never changes it.
5. **Portable Node.js 26** in `hermes-rs\tools\node` (the `tools` folder isn't in git): download
   the "Windows x64 zip" of Node 26 from nodejs.org and unpack it so that
   `hermes-rs\tools\node\node.exe` exists. Check:
   ```powershell
   .\tools\node\node.exe --version     # v26.x
   ```

## Part 2 — build the installer

### Step 1: start from the code you want to ship

```powershell
git status          # should say "nothing to commit, working tree clean"
git pull            # the latest main (skip if you're building what's on this computer)
git log --oneline -1
```

Note the commit id that `git log` prints: it's the "built from" line of the release.

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

### Step 3 (optional): raise the version number

Windows shows the version in "Installed apps", and the file name carries it. If you're shipping
changes to a computer that already has the app, raise it. Edit the version in **both** files,
keeping them equal (for example `0.1.0` → `0.2.0`):

- `crates\desktop\tauri.conf.json`: the line `"version": "0.1.0",`
- `crates\desktop\Cargo.toml`: the line `version = "0.1.0"` near the top

Then commit it:

```powershell
git commit -am "Version 0.2.0"
```

(Installing a build with the same version over an older one also works; the version only helps
you tell builds apart.)

### Step 4: build Hermes's frontend

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-hermes-webapp.ps1
```

It exports Hermes at the pinned commit to `target\hermes-src`, applies our patches, installs the
frontend's packages and builds it. The first time takes a few minutes. It ends without a red
error; the result is the folder `target\hermes-src\apps\desktop\dist-webapp`. Check it:

```powershell
Test-Path target\hermes-src\apps\desktop\dist-webapp\index.html    # True
```

npm prints warnings along the way; those are normal.

### Step 5: build the app and the installer

```powershell
$env:CARGO_TARGET_DIR = "$PWD\target\claude"
$env:PATH = "$PWD\tools\node;$env:PATH"
Push-Location crates\desktop
npx -y @tauri-apps/cli@2 build
Pop-Location
```

What happens: Tauri's build tool compiles the app in release mode (5–10 minutes the first time,
less afterwards), then packs it, the frontend from Step 4 and the icon into an installer. The
last lines name the file:

```
Finished 1 bundle at:
    ...\target\claude\release\bundle\nsis\Hermes_0.1.0_x64-setup.exe
```

(`target\claude` is the build folder used for builds that shouldn't disturb a running copy of
the app. Any folder works; the installer lands in `<that folder>\release\bundle\nsis\`.)

### Step 6: try it before shipping

1. Close the app if it's running.
2. Double-click the new `Hermes_<version>_x64-setup.exe` and click through it. It installs for
   your user only (no administrator prompt).
3. Start **Hermes** from the Start menu and check: your sessions are there, Settings ▸ Providers
   lists your endpoint, a short message gets an answer.

Installing over an existing copy updates it in place. Your settings and sessions live in
`%APPDATA%\hermes-rs`, outside the install folder, so installing, updating and uninstalling never
touch them.

## Part 3 — get it to the other computer

Pick one:

**A. Copy the file.** Put `Hermes_<version>_x64-setup.exe` on a USB stick, a cloud drive or an
email to yourself, and run it on the other computer. Nothing else is needed: if that computer
lacks WebView2 (rare on Windows 10/11), the installer downloads it.

**B. Publish it on GitHub** (the `installer` branch, which holds only the installer and a short
README). Run this in **Git Bash** (it comes with Git) from the repository folder; change the
version and the README text to match:

```bash
exe=target/claude/release/bundle/nsis/Hermes_0.1.0_x64-setup.exe
built=$(git rev-parse --short HEAD)
cat > /tmp/installer-README.md <<EOF
# Hermes (hermes-rs) installer

\`$(basename "$exe")\`: the Windows installer for hermes-desktop, built from \`main\` at $built.
Download it (the file's page ▸ "Download raw file"), run it, and start Hermes from the Start menu.
It installs for your user only and downloads WebView2 if the computer doesn't have it.

This branch holds only the installer; the code is on \`main\`.
EOF
git fetch origin installer
blob_exe=$(git hash-object -w "$exe")
blob_readme=$(git hash-object -w /tmp/installer-README.md)
tree=$(printf '100644 blob %s\t%s\n100644 blob %s\tREADME.md\n' \
  "$blob_exe" "$(basename "$exe")" "$blob_readme" | git mktree)
commit=$(git commit-tree "$tree" -p origin/installer -m "Hermes installer (built from main at $built)")
git push origin "$commit:refs/heads/installer"
```

This adds a commit to the `installer` branch without touching your working folder or `main`. On
the other computer, open the repository on GitHub, switch to the `installer` branch, click the
`.exe`, then "Download raw file". (Each published installer adds about 22 MB to the repository;
if that ever matters, ask an agent to replace the branch instead.)

## If something goes wrong

| Symptom | Fix |
|---|---|
| Step 4: `Node 26+ is expected in tools\node` | Part 1, step 5. |
| Step 4: an error mentioning `tar` | You ran it from Git Bash; run it from PowerShell. |
| Step 4: a patch "does not apply" | `HERMES_COMMIT` and the patches disagree (someone updated one without the other). Ask an agent to follow PORTING.md's "Updating Hermes". |
| Step 5: `npx` isn't recognized | The `$env:PATH` line of Step 5 wasn't run in this PowerShell window. |
| Step 5: `linker 'link.exe' not found` | Part 1, step 2 (Visual Studio Build Tools). |
| Step 5: "Access is denied" writing the exe | A copy of the app built into that same folder is running; close it, or use another `CARGO_TARGET_DIR`. |
| The installed app opens a blank window | Step 4's output was missing when Step 5 ran; redo Step 4, then Step 5. |
| Windows SmartScreen warns about an unknown publisher | The installer isn't code-signed. Click "More info" ▸ "Run anyway". |
