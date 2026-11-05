# Hermes (hermes-rs) installer

`Hermes_0.1.0_x64-setup.exe`: the Windows installer for hermes-desktop, built from `main` at
2088bf2. Download it (the file's page ▸ "Download raw file"), run it, and start Hermes from the
Start menu. It installs for your user only (no admin prompt) and downloads WebView2 if the
computer doesn't have it.

First run: set up your endpoint in Settings ▸ Providers ▸ Custom endpoints. To bring over the
original Hermes's sessions on that computer, open the command palette (Ctrl+K) ▸ "Import
session", or type `/import-hermes` in a chat to import all of them.

This branch holds only the installer; the code is on `main`.
