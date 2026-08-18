//! Loopback HTTP + SSE. Serves the strip UI and the DSH plugin.
//!
//! No auth: the boundary is 127.0.0.1 plus filesystem permissions, the same
//! posture as the herdr socket. CORS is granted to loopback origins only,
//! because the DSH plugin's host half fetches from a localhost page.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::Stream;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tower_http::services::ServeDir;
use tracing::{info, warn};

use crate::config::Config;
use crate::model::{now_ms, Harness};
use crate::store::Store;
use crate::Events;

#[derive(Clone)]
struct Api {
    store: Arc<Store>,
    events: Events,
    /// Fires once on Ctrl-C. Open SSE streams watch it, because a graceful
    /// shutdown waits for in-flight responses and an SSE stream never ends
    /// on its own — with the strip connected, Ctrl-C would hang forever.
    shutdown: broadcast::Sender<()>,
}

pub async fn serve(cfg: Arc<Config>, store: Arc<Store>, events: Events) -> Result<()> {
    let web_dir = cfg.web_dir.clone();
    if !web_dir.join("index.html").is_file() {
        warn!(dir = %web_dir.display(), "strip UI not found; API still served");
    }

    let (shutdown, _) = broadcast::channel::<()>(1);

    let app = Router::new()
        .route("/api/snapshot", get(snapshot))
        .route("/api/stream", get(stream))
        .route("/api/sessions", get(sessions))
        .route("/api/summaries", get(summaries))
        .route("/api/summary/{harness}/{session_id}", get(summary))
        .route("/api/health", get(health))
        .fallback_service(ServeDir::new(&web_dir).append_index_html_on_directories(true))
        .layer(middleware::from_fn(cors))
        .with_state(Api { store, events, shutdown: shutdown.clone() });

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, cfg.port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            return Err(anyhow!(
                "port {} is already in use — another agent-monitord is running, or set AGENT_MONITOR_PORT",
                cfg.port
            ))
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("binding {addr}"))),
    };
    info!(%addr, web = %web_dir.display(), "api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // wakes every open SSE stream so it can end itself
            let _ = shutdown.send(());
        })
        .await
        .context("http server")
}

/// Ctrl-C, or SIGTERM from a service manager. Returning drops the listener and
/// lets in-flight requests finish, so the port is free for the next start.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // No SIGTERM handler is not a reason to refuse to run.
            Err(e) => {
                warn!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await
            }
        }
    };
    tokio::select! {
        _ = ctrl_c => info!("interrupted; shutting down"),
        _ = terminate => info!("terminated; shutting down"),
    }
}

// -- handlers -----------------------------------------------------------

async fn snapshot(State(api): State<Api>) -> Result<Response, ApiError> {
    Ok(Json(api.store.snapshot()?).into_response())
}

async fn stream(State(api): State<Api>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = api.events.subscribe();
    let sd = api.shutdown.subscribe();
    let stream = futures::stream::unfold(
        (rx, sd, api.store, true),
        |(mut rx, mut sd, store, first)| async move {
            if !first {
                loop {
                    tokio::select! {
                        biased;
                        _ = sd.recv() => return None,
                        got = rx.recv() => match got {
                            Ok(()) => break,
                            // Falling behind is not an error here: the next snapshot is
                            // the whole state anyway, so coalesce and carry on.
                            Err(RecvError::Lagged(_)) => break,
                            Err(RecvError::Closed) => return None,
                        },
                    }
                }
            }
            Some((Ok(snapshot_event(&store)), (rx, sd, store, false)))
        },
    );

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn snapshot_event(store: &Store) -> Event {
    let ev = match store.snapshot() {
        Ok(snap) => Event::default().event("snapshot").json_data(&snap),
        Err(e) => return error_event(&e.to_string()),
    };
    ev.unwrap_or_else(|e| error_event(&e.to_string()))
}

fn error_event(msg: &str) -> Event {
    warn!(error = msg, "snapshot for sse failed");
    // Not named "error": EventSource dispatches that at the connection itself,
    // where it is indistinguishable from a dropped socket.
    Event::default().event("snapshot_error").data(msg.replace('\n', " "))
}

#[derive(Deserialize)]
struct SessionsQuery {
    limit: Option<i64>,
    since_ms: Option<i64>,
}

async fn sessions(
    State(api): State<Api>,
    Query(q): Query<SessionsQuery>,
) -> Result<Response, ApiError> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let since_ms = q.since_ms.unwrap_or_else(|| now_ms() - 24 * 3600 * 1000);
    Ok(Json(api.store.recent_sessions(limit, since_ms)?).into_response())
}

#[derive(Deserialize)]
struct SummariesQuery {
    /// Where the previous page ended. Absent means start at the newest.
    before_ms: Option<i64>,
    harness: Option<String>,
    limit: Option<i64>,
}

/// One page of summaries, for the list the panel opens when the card is too
/// small to hold them -- which it always is.
async fn summaries(
    State(api): State<Api>,
    Query(q): Query<SummariesQuery>,
) -> Result<Response, ApiError> {
    let limit = q.limit.unwrap_or(30).clamp(1, 200);
    let before_ms = q.before_ms.unwrap_or(i64::MAX);
    let harness = q.harness.as_deref().filter(|h| !h.is_empty() && *h != "all");
    // Asked for one more than the page, so "is there another page" is answered
    // by the same query instead of a second count over the whole table.
    let mut rows = api.store.summaries_before(before_ms, harness, limit + 1)?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    Ok(Json(json!({ "summaries": rows, "has_more": has_more })).into_response())
}

async fn summary(
    State(api): State<Api>,
    Path((harness, session_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let Some(harness) = Harness::parse(&harness) else {
        return Ok(not_found("unknown harness"));
    };
    Ok(match api.store.summary_of(harness, &session_id)? {
        Some(s) => Json(s).into_response(),
        None => not_found("no summary for that session"),
    })
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

fn not_found(msg: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": msg }))).into_response()
}

// -- CORS ---------------------------------------------------------------

async fn cors(req: Request<Body>, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| is_loopback_origin(o))
        .and_then(|o| HeaderValue::from_str(o).ok());
    let preflight = req.method() == Method::OPTIONS;

    let mut res = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };

    if let Some(origin) = origin {
        let h = res.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        h.insert(header::VARY, HeaderValue::from_static("origin"));
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("content-type"),
        );
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    }
    res
}

fn is_loopback_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    if rest.contains('/') {
        return false;
    }
    let host = match rest.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => rest,
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

// -- errors -------------------------------------------------------------

struct ApiError(anyhow::Error);

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        warn!(error = %self.0, "request failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}
