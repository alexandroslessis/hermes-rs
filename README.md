# Hermes (hermes-rs) installer

`Hermes_0.1.0_x64-setup.exe`: the Windows installer for hermes-desktop, built from `main` at
6fc81cd. Download it (the file's page ▸ "Download raw file"), run it, and start Hermes from the
Start menu. It installs for your user only (no admin prompt) and downloads WebView2 if the
computer doesn't have it. Installing over an earlier copy updates it; settings and sessions (in
`%APPDATA%\hermes-rs`) are kept.

First run: set up your endpoint in Settings ▸ Providers ▸ Custom endpoints. To bring over the
original Hermes's sessions on that computer, open the command palette (Ctrl+K) ▸ "Import
session", or type `/import-hermes` in a chat to import all of them.

New in this build:
- Archive sessions (sidebar, Alt+Shift-click, or the shortcut); Settings ▸ Sessions lists them
  to restore or delete, with an opt-in auto-archive of idle sessions.
- The Artifacts gallery in the sidebar: links and files from recent sessions; a file opens in
  its default app (programs and scripts are only shown in Explorer).
- Settings ▸ Model ▸ Auxiliary: separate models for compression, approvals, titles and the
  background review.
- Imported Hermes sessions keep Hermes's compression; compression shows its progress.

This branch holds only the installer; the code is on `main`. To build one yourself, see
`docs/INSTALLER-TUTORIAL.md` on `main`.
