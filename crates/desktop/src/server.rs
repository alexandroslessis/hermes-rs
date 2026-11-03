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
}

pub async fn serve(listener: tokio::net::TcpListener, state: Arc<ServerState>) {
    let app = Router::new()
        .route("/api/ws", get(websocket))
        .route("/api/{*rest}", any(api))
        .fallback(get(static_file))
        .with_state(state);
    if let Err(error) = axum::serve(listener, app).await {
        tracing::error!("server stopped: {error}");
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
        if file.is_file() {
            if let Ok(bytes) = tokio::fs::read(&file).await {
                return (
                    [(header::CONTENT_TYPE, content_type(path))],
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
                    // Bundled plugins whose backend hermes-rs lacks start off; a choice the
                    // user makes in Capabilities ▸ Plugins is kept (`plugins-store.ts`).
                    "try{{var k='hermes.desktop.pluginDecisions.v2',",
                    "d=JSON.parse(localStorage.getItem(k)||'{{}}');",
                    "['hermes-bots','kanban','radio'].forEach(function(i){{if(!(i in d))d[i]=false}});",
                    "localStorage.setItem(k,JSON.stringify(d));}}catch(e){{}}",
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
