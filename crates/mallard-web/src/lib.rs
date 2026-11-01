//! The Web UI's server, run by `mallard web` (ADR 0016): the React app built in,
//! plus a small same-origin proxy to the server (`mallard serve`), so browser code never holds
//! a token. It is a client like the TUI: HTTP only, no DuckDB, no server code.

use std::{
    collections::HashMap,
    net::SocketAddr,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri,
        header::{self, CACHE_CONTROL, CONTENT_TYPE, COOKIE, HOST, ORIGIN, SET_COOKIE},
        uri::Authority,
    },
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use mallard_common::http::{ApiErr, J};
use serde::{Deserialize, Serialize};
use tower_http::{catch_panic::CatchPanicLayer, compression::CompressionLayer, trace::TraceLayer};

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// Holds a random session id, never the token: cookies are not isolated by port,
/// so another service on this host may see it. `HttpOnly`: page code cannot.
const COOKIE_NAME: &str = "mallard_session";
/// The same under TLS. The prefix makes browsers refuse one set from elsewhere.
const SECURE_COOKIE_NAME: &str = "__Host-mallard_session";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// Tokens are long and random; this only keeps guessing from being free.
const FAILED_LOGIN_DELAY: Duration = Duration::from_millis(250);
/// More than any list the UI asks for; a misbehaving server cannot fill our memory.
const MAX_RESPONSE: usize = 64 * 1024 * 1024;
/// Writes must carry this header: a cross-site form cannot set it, and a
/// cross-site fetch needs a CORS preflight this server never answers.
const WRITE_HEADER: &str = "x-mallard-web";
/// The only write is `{"status": …, "reason": …}`.
const MAX_BODY: usize = 64 * 1024;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);
/// No inline scripts. `style-src 'unsafe-inline'`: Kumo and ECharts set inline styles.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                   font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
                   frame-ancestors 'none'";

const NOT_BUILT: &str = "<!doctype html><meta charset=utf-8><title>mallard</title>\
<h1>The UI is not built into this binary</h1>\
<p>Run <code>npm ci &amp;&amp; npm run build</code> in <code>crates/mallard-web/ui</code>, \
then build <code>mallard-web</code> again.</p>";

#[derive(Clone)]
pub struct Config {
    server_url: String,
    /// For showing: without any credentials in the URL.
    display_url: String,
    /// Set: the proxy uses it (loopback only). Unset: users sign in.
    token: Option<String>,
}

impl Config {
    pub fn new(server_url: &str, token: Option<String>) -> Result<Self> {
        let server_url = server_url.trim_end_matches('/').to_string();
        let parsed = reqwest::Url::parse(&server_url).with_context(|| format!("invalid server URL {server_url:?}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("invalid server URL {server_url:?}: expected http or https");
        }
        let mut shown = parsed;
        let _ = shown.set_username("");
        let _ = shown.set_password(None);
        let display_url = shown.as_str().trim_end_matches('/').to_string();
        Ok(Self { server_url, display_url, token })
    }

    /// The server's address, safe to show.
    pub fn display_url(&self) -> &str {
        &self.display_url
    }

    pub fn mode_description(&self) -> &'static str {
        if self.token.is_some() { "using the given token" } else { "users sign in" }
    }
}

/// Serve until Ctrl-C. For a binary to call: it installs logging, as binaries do.
pub async fn run(url: &str, token: Option<&str>, addr: &str) -> Result<()> {
    mallard_common::init_tracing(mallard_common::LogFormat::Text);
    let cfg = Config::new(url, token.filter(|t| !t.trim().is_empty()).map(str::to_string))?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("listening on {addr}"))?;
    let local = listener.local_addr()?;
    check_exposure(&cfg, &local)?;
    println!("mallard web: http://{local} (server {}, {})", cfg.display_url(), cfg.mode_description());
    let app = router(cfg)?;
    axum::serve(listener, app).with_graceful_shutdown(mallard_common::shutdown_signal()).await?;
    Ok(())
}

/// A token in the process gives everyone who can reach the port its rights,
/// so it is only allowed on this machine.
pub fn check_exposure(cfg: &Config, addr: &SocketAddr) -> Result<()> {
    if cfg.token.is_some() && !addr.ip().is_loopback() {
        bail!(
            "refusing to listen on {addr} with a token: everyone who can reach it would get the token's rights \
             (listen on 127.0.0.1, or leave out --token and unset MALLARD_TOKEN, so users sign in)"
        );
    }
    Ok(())
}

#[derive(Clone)]
struct App {
    cfg: Config,
    http: reqwest::Client,
    /// Signed-in users: session id → API token. A restart signs everyone out.
    sessions: Arc<Mutex<HashMap<String, (String, Instant)>>>,
    /// One sign-in check at a time, so a failed one's delay limits guessing.
    login_gate: Arc<tokio::sync::Mutex<()>>,
}

impl App {
    fn session_token(&self, id: &str) -> Option<String> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(id).filter(|(_, expires)| *expires > Instant::now()).map(|(token, _)| token.clone())
    }

    fn new_session(&self, token: &str) -> String {
        let id = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, (_, expires)| *expires > Instant::now());
        sessions.insert(id.clone(), (token.to_string(), Instant::now() + SESSION_TTL));
        id
    }

    fn end_session(&self, id: &str) {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }
}

/// Everything `mallard-web` serves.
pub fn router(cfg: Config) -> Result<Router> {
    let http = reqwest::Client::builder()
        .timeout(UPSTREAM_TIMEOUT)
        // A redirect could carry the bearer token somewhere else.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building the HTTP client")?;
    let app = App { cfg, http, sessions: Arc::default(), login_gate: Arc::default() };
    Ok(Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/web/session", get(session))
        .route("/web/login", post(login))
        .route("/web/logout", post(logout))
        .route("/sentry/v1/{*rest}", axum::routing::any(proxy))
        .route("/otel/api/v1/{*rest}", axum::routing::any(proxy))
        .route("/api/v1/{*rest}", axum::routing::any(proxy))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .layer(middleware::map_response(security_headers))
        .layer(CompressionLayer::new())
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(app))
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(HeaderName::from_static("cross-origin-resource-policy"), HeaderValue::from_static("same-origin"));
    res
}

fn forbidden(message: &str) -> ApiErr {
    ApiErr::new(StatusCode::FORBIDDEN, "forbidden", message)
}

/// A `Host` value that names this machine. Anything else, including a value with
/// user info (`localhost:80@evil.example`), is not.
fn is_loopback_name(host: &str) -> bool {
    if host.contains('@') {
        return false;
    }
    Authority::from_str(host)
        .is_ok_and(|a| matches!(a.host().to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]"))
}

/// Cross-site protection, before any handler.
async fn guard(State(app): State<App>, req: Request, next: Next) -> Result<Response, ApiErr> {
    let headers = req.headers();
    let host = headers.get(HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    // DNS rebinding: evil.example resolving to 127.0.0.1 would otherwise read
    // through the proxy with our token. With sign-in its origin has no cookie.
    if app.cfg.token.is_some() && !is_loopback_name(host) {
        return Err(forbidden("mallard web only answers to localhost when it holds a token"));
    }
    // Another site, or another port on this host, may not even read: a timing
    // difference alone would tell it what the logs contain.
    let path = req.uri().path();
    if ["/sentry/", "/otel/", "/api/", "/web/"].iter().any(|p| path.starts_with(p))
        && headers.get("sec-fetch-site").is_some_and(|v| v == "cross-site" || v == "same-site")
    {
        return Err(forbidden("cross-site requests are not allowed"));
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        if !headers.contains_key(WRITE_HEADER) {
            return Err(forbidden("writes need the x-mallard-web header"));
        }
        if let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) {
            let origin_host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
            let forwarded = headers.get("x-forwarded-host").and_then(|v| v.to_str().ok());
            if origin_host != host && Some(origin_host) != forwarded {
                return Err(forbidden("cross-origin writes are not allowed"));
            }
        }
    }
    Ok(next.run(req).await)
}

/// Every session id the request carries. Another port on this host can set a
/// cookie that shadows ours, so all of them are tried.
fn cookie_sessions(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .filter(|(name, _)| *name == COOKIE_NAME || *name == SECURE_COOKIE_NAME)
        .map(|(_, value)| value)
}

/// The token for this request: the configured one, or the signed-in user's.
fn request_token(app: &App, headers: &HeaderMap) -> Option<String> {
    app.cfg.token.clone().or_else(|| cookie_sessions(headers).find_map(|id| app.session_token(id)))
}

/// The proxy passes only what the TUI uses (ADR 0016). Anything else, such
/// as SQL, maintenance or project creation, is not reachable from a browser.
fn allowed(method: &Method, path: &str) -> bool {
    // Never let a path climb out of the API prefix on the way upstream.
    let lower = path.to_ascii_lowercase();
    if lower.contains("..") || lower.contains("//") || lower.contains('\\') {
        return false;
    }
    if ["%2e", "%2f", "%5c"].iter().any(|escape| lower.contains(escape)) {
        return false;
    }
    let reads = path.starts_with("/sentry/v1/")
        || path.starts_with("/otel/api/v1/")
        || ["/api/v1/apps", "/api/v1/find"].contains(&path);
    match *method {
        Method::GET => reads,
        Method::PATCH => {
            let segments: Vec<_> = path.split('/').collect();
            matches!(segments[..], ["", "sentry", "v1", "projects", p, "issues", id] if !p.is_empty() && !id.is_empty())
        }
        _ => false,
    }
}

fn upstream_error(app: &App, e: &reqwest::Error) -> ApiErr {
    tracing::warn!(error = %e, "upstream request failed");
    if e.is_timeout() {
        ApiErr::new(StatusCode::GATEWAY_TIMEOUT, "timeout", format!("{} took too long to answer", app.cfg.display_url))
    } else {
        ApiErr::new(StatusCode::BAD_GATEWAY, "unavailable", format!("cannot reach {}", app.cfg.display_url))
    }
}

fn bearer(token: &str) -> Result<HeaderValue, ApiErr> {
    let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| ApiErr::unauthorized())?;
    value.set_sensitive(true);
    Ok(value)
}

async fn proxy(State(app): State<App>, req: Request) -> Result<Response, ApiErr> {
    let (parts, body) = req.into_parts();
    if !allowed(&parts.method, parts.uri.path()) {
        return Err(forbidden("mallard-web does not forward this request"));
    }
    let Some(token) = request_token(&app, &parts.headers) else {
        return Err(ApiErr::new(StatusCode::UNAUTHORIZED, "unauthorized", "sign in first"));
    };
    let target = parts.uri.path_and_query().map_or(parts.uri.path(), |pq| pq.as_str());
    let mut upstream = app
        .http
        .request(parts.method, format!("{}{target}", app.cfg.server_url))
        .header(header::AUTHORIZATION, bearer(&token)?);
    if let Some(ct) = parts.headers.get(CONTENT_TYPE) {
        upstream = upstream.header(CONTENT_TYPE, ct);
    }
    let body = to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| ApiErr::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "request body too large"))?;
    if !body.is_empty() {
        upstream = upstream.body(body);
    }
    let mut res = upstream.send().await.map_err(|e| upstream_error(&app, &e))?;

    let status = res.status();
    let keep = [CONTENT_TYPE, header::RETRY_AFTER, HeaderName::from_static("x-next-cursor")];
    let mut out = Response::builder().status(status).header(CACHE_CONTROL, "no-store");
    for name in keep {
        if let Some(v) = res.headers().get(&name) {
            out = out.header(name, v);
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| upstream_error(&app, &e))? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err(ApiErr::new(
                StatusCode::BAD_GATEWAY,
                "too_large",
                "the answer is too large; narrow the window",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    out.body(Body::from(bytes)).map_err(|_| ApiErr::internal())
}

#[derive(Serialize)]
struct Session<'a> {
    /// `token`: the process holds one. `login`: users sign in.
    mode: &'static str,
    authenticated: bool,
    /// Only once signed in.
    server_url: Option<&'a str>,
    version: &'static str,
}

async fn session(State(app): State<App>, headers: HeaderMap) -> impl IntoResponse {
    let authenticated = request_token(&app, &headers).is_some();
    let body = Session {
        mode: if app.cfg.token.is_some() { "token" } else { "login" },
        authenticated,
        server_url: authenticated.then_some(app.cfg.display_url.as_str()),
        version: env!("CARGO_PKG_VERSION"),
    };
    ([(CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

#[derive(Deserialize)]
struct Login {
    token: String,
}

/// Always `Secure`, except on this machine, where browsers allow plain HTTP. Over
/// plain HTTP elsewhere the browser refuses the cookie and the sign-in page says so.
fn needs_secure(headers: &HeaderMap) -> bool {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let host = text("x-forwarded-host").or(text("host")).unwrap_or("");
    !is_loopback_name(host) || text("x-forwarded-proto").is_some_and(|p| p.to_ascii_lowercase().contains("https"))
}

fn session_cookie(value: &str, max_age: Option<u32>, headers: &HeaderMap) -> Result<HeaderValue, ApiErr> {
    let (name, secure) = if needs_secure(headers) { (SECURE_COOKIE_NAME, "; Secure") } else { (COOKIE_NAME, "") };
    let age = max_age.map_or(String::new(), |a| format!("; Max-Age={a}"));
    HeaderValue::from_str(&format!("{name}={value}; Path=/; HttpOnly; SameSite=Strict{secure}{age}"))
        .map_err(|_| ApiErr::internal())
}

/// Checks the token against the server; only an accepted token gets a session.
async fn login(State(app): State<App>, headers: HeaderMap, J(body): J<Login>) -> Result<Response, ApiErr> {
    if app.cfg.token.is_some() {
        return Err(ApiErr::bad_request("mallard web was started with a token"));
    }
    let token = body.token.trim();
    let _gate = app.login_gate.lock().await;
    let res = app
        .http
        .get(format!("{}/sentry/v1/info", app.cfg.server_url))
        .header(header::AUTHORIZATION, bearer(token)?)
        .send()
        .await
        .map_err(|e| upstream_error(&app, &e))?;
    match res.status() {
        s if s.is_success() => {
            let cookie = session_cookie(&app.new_session(token), None, &headers)?;
            Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response())
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            tokio::time::sleep(FAILED_LOGIN_DELAY).await;
            Err(ApiErr::new(StatusCode::UNAUTHORIZED, "unauthorized", "the server does not accept this token"))
        }
        s => Err(ApiErr::new(StatusCode::BAD_GATEWAY, "unavailable", format!("the server answered {s}"))),
    }
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<Response, ApiErr> {
    for id in cookie_sessions(&headers) {
        app.end_session(id);
    }
    let cookie = session_cookie("", Some(0), &headers)?;
    Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response())
}

fn index() -> Response {
    let page = ASSETS.iter().find(|(p, ..)| *p == "/index.html");
    let mut res = match page {
        Some((_, _, bytes)) => Html(*bytes).into_response(),
        None => Html(NOT_BUILT).into_response(),
    };
    res.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

/// Built files by path. Vite puts everything it references under `/assets/`, so
/// a miss there is a real 404; any other path is a screen of the app, so the
/// page's router handles deep links (metric names have dots in them).
async fn asset(uri: Uri) -> Response {
    let path = uri.path();
    if path == "/" || path == "/index.html" {
        return index();
    }
    if let Some((_, content_type, bytes)) = ASSETS.iter().find(|(p, ..)| *p == path) {
        // Vite puts a content hash in everything under /assets/.
        let cache = if path.starts_with("/assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
        return ([(CONTENT_TYPE, *content_type), (CACHE_CONTROL, cache)], *bytes).into_response();
    }
    if path.starts_with("/assets/") {
        return ApiErr::not_found("no such file").into_response();
    }
    index()
}
