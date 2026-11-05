//! Hermes's desktop frontend in a Tauri window, backed by the hermes-rs runtime instead of
//! Hermes's Python backend. The window loads Hermes's webapp bundle from a local server
//! (`server.rs`) that also speaks Hermes's gateway protocol (`gateway.rs`).

// Release builds are GUI-only (no console window); debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod attachments;
mod branch;
mod commands;
mod endpoints;
mod export;
mod files;
mod gateway;
mod git;
mod git_review;
mod hub;
mod maintenance;
mod paths;
mod projects;
mod providers;
mod rest;
mod server;
mod skills;
mod terminal;
mod toolsets;
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
    let config = RuntimeConfig::for_current_user();
    init_logging(config.as_ref().ok().map(|c| c.data_dir.as_path()));

    // The agent and the server run on their own tokio runtime; Tauri keeps the main thread.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("hermes-runtime")
        .enable_all()
        .build()?;
    let mut config = config?;
    let data_dir = config.data_dir.clone();
    // The terminal tool runs PowerShell, Windows' own shell; `terminal.shell: "bash"` in
    // desktop-config.json keeps Git Bash (when installed) instead.
    if !wants_git_bash(&data_dir) {
        config.shell = hermes_tools::ShellKind::PowerShell;
    }

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
    // The window, once Tauri has made it: the page's caption buttons act on it through the server.
    let window = Arc::new(std::sync::OnceLock::new());
    let url = format!("http://{address}/");

    // The backend (store, skills, tools; on a first run, seeding the bundled skills) is built
    // while Tauri starts the window and WebView2, not before: the page only asks for it once
    // WebView2 is up, and its first requests wait in the bound port's queue until it serves.
    // (Built in sequence, the window started ~130ms later on every launch, ~1.9s on a first run.)
    // The side panel's shells (`terminal.rs`), ended when the window closes.
    let terminals = Arc::new(terminal::Terminals::default());
    {
        let window = window.clone();
        let url = url.clone();
        let terminals = terminals.clone();
        tokio.spawn(async move {
            let built = tokio::task::spawn_blocking(move || -> Result<server::ServerState> {
                // The runtime asks the user (approvals, clarify) through the page following the
                // session.
                let hub = Arc::new(hub::Hub::default());
                let runtime = Arc::new(Runtime::new(
                    config,
                    Arc::new(hub::DesktopBridge { hub: hub.clone() }),
                )?);
                runtime.warm_up();
                Ok(server::ServerState {
                    dist: webapp_dist(),
                    gateway: Arc::new(gateway::Gateway::new(runtime, hub, data_dir)),
                    token: uuid::Uuid::new_v4().simple().to_string(),
                    port: address.port(),
                    window,
                    terminals,
                })
            })
            .await;
            let state = match built {
                Ok(Ok(state)) => Arc::new(state),
                Ok(Err(error)) => fail(&format!("{error:#}")),
                Err(error) => fail(&error.to_string()),
            };
            match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => {
                    tracing::info!("serving Hermes's webapp at {url}");
                    server::serve(listener, state).await
                }
                Err(error) => tracing::error!("server: {error}"),
            }
        });
    }

    tauri::Builder::default()
        .setup(move |app| {
            let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
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
                // No system title bar: Hermes's own, in the theme's colors, with the page's
                // caption buttons (Hermes's Electron window is frameless too). WebView2 honors the
                // page's `app-region: drag` areas (wry enables non-client regions), so the bar
                // drags and double-click maximizes; the shadow keeps the resize edges.
                .decorations(false)
                .shadow(true)
                // Files dropped on the page reach it (the composer's drag-and-drop): Tauri's own
                // drop handler would take them first.
                .disable_drag_drop_handler()
                .inner_size(1280.0, 860.0)
                .min_inner_size(720.0, 480.0)
                .build()?;
            // Sharp taskbar and title icons, again when the window moves to a display of
            // another scale.
            set_window_icons(&built);
            let rescaled = built.clone();
            built.on_window_event(move |event| {
                if let tauri::WindowEvent::ScaleFactorChanged { .. } = event {
                    set_window_icons(&rescaled);
                }
            });
            let _ = window.set(built);
            Ok(())
        })
        .run(tauri::generate_context!())
        .context("running the window")?;
    terminals.close_all();
    drop(tokio);
    Ok(())
}

/// The window's icons from the exe's own icon (`icons/icon.ico`, every size), at exactly the
/// sizes Windows draws them at the window's DPI: the taskbar's (large) and the title's (small).
/// Tauri sets one bitmap (the `.ico`'s first entry) and Windows rescales it, which blurs.
fn set_window_icons(window: &tauri::WebviewWindow) {
    use std::sync::Mutex;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Controls::LoadIconWithScaleDown;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, HICON, ICON_BIG, ICON_SMALL, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON,
        SendMessageW, WM_SETICON,
    };
    /// The icon resource tauri-build embeds (`32512 ICON "icons/icon.ico"`).
    const APP_ICON: usize = 32512;
    /// The icons this set before (destroyed when replaced): `[large, small]`.
    static PREVIOUS: Mutex<[usize; 2]> = Mutex::new([0, 0]);

    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let hwnd = hwnd.0;
    let mut previous = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: plain Win32 calls on this process's own window and module; each icon handle is
    // used only after the call that made it succeeded.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let module = GetModuleHandleW(std::ptr::null());
        let kinds = [(ICON_BIG, SM_CXICON, SM_CYICON), (ICON_SMALL, SM_CXSMICON, SM_CYSMICON)];
        for (slot, (kind, metric_x, metric_y)) in kinds.into_iter().enumerate() {
            let (width, height) = (
                GetSystemMetricsForDpi(metric_x, dpi),
                GetSystemMetricsForDpi(metric_y, dpi),
            );
            let mut icon: HICON = std::ptr::null_mut();
            if LoadIconWithScaleDown(module, APP_ICON as *const u16, width, height, &mut icon) != 0
                || icon.is_null()
            {
                continue;
            }
            SendMessageW(hwnd, WM_SETICON, kind as usize, icon as isize);
            if previous[slot] != 0 {
                DestroyIcon(previous[slot] as HICON);
            }
            previous[slot] = icon as usize;
        }
    }
}

/// The log: to stdout (a debug build's console) and to `logs\desktop.log` in the data folder,
/// which a release build (no console) otherwise loses and Settings ▸ Advanced ▸ Logs reads. A log
/// past 5 MB is moved to `desktop.log.1` at startup. `RUST_LOG` filters it (default `info`).
fn init_logging(data_dir: Option<&std::path::Path>) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let file = data_dir.and_then(|dir| {
        let folder = dir.join("logs");
        std::fs::create_dir_all(&folder).ok()?;
        let path = folder.join("desktop.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 << 20) {
            let _ = std::fs::rename(&path, folder.join("desktop.log.1"));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false);
    match file {
        Some(file) => builder
            .with_writer(std::io::stdout.and(std::sync::Mutex::new(file)))
            .init(),
        None => builder.init(),
    }
}

/// The backend couldn't start (its data folder unreadable, the database locked by a newer
/// schema, …): say so and quit, as the app did when the backend was built before the window.
fn fail(error: &str) -> ! {
    tracing::error!("starting the backend: {error}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Hermes")
        .set_description(format!("Hermes couldn't start its backend:\n\n{error}"))
        .show();
    std::process::exit(1)
}

/// Whether Settings' config record asks for Git Bash (`terminal.shell: "bash"`).
fn wants_git_bash(data_dir: &std::path::Path) -> bool {
    std::fs::read(data_dir.join("desktop-config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|config| config["terminal"]["shell"] == "bash")
}
