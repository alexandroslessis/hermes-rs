//! Hermes's desktop frontend in a Tauri window, backed by the hermes-rs runtime instead of
//! Hermes's Python backend. The window loads Hermes's webapp bundle from a local server
//! (`server.rs`) that also speaks Hermes's gateway protocol (`gateway.rs`).

// Release builds are GUI-only (no console window); debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod gateway;
mod git;
mod hub;
mod paths;
mod projects;
mod providers;
mod rest;
mod server;
mod transcript;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use hermes_runtime::{Runtime, RuntimeConfig};
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Where the local server listens by default (the page's origin, which its localStorage is kept by).
const DEFAULT_PORT: u16 = 47_731;

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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // The agent and the server run on their own tokio runtime; Tauri keeps the main thread.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("hermes-runtime")
        .enable_all()
        .build()?;
    let mut config = RuntimeConfig::for_current_user()?;
    let data_dir = config.data_dir.clone();
    // The terminal tool runs PowerShell, Windows' own shell; `terminal.shell: "bash"` in
    // desktop-config.json keeps Git Bash (when installed) instead.
    if !wants_git_bash(&data_dir) {
        config.shell = hermes_tools::ShellKind::PowerShell;
    }
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

    // The page keeps some settings in its localStorage, which belongs to its origin, so the
    // port stays the same between runs (HERMES_DESKTOP_PORT overrides it). When it's taken
    // (a second instance), any free port: that window starts with empty local settings.
    let port: u16 = std::env::var("HERMES_DESKTOP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .or_else(|error| {
            tracing::warn!("port {port} is taken ({error}); local settings won't carry over");
            std::net::TcpListener::bind(("127.0.0.1", 0))
        })
        .context("binding the local server")?;
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
                // A reply keeps streaming while the window is in the background (Hermes's
                // Electron app lifts throttling during turns; WebView2 takes Chromium's flags).
                // The first flag is wry's default, which this replaces.
                .additional_browser_args(concat!(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection ",
                    "--disable-background-timer-throttling ",
                    "--disable-renderer-backgrounding ",
                    "--disable-backgrounding-occluded-windows"
                ))
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

/// Whether Settings' config record asks for Git Bash (`terminal.shell: "bash"`).
fn wants_git_bash(data_dir: &std::path::Path) -> bool {
    std::fs::read(data_dir.join("desktop-config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|config| config["terminal"]["shell"] == "bash")
}
