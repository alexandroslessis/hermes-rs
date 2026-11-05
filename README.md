# Hermes (hermes-rs) installer

`Hermes_0.1.0_x64-setup.exe`: the Windows installer for hermes-desktop, built from `main` at 7bbcee9.
Download it (the file's page ▸ "Download raw file"), run it, and start Hermes from the Start
menu. It installs for your user only (no admin prompt) and downloads WebView2 if the computer
doesn't have it. Installing over an earlier copy updates it; settings and sessions (in
`%APPDATA%\hermes-rs`) are kept.

First run: set up your endpoint in Settings ▸ Providers ▸ Custom endpoints. To bring over the
original Hermes's sessions on that computer, open the command palette (Ctrl+K) ▸ "Import
session", or type `/import-hermes` in a chat to import all of them.

New in this build: imported Hermes sessions keep Hermes's own compression (no long first
reply), compression shows its progress, and Settings ▸ Model ▸ Auxiliary picks the models for
compression, approvals, titles and the background review.

This branch holds only the installer; the code is on `main`. To build one yourself, see
`docs/INSTALLER-TUTORIAL.md` on `main`.
