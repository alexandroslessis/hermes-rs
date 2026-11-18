//! `hermes-desktop.exe --acp`: the agent for an editor (Zed, t3code) over the Agent Client
//! Protocol, on stdin and stdout. No window and no web server: the same runtime and gateway the
//! page uses, driven in this process by [`server::AcpServer`]. The plan, the protocol and the
//! decisions are in `docs/acp/`.
//!
//! **stdout is the protocol**: logging goes to `logs\acp.log` and stderr, nothing else may print
//! there, or the editor's parser breaks.
//!
//! Hermes: `acp_adapter/entry.py` (stdio only, logging to stderr).

mod controls;
mod history;
mod permissions;
mod prompt;
pub mod rpc;
mod server;
mod tools;
mod translate;

use std::sync::Arc;

use anyhow::{Context as _, Result};
use hermes_runtime::RuntimeConfig;

/// The log: stderr (which the editor keeps) and `logs\acp.log` in the data folder; a log past
/// 5 MB moves to `acp.log.1` at startup. `RUST_LOG` filters it (default `info`).
fn init_logging(data_dir: &std::path::Path) {
    use std::io::Write;
    /// The file first, then stderr; a stderr the editor has closed must not lose the file's line.
    struct Both(Option<std::fs::File>);
    impl Write for Both {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Some(file) = &mut self.0 {
                let _ = file.write_all(buf);
            }
            let _ = std::io::stderr().write_all(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let file = (|| {
        let folder = data_dir.join("logs");
        std::fs::create_dir_all(&folder).ok()?;
        let path = folder.join("acp.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 << 20) {
            let _ = std::fs::rename(&path, folder.join("acp.log.1"));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    })();
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(Both(file)))
        .init();
}

/// Runs the agent until the editor closes stdin.
pub fn run() -> Result<()> {
    let mut config = RuntimeConfig::for_current_user()?;
    init_logging(&config.data_dir);
    tracing::info!("hermes-rs ACP agent {} starting", env!("CARGO_PKG_VERSION"));
    // A panic goes to the log file too (the default hook prints to stderr only, which an editor
    // may not keep).
    std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
    let data_dir = config.data_dir.clone();
    // The terminal tool's shell, as the desktop app chooses it.
    if !crate::wants_git_bash(&data_dir) {
        config.shell = hermes_tools::ShellKind::PowerShell;
    }
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("hermes-acp")
        .enable_all()
        .build()?;
    tokio.block_on(async move {
        let handle = tokio::runtime::Handle::current();
        let gateway =
            tokio::task::spawn_blocking(move || -> Result<Arc<crate::gateway::Gateway>> {
                // Approvals and questions reach `AcpServer` as the gateway's events (the hub).
                // No app lock (`hermes_core::lock`): this runs beside the desktop app.
                let core =
                    hermes_core::Core::new(config, handle).context("opening the data folder")?;
                core.runtime().warm_up();
                let gateway = Arc::new(crate::gateway::Gateway::new_for_acp(core));
                // A thread's background review uses its provider as a turn would (Phase 7).
                crate::gateway::Gateway::install_review_route(&gateway);
                Ok(gateway)
            })
            .await??;
        let ledger = gateway.runtime.ledger().clone();
        let runtime = gateway.runtime.clone();
        server::AcpServer::serve(gateway, tokio::io::stdin(), tokio::io::stdout()).await;
        // The editor closed stdin: write the request counts before the process ends, and let
        // the store's writer commit what is still queued (Phase 13 CP13b).
        ledger.flush_blocking();
        let store_closed = tokio::task::spawn_blocking(move || runtime.store().shutdown()).await;
        if let Ok(Err(error)) = store_closed {
            tracing::error!("session database at exit: {error:#}");
        }
        Ok::<(), anyhow::Error>(())
    })?;
    // Tasks that never finish (a read of stdin) must not keep the process alive.
    tokio.shutdown_timeout(std::time::Duration::from_secs(2));
    Ok(())
}
