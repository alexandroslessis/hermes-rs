//! `hermes-acp.exe`: the agent for an editor (Zed, t3code) over the Agent Client Protocol, on
//! stdin and stdout. The same as `hermes-desktop.exe --acp`, but a *console* program: editors
//! (Zed on Windows) cannot drive the release `hermes-desktop.exe`, which is a GUI program with no
//! console, so the installer ships this one next to it. See `docs/acp/`.

fn main() -> anyhow::Result<()> {
    hermes_desktop::acp::run()
}
