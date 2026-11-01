//! Sentry ingest endpoints.
//!
//! - `POST /api/{project_id}/envelope/` (all current SDKs)
//! - `POST /api/{project_id}/store/` (legacy JSON events)
//!
//! A request is answered only after its event was committed (group commit
//! by the writer), so `200` means stored. When the write queue is full we
//! answer `429` with `Retry-After`, which SDKs respect.

// Handlers return a ready `Response` as their error: idiomatic in axum and
// only on the (cold) rejection path, so its size does not matter.
#![allow(clippy::result_large_err)]

use crate::AppState;
use crate::db::{EnqueueError, EventWrite, WriteCmd};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::Utc;
use sdl_protocol::envelope::Envelope;
use sdl_protocol::{NormalizeContext, ProcessedEvent, auth, normalize_event, symbolicate};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::decompression::RequestDecompressionLayer;

pub fn router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::CONTENT_ENCODING,
            header::AUTHORIZATION,
            header::ACCEPT,
            header::ORIGIN,
            header::HeaderName::from_static("x-sentry-auth"),
            header::HeaderName::from_static("x-requested-with"),
            header::HeaderName::from_static("sentry-trace"),
            header::HeaderName::from_static("baggage"),
        ])
        .expose_headers([
            header::RETRY_AFTER,
            header::HeaderName::from_static("x-sentry-error"),
            header::HeaderName::from_static("x-sentry-rate-limits"),
        ])
        .max_age(Duration::from_secs(3600));
    Router::new()
        .route("/api/{project_id}/envelope/", post(envelope))
        .route("/api/{project_id}/envelope", post(envelope))
        .route("/api/{project_id}/store/", post(store))
        .route("/api/{project_id}/store", post(store))
        .layer(RequestDecompressionLayer::new())
        .layer(tower::limit::GlobalConcurrencyLimitLayer::new(state.cfg.ingest_concurrency.max(1)))
        .layer(cors)
        .with_state(state)
}

/// Error response in Sentry's shape: `{"detail": "..."}` + `X-Sentry-Error`.
fn sentry_error(status: StatusCode, detail: &str) -> Response {
    metrics::counter!("sdl_ingest_rejected_total", "status" => status.as_u16().to_string()).increment(1);
    let mut r = (status, axum::Json(json!({ "detail": detail }))).into_response();
    if let Ok(v) = HeaderValue::from_str(detail) {
        r.headers_mut().insert("x-sentry-error", v);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
        r.headers_mut().insert("x-sentry-rate-limits", HeaderValue::from_static("5::organization"));
    }
    if status == StatusCode::SERVICE_UNAVAILABLE {
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
    }
    r
}

struct Incoming {
    project_id: u32,
    /// Key from a header or the query string (already checked).
    header_key: Option<String>,
    client_ip: Option<String>,
    body: Bytes,
}

async fn read_request(state: &AppState, project_id: &str, req: Request) -> Result<Incoming, Response> {
    let project_id: u32 =
        project_id.parse().map_err(|_| sentry_error(StatusCode::BAD_REQUEST, "invalid project id"))?;
    let (parts, body) = req.into_parts();
    let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string());
    let forwarded = state
        .cfg
        .trust_forwarded_for
        .then(|| parts.headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()))
        .flatten()
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string());
    // Authenticate before reading (and decompressing) the body whenever the
    // key is in a header or the query string, so unauthenticated clients
    // cannot make us inflate large bodies. Requests that carry the key only
    // in the envelope (tunnels) get a small body limit.
    let query = parts.uri.query().map(str::to_string);
    let header_key = key_from_headers(&parts.headers, query.as_deref());
    let limit = match &header_key {
        Some(key) => {
            check_key(state, project_id, key)?;
            state.cfg.max_body_bytes
        }
        None => state.cfg.max_body_bytes.min(UNAUTHENTICATED_BODY_LIMIT),
    };
    let body = axum::body::to_bytes(body, limit).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("length limit") {
            sentry_error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
        } else {
            sentry_error(StatusCode::BAD_REQUEST, "could not read request body (bad compression?)")
        }
    })?;
    Ok(Incoming { project_id, header_key, client_ip: forwarded.or(peer), body })
}

/// Body limit for requests whose key is only inside the envelope.
const UNAUTHENTICATED_BODY_LIMIT: usize = 2 * 1024 * 1024;

fn key_from_headers(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    headers
        .get("x-sentry-auth")
        .and_then(|v| v.to_str().ok())
        .and_then(auth::parse_auth_header)
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.trim_start().to_ascii_lowercase().starts_with("sentry "))
                .and_then(auth::parse_auth_header)
        })
        .or_else(|| query.and_then(auth::parse_query))
}

fn check_key(state: &AppState, project_id: u32, key: &str) -> Result<(), Response> {
    match state.db.caches.project(project_id) {
        Some(p) if constant_time_eq(p.public_key.as_bytes(), key.as_bytes()) => Ok(()),
        Some(_) => Err(sentry_error(StatusCode::UNAUTHORIZED, "invalid api key")),
        None => Err(sentry_error(StatusCode::NOT_FOUND, "project not found")),
    }
}

/// Check the key from the envelope's `dsn` header when there was none in
/// the headers or query (those were checked before reading the body).
fn authenticate(state: &AppState, inc: &Incoming, envelope_dsn: Option<&str>) -> Result<(), Response> {
    if inc.header_key.is_some() {
        return Ok(());
    }
    let key = envelope_dsn.and_then(|d| sdl_protocol::dsn::Dsn::parse(d).ok()).map(|d| d.public_key);
    match key {
        Some(key) => check_key(state, inc.project_id, &key),
        None => Err(sentry_error(StatusCode::UNAUTHORIZED, "missing authorization information")),
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn envelope(State(state): State<Arc<AppState>>, Path(project_id): Path<String>, req: Request) -> Response {
    let started = Instant::now();
    let inc = match read_request(&state, &project_id, req).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    let env = match Envelope::parse(inc.body.clone()) {
        Ok(e) => e,
        Err(e) => return sentry_error(StatusCode::BAD_REQUEST, &format!("invalid envelope: {e}")),
    };
    if let Err(r) = authenticate(&state, &inc, env.dsn()) {
        return r;
    }
    let mut stored_id: Option<String> = None;
    for item in &env.items {
        let ty = item.ty();
        metrics::counter!("sdl_ingest_items_total", "type" => ty.to_string()).increment(1);
        match ty {
            "event" | "error" => {
                if item.payload.len() > state.cfg.max_event_bytes {
                    return sentry_error(StatusCode::PAYLOAD_TOO_LARGE, "event too large");
                }
                let raw: Value = match serde_json::from_slice(&item.payload) {
                    Ok(v) => v,
                    Err(e) => return sentry_error(StatusCode::BAD_REQUEST, &format!("invalid event JSON: {e}")),
                };
                match process_event(&state, &inc, raw, env.event_id()).await {
                    Ok(id) => stored_id = Some(id),
                    Err(r) => return r,
                }
            }
            // Accepted and dropped: transactions, sessions, client reports,
            // attachments, replays, profiles, check-ins, logs, feedback...
            // This server stores errors only (see docs/compatibility.md).
            _ => metrics::counter!("sdl_ingest_items_dropped_total", "type" => ty.to_string()).increment(1),
        }
    }
    metrics::histogram!("sdl_ingest_request_seconds", "endpoint" => "envelope").record(started.elapsed().as_secs_f64());
    let id = stored_id.or_else(|| env.event_id().map(str::to_string));
    axum::Json(match id {
        Some(id) => json!({ "id": id }),
        None => json!({}),
    })
    .into_response()
}

async fn store(State(state): State<Arc<AppState>>, Path(project_id): Path<String>, req: Request) -> Response {
    let started = Instant::now();
    let inc = match read_request(&state, &project_id, req).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    if let Err(r) = authenticate(&state, &inc, None) {
        return r;
    }
    let Some(json_bytes) = sdl_protocol::store::decode_store_body(&inc.body, state.cfg.max_event_bytes) else {
        return sentry_error(StatusCode::BAD_REQUEST, "invalid or too large event body");
    };
    if json_bytes.len() > state.cfg.max_event_bytes {
        return sentry_error(StatusCode::PAYLOAD_TOO_LARGE, "event too large");
    }
    let raw: Value = match serde_json::from_slice(&json_bytes) {
        Ok(v) => v,
        Err(e) => return sentry_error(StatusCode::BAD_REQUEST, &format!("invalid event JSON: {e}")),
    };
    // Transactions sent to /store/ by very old SDKs are not errors.
    if raw.get("type").and_then(Value::as_str) == Some("transaction") {
        metrics::counter!("sdl_ingest_items_dropped_total", "type" => "transaction").increment(1);
        return axum::Json(json!({ "id": raw.get("event_id") })).into_response();
    }
    let r = match process_event(&state, &inc, raw, None).await {
        Ok(id) => axum::Json(json!({ "id": id })).into_response(),
        Err(r) => r,
    };
    metrics::histogram!("sdl_ingest_request_seconds", "endpoint" => "store").record(started.elapsed().as_secs_f64());
    r
}

/// Normalize, symbolicate, group and store one event. Returns its id.
async fn process_event(state: &AppState, inc: &Incoming, raw: Value, hint: Option<&str>) -> Result<String, Response> {
    if raw.get("type").and_then(Value::as_str).is_some_and(|t| t != "error" && t != "default") {
        // e.g. "transaction" or "feedback" sent as an event item.
        let id = raw.get("event_id").and_then(Value::as_str).unwrap_or_default().to_string();
        metrics::counter!("sdl_ingest_items_dropped_total", "type" => "non_error_event").increment(1);
        return Ok(id);
    }
    let received_at = Utc::now();
    let mut ctx = NormalizeContext::new(received_at);
    ctx.client_ip = inc.client_ip.clone();
    ctx.event_id_hint = hint.map(str::to_string);
    // Normalizing, symbolicating and grouping are CPU work: keep them off
    // the async worker threads.
    let blocking_err = |_| sentry_error(StatusCode::INTERNAL_SERVER_ERROR, "event processing failed");
    let ev = tokio::task::spawn_blocking(move || normalize_event(raw, &ctx)).await.map_err(blocking_err)?;
    let maps = state.sourcemaps.resolve(&state.db, inc.project_id, &ev).await;
    let (processed, payload_json) = tokio::task::spawn_blocking(move || {
        let mut ev = ev;
        if !maps.is_empty() {
            let n = symbolicate::apply(&mut ev, &maps);
            metrics::counter!("sdl_frames_symbolicated_total").increment(n as u64);
        }
        let processed = ProcessedEvent::from_normalized(ev, received_at);
        let payload_json = processed.payload.to_string();
        (processed, payload_json)
    })
    .await
    .map_err(blocking_err)?;
    let id = processed.event_id.simple().to_string();
    let dedupe_key = (inc.project_id, processed.event_id);
    if state.recent_events.contains_key(&dedupe_key) {
        metrics::counter!("sdl_ingest_duplicates_total").increment(1);
        return Ok(id);
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let cmd =
        WriteCmd::Event(Box::new(EventWrite { project_id: inc.project_id, event: processed, payload_json, reply: tx }));
    match state.db.try_send(cmd) {
        Ok(()) => {}
        Err(EnqueueError::Full) => {
            return Err(sentry_error(StatusCode::TOO_MANY_REQUESTS, "write queue full, retry later"));
        }
        Err(EnqueueError::Stopped) => {
            return Err(sentry_error(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down"));
        }
    }
    match rx.await {
        Ok(Ok(())) => {
            state.recent_events.insert(dedupe_key, ());
            metrics::counter!("sdl_ingest_events_total").increment(1);
            Ok(id)
        }
        Ok(Err(e)) => {
            tracing::error!(error = format!("{e:#}"), "storing event failed");
            Err(sentry_error(StatusCode::SERVICE_UNAVAILABLE, "storing the event failed"))
        }
        Err(_) => Err(sentry_error(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down")),
    }
}
