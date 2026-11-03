//! Hermes's desktop frontend in a Tauri window, backed by the hermes-rs runtime instead of
//! Hermes's Python backend. The window loads Hermes's webapp bundle from a local server
//! (`server.rs`) that also speaks Hermes's gateway protocol (`gateway.rs`).

// Release builds are GUI-only (no console window); debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gateway;
mod hub;
mod providers;
mod rest;
mod server;
mod transcript;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use hermes_runtime::{Runtime, RuntimeConfig};
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Hermes's built webapp: `HERMES_UI_DIST`, else the prototype's build output.
fn webapp_dist() -> PathBuf {
    std::env::var_os("HERMES_UI_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/hermes-src/apps/desktop/dist-webapp")
        })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // The agent and the server run on their own tokio runtime; Tauri keeps the main thread.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("hermes-runtime")
        .enable_all()
        .build()?;
    let config = RuntimeConfig::for_current_user()?;
    let data_dir = config.data_dir.clone();
    // The runtime asks the user (approvals, clarify) through the page following the session.
    let hub = Arc::new(hub::Hub::default());
    let runtime = {
        let _guard = tokio.enter();
        Arc::new(Runtime::new(
            config,
            Arc::new(hub::DesktopBridge { hub: hub.clone() }),
        )?)
    };
    runtime.warm_up();

    // A fixed port (HERMES_DESKTOP_PORT) lets a browser open the same page while debugging.
    let port: u16 = std::env::var("HERMES_DESKTOP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", port)).context("binding the local server")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let state = Arc::new(server::ServerState {
        dist: webapp_dist(),
        gateway: Arc::new(gateway::Gateway::new(runtime, hub, data_dir)),
        token: uuid::Uuid::new_v4().simple().to_string(),
    });
    tokio.spawn(async move {
        match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => server::serve(listener, state).await,
            Err(error) => tracing::error!("server: {error}"),
        }
    });
    let url = format!("http://{address}/");
    tracing::info!("serving Hermes's webapp at {url}");

    tauri::Builder::default()
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("Hermes")
                .inner_size(1280.0, 860.0)
                .min_inner_size(720.0, 480.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .context("running the window")?;
    drop(tokio);
    Ok(())
}
