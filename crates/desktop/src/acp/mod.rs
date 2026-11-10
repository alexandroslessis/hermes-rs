//! `hermes-desktop.exe --acp`: the agent for an editor (Zed, t3code) over the Agent Client
//! Protocol, on stdin and stdout. No window and no web server: the same runtime and gateway the
//! page uses, driven in this process by [`server::AcpServer`]. The plan, the protocol and the
//! decisions are in `docs/acp/`.
//!
//! **stdout is the protocol**: logging goes to `logs\acp.log` and stderr, nothing else may print
//! there, or the editor's parser breaks.
//!
//! Hermes: `acp_adapter/entry.py` (stdio only, logging to stderr).

mod history;
mod permissions;
mod prompt;
pub mod rpc;
mod server;
mod tools;
mod translate;

use std::sync::Arc;

use anyhow::{Context as _, Result};
use hermes_runtime::{Runtime, RuntimeConfig};

/// The log: stderr (which the editor keeps) and `logs\acp.log` in the data folder; a log past
/// 5 MB moves to `acp.log.1` at startup. `RUST_LOG` filters it (default `info`).
fn init_logging(data_dir: &std::path::Path) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
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
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::io::stderr);
    match file {
        Some(file) => builder
            .with_writer(std::io::stderr.and(std::sync::Mutex::new(file)))
            .init(),
        None => builder.init(),
    }
}

/// Runs the agent until the editor closes stdin.
pub fn run() -> Result<()> {
    let mut config = RuntimeConfig::for_current_user()?;
    init_logging(&config.data_dir);
    tracing::info!("hermes-rs ACP agent {} starting", env!("CARGO_PKG_VERSION"));
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
        let gateway =
            tokio::task::spawn_blocking(move || -> Result<Arc<crate::gateway::Gateway>> {
                // Approvals and questions go through the gateway's hub, which `AcpServer` reads.
                let hub = Arc::new(crate::hub::Hub::default());
                let runtime = Arc::new(
                    Runtime::new(
                        config,
                        Arc::new(crate::hub::DesktopBridge { hub: hub.clone() }),
                    )
                    .context("opening the data folder")?,
                );
                runtime.warm_up();
                Ok(Arc::new(crate::gateway::Gateway::new_for_acp(
                    runtime, hub, data_dir,
                )))
            })
            .await??;
        server::AcpServer::serve(gateway, tokio::io::stdin(), tokio::io::stdout()).await;
        Ok::<(), anyhow::Error>(())
    })?;
    // Tasks that never finish (a read of stdin) must not keep the process alive.
    tokio.shutdown_timeout(std::time::Duration::from_secs(2));
    Ok(())
}
