//! The local web server the window loads: Hermes's webapp bundle, its REST routes under `/api`,
//! and the `tui_gateway` JSON-RPC socket on `/api/ws` (what `hermes_cli/web_server_dashboard.py`
//! and `tui_gateway/ws.py` serve in Hermes). Routes the backend doesn't have answer Hermes's
//! own "No such API endpoint" 404, which the frontend reads as "this backend lacks the feature".

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};

use crate::gateway::Gateway;

/// What the page hides of Hermes's interface (`plugins/hermes-rs/hide.css`).
const HIDE_CSS: &str = include_str!("../plugins/hermes-rs/hide.css");

/// Logs a call this backend doesn't serve, once per call: the page polls several of them every
/// few seconds, and one line each is enough to know what's missing.
pub fn log_missing(call: &str, detail: &str) {
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let first = SEEN
        .lock()
        .map(|mut seen| {
            seen.get_or_insert_with(HashSet::new)
                .insert(call.to_string())
        })
        .unwrap_or(true);
    if first {
        tracing::info!(target: "hermes_desktop::missing", "{call} {detail}");
    }
}

pub struct ServerState {
    /// Hermes's built webapp (`apps/desktop/dist-webapp`).
    pub dist: PathBuf,
    pub gateway: Arc<Gateway>,
    /// This run's session token: the page gets it, REST calls send it back.
    pub token: String,
    /// The port the page is served on (its origin is `http://127.0.0.1:{port}`).
    pub port: u16,
    /// The app's window, once made: the page's caption buttons act on it.
    pub window: Arc<std::sync::OnceLock<tauri::WebviewWindow>>,
}

pub async fn serve(listener: tokio::net::TcpListener, state: Arc<ServerState>) {
    let app = Router::new()
        .route("/api/ws", get(websocket))
        .route("/api/hermes-rs/window", axum::routing::post(window_control))
        .route("/api/{*rest}", any(api))
        .fallback(get(static_file))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            only_this_app,
        ))
        .with_state(state);
    if let Err(error) = axum::serve(listener, app).await {
        tracing::error!("server stopped: {error}");
    }
}

/// Only the app's own page reaches the server. Anything a web page sends to 127.0.0.1 would
/// otherwise run here: a site open in the user's browser could open the gateway socket and
/// submit prompts (which run commands), or post to the REST routes. Browsers always send `Origin`
/// on a WebSocket handshake and on a cross-site request, so a foreign one is refused; and a
/// `Host` other than this machine's loopback names refuses DNS rebinding, where a site's own name
/// is pointed at 127.0.0.1 to make its requests same-origin.
async fn only_this_app(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let host = header_text(&request, header::HOST);
    let origin = header_text(&request, header::ORIGIN);
    if !from_this_app(state.port, host.as_deref(), origin.as_deref()) {
        tracing::warn!(
            "refused a request from outside the app (host {host:?}, origin {origin:?}): {}",
            request.uri().path()
        );
        return (
            StatusCode::FORBIDDEN,
            "This server only serves the Hermes window.",
        )
            .into_response();
    }
    next.run(request).await
}

/// The `Host` names this machine's loopback on the app's port, and `Origin`, when sent, is the
/// app's own page.
fn from_this_app(port: u16, host: Option<&str>, origin: Option<&str>) -> bool {
    let ours = |value: &str| {
        let value = value.trim_end_matches('/');
        value == format!("127.0.0.1:{port}") || value == format!("localhost:{port}")
    };
    host.is_some_and(ours)
        && origin.is_none_or(|origin| origin.strip_prefix("http://").is_some_and(ours))
}

fn header_text(request: &Request, name: header::HeaderName) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// `POST /api/hermes-rs/window` `{"action": "minimize" | "toggleMaximize" | "close"}`: the page's
/// caption buttons (patches/0007-window-controls.patch), the frameless window having none.
async fn window_control(
    State(state): State<Arc<ServerState>>,
    body: axum::body::Bytes,
) -> Response {
    let action = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["action"].as_str().map(str::to_string))
        .unwrap_or_default();
    let Some(window) = state.window.get() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = match action.as_str() {
        "minimize" => window.minimize(),
        "toggleMaximize" => match window.is_maximized() {
            Ok(true) => window.unmaximize(),
            _ => window.maximize(),
        },
        "close" => window.close(),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    match result {
        Ok(()) => (
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({"maximized": window.is_maximized().unwrap_or(false)}).to_string(),
        )
            .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn websocket(
    State(state): State<Arc<ServerState>>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let gateway = state.gateway.clone();
    // The page sends its ticket as a subprotocol (`hermes-gateway-ticket.<ticket>`); a browser
    // drops the socket unless the server picks one of the offered protocols.
    let offered: Vec<String> = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|protocol| protocol.trim().to_string())
        .filter(|protocol| !protocol.is_empty())
        .collect();
    let chosen = offered
        .iter()
        .find(|p| p.starts_with("hermes-gateway-ticket."))
        .or(offered.first())
        .cloned();
    upgrade
        .protocols(chosen)
        .max_message_size(64 << 20)
        .on_upgrade(move |socket| async move { gateway.connection(socket).await })
}

/// REST: the routes this backend implements, else Hermes's catch-all 404.
async fn api(State(state): State<Arc<ServerState>>, request: Request) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let body = axum::body::to_bytes(request.into_body(), 16 << 20)
        .await
        .unwrap_or_default();
    match state
        .gateway
        .rest(method.as_str(), uri.path(), uri.query(), &body)
        .await
    {
        Some((status, json)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
            [(header::CONTENT_TYPE, "application/json")],
            json.to_string(),
        )
            .into_response(),
        None => {
            log_missing(&format!("REST {method} {}", uri.path()), "");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::json!({ "detail": format!("No such API endpoint: {}", uri.path()) })
                    .to_string(),
            )
                .into_response()
        }
    }
}

/// The bundle's files; any other path is the single-page app (`index.html`, with the globals
/// Hermes's `_serve_index` injects for the webapp surface).
async fn static_file(State(state): State<Arc<ServerState>>, request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    if !path.is_empty() && !path.contains("..") {
        let file = state.dist.join(path);
        if let Ok(meta) = tokio::fs::metadata(&file).await
            && meta.is_file()
        {
            // Cached, so WebView2 keeps the compiled scripts between launches (V8's code cache
            // only serves scripts from the HTTP cache): without it every launch parses ~9 MB of
            // JavaScript again. The bundler names everything under `assets/` by its content
            // hash, so those never change under the same name; the few other files are
            // revalidated by size and time (a 304 when unchanged).
            let (cache, etag) = cache_policy(path, meta.len(), meta.modified().ok());
            let unchanged = request
                .headers()
                .get(header::IF_NONE_MATCH)
                .is_some_and(|tag| tag.as_bytes() == etag.as_bytes());
            if unchanged {
                return (
                    StatusCode::NOT_MODIFIED,
                    [(header::ETAG, etag), (header::CACHE_CONTROL, cache.into())],
                )
                    .into_response();
            }
            if let Ok(bytes) = tokio::fs::read(&file).await {
                return (
                    [
                        (header::CONTENT_TYPE, content_type(path).to_string()),
                        (header::CACHE_CONTROL, cache.into()),
                        (header::ETAG, etag),
                    ],
                    Body::from(bytes),
                )
                    .into_response();
            }
        }
    }
    match tokio::fs::read_to_string(state.dist.join("index.html")).await {
        Ok(html) => {
            // The webapp's launch page leaves the tab's session token in sessionStorage
            // (`browser-launch-session.ts`); this window is the launch, so the page carries it.
            let bootstrap = format!(
                concat!(
                    "<script>",
                    "window.__HERMES_DASHBOARD_EMBEDDED_CHAT__=true;",
                    "window.__HERMES_BASE_PATH__=\"\";",
                    "window.__HERMES_AUTH_REQUIRED__=false;",
                    "window.__HERMES_INITIAL_PROFILE__=\"\";",
                    "window.__HERMES_UI_SURFACE__=\"webapp\";",
                    "window.__HERMES_DASHBOARD_PROFILE__=\"default\";",
                    "try{{sessionStorage.setItem('hermes.webapp.session.v1:'",
                    "+JSON.stringify([location.origin,'']),{token});}}catch(e){{}}",
                    // The frameless window's caption buttons (patches/0007-window-controls.patch
                    // paints them; `window_control` acts on the window).
                    "window.hermesRsWindow={{custom:true,",
                    "minimize:function(){{hermesRsWindowDo('minimize')}},",
                    "toggleMaximize:function(){{hermesRsWindowDo('toggleMaximize')}},",
                    "close:function(){{hermesRsWindowDo('close')}}}};",
                    "function hermesRsWindowDo(a){{fetch('/api/hermes-rs/window',{{method:'POST',",
                    "headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{action:a}})}})}}",
                    "</script><style>{style}</style>"
                ),
                style = HIDE_CSS,
                token = serde_json::Value::String(state.token.clone()),
            );
            let html = html.replacen("</head>", &format!("{bootstrap}</head>"), 1);
            (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                html,
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::NOT_FOUND,
            format!(
                "Hermes's webapp isn't built at {} (see crates/desktop/README.md)",
                state.dist.display()
            ),
        )
            .into_response(),
    }
}

/// A bundle file's `Cache-Control` and `ETag`: content-hashed `assets/` are kept for good, the
/// rest revalidated by size and modification time.
fn cache_policy(
    path: &str,
    len: u64,
    modified: Option<std::time::SystemTime>,
) -> (&'static str, String) {
    let nanos = modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (cache, format!("W/\"{len:x}-{nanos:x}\""))
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "html" => "text/html; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_policy, from_this_app};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn only_the_apps_own_page_gets_in() {
        let port = 47731;
        // The window's page, and requests without an Origin (same-origin GETs, the page's assets).
        assert!(from_this_app(
            port,
            Some("127.0.0.1:47731"),
            Some("http://127.0.0.1:47731")
        ));
        assert!(from_this_app(port, Some("localhost:47731"), None));
        // A site open in the user's browser, by socket or form post.
        assert!(!from_this_app(
            port,
            Some("127.0.0.1:47731"),
            Some("https://example.com")
        ));
        assert!(!from_this_app(port, Some("127.0.0.1:47731"), Some("null")));
        // Another local port's page.
        assert!(!from_this_app(
            port,
            Some("127.0.0.1:47731"),
            Some("http://127.0.0.1:3000")
        ));
        // DNS rebinding: the site's own name, resolved to 127.0.0.1.
        assert!(!from_this_app(port, Some("evil.example:47731"), None));
        assert!(!from_this_app(port, None, None));
    }

    #[test]
    fn hashed_assets_are_kept_the_rest_revalidated() {
        let at = Some(UNIX_EPOCH + Duration::from_secs(1));
        let (cache, _) = cache_policy("assets/main-MDSzRDRC.js", 10, at);
        assert_eq!(cache, "public, max-age=31536000, immutable");
        let (cache, tag) = cache_policy("hermes.png", 10, at);
        assert_eq!(cache, "no-cache");
        // The tag changes with the file.
        assert_ne!(tag, cache_policy("hermes.png", 11, at).1);
        assert_ne!(tag, cache_policy("hermes.png", 10, Some(UNIX_EPOCH)).1);
        assert_eq!(tag, cache_policy("hermes.png", 10, at).1);
    }
}
