//! The proxy and session rules, against a fake upstream that records requests.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, Method, StatusCode, header},
    response::IntoResponse,
};
use mallard_web::{Config, check_exposure, router};
use tower::ServiceExt;

const GOOD: &str = "good-token";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path_and_query: String,
    auth: Option<String>,
    cookie: Option<String>,
    body: String,
}

async fn upstream() -> (String, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let app = Router::new().fallback(move |req: Request| {
        let log = log.clone();
        async move {
            let (parts, body) = req.into_parts();
            let auth = parts.headers.get(header::AUTHORIZATION).map(|v| v.to_str().unwrap().to_string());
            let body = String::from_utf8(to_bytes(body, 1024).await.unwrap().to_vec()).unwrap();
            log.lock().unwrap().push(Seen {
                method: parts.method.to_string(),
                path_and_query: parts.uri.path_and_query().unwrap().to_string(),
                auth: auth.clone(),
                cookie: parts.headers.get(header::COOKIE).map(|v| v.to_str().unwrap().to_string()),
                body,
            });
            if auth.as_deref() != Some("Bearer good-token") {
                return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized", "message": "no"})))
                    .into_response();
            }
            let mut h = HeaderMap::new();
            h.insert("x-next-cursor", "abc".parse().unwrap());
            h.insert("x-internal", "secret".parse().unwrap());
            (h, Json(serde_json::json!({"ok": true}))).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn get(path: &str) -> axum::http::request::Builder {
    Request::builder().method(Method::GET).uri(path).header(header::HOST, "localhost:8088")
}

fn write(method: Method, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "localhost:8088")
        .header("x-mallard-web", "1")
        .header(header::CONTENT_TYPE, "application/json")
}

async fn with_token() -> (Router, Arc<Mutex<Vec<Seen>>>) {
    let (url, seen) = upstream().await;
    (router(Config::new(&url, Some(GOOD.into())).unwrap()).unwrap(), seen)
}

async fn with_login() -> (Router, Arc<Mutex<Vec<Seen>>>) {
    let (url, seen) = upstream().await;
    (router(Config::new(&url, None).unwrap()).unwrap(), seen)
}

async fn send(app: &Router, req: axum::http::request::Builder) -> axum::response::Response {
    app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
}

async fn text(res: axum::response::Response) -> String {
    String::from_utf8(to_bytes(res.into_body(), 1 << 22).await.unwrap().to_vec()).unwrap()
}

#[tokio::test]
async fn reads_are_forwarded_with_the_token_and_only_known_headers_come_back() {
    let (app, seen) = with_token().await;
    let res = send(&app, get("/otel/api/v1/traces?since=1h&limit=2").header(header::COOKIE, "other=1")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-next-cursor"], "abc");
    assert!(res.headers().get("x-internal").is_none());
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].path_and_query, "/otel/api/v1/traces?since=1h&limit=2");
    assert_eq!(seen[0].auth.as_deref(), Some("Bearer good-token"));
    assert_eq!(seen[0].cookie, None, "browser cookies never go upstream");
}

#[tokio::test]
async fn only_the_issue_status_write_passes() {
    let (app, seen) = with_token().await;
    for (method, path) in [
        (Method::POST, "/otel/api/v1/sql"),
        (Method::POST, "/sentry/v1/sql"),
        (Method::POST, "/sentry/v1/maintenance"),
        (Method::POST, "/sentry/v1/projects"),
        (Method::DELETE, "/sentry/v1/projects/web/issues/1"),
        (Method::PATCH, "/sentry/v1/projects/web"),
        (Method::PATCH, "/sentry/v1/projects/web/issues/1/x"),
        (Method::PUT, "/otel/api/v1/traces"),
    ] {
        let res = send(&app, write(method.clone(), path)).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} {path}");
    }
    assert!(seen.lock().unwrap().is_empty(), "nothing reached the server");

    let req = write(Method::PATCH, "/sentry/v1/projects/web/issues/1").body(Body::from(r#"{"status":"resolved"}"#));
    let res = app.clone().oneshot(req.unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].method, "PATCH");
    assert_eq!(seen[0].body, r#"{"status":"resolved"}"#);
}

#[tokio::test]
async fn the_apps_and_find_reads_pass_and_nothing_else_of_that_api() {
    let (app, seen) = with_token().await;
    for path in ["/api/v1/apps?since=1h", "/api/v1/find?id=abc"] {
        let res = send(&app, get(path)).await;
        assert_eq!(res.status(), StatusCode::OK, "{path}");
    }
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(seen.lock().unwrap()[0].path_and_query, "/api/v1/apps?since=1h");
    for path in ["/api/v1/other", "/api/v1/apps/x", "/api/v1/apps/../sql"] {
        let res = send(&app, get(path)).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let res = send(&app, get("/api/v1/apps").method("POST").header("x-mallard-web", "1")).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(seen.lock().unwrap().len(), 2);
    let res = send(&app, get("/api/v1/apps").header("sec-fetch-site", "cross-site")).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "other sites may not read it either");
}

#[tokio::test]
async fn paths_cannot_leave_the_api() {
    let (app, seen) = with_token().await;
    for path in [
        "/sentry/v1/../api/x/envelope",
        "/otel/api/v1/%2e%2e/%2e%2e/metrics",
        "/otel/api/v1/%2E%2E/x",
        "/sentry/v1/a%2fb",
        "/sentry/v1//x",
        "/sentry/v1/a%5cb",
    ] {
        let res = send(&app, get(path)).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{path}");
    }
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn writes_need_the_header_and_the_same_origin() {
    let (app, seen) = with_token().await;
    let path = "/sentry/v1/projects/web/issues/1";
    let no_header = Request::builder().method(Method::PATCH).uri(path).header(header::HOST, "localhost:8088");
    assert_eq!(send(&app, no_header).await.status(), StatusCode::FORBIDDEN);
    let cross = write(Method::PATCH, path).header(header::ORIGIN, "https://evil.example");
    assert_eq!(send(&app, cross).await.status(), StatusCode::FORBIDDEN);
    assert!(seen.lock().unwrap().is_empty());
    let same = write(Method::PATCH, path).header(header::ORIGIN, "http://localhost:8088");
    assert_eq!(send(&app, same).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn with_a_token_only_loopback_hosts_are_answered() {
    let (app, seen) = with_token().await;
    for host in
        ["evil.example", "evil.example:8088", "localhost.evil.example", "localhost:80@evil.example", "a@localhost"]
    {
        let req = Request::builder().uri("/otel/api/v1/traces").header(header::HOST, host);
        assert_eq!(send(&app, req).await.status(), StatusCode::FORBIDDEN, "{host}");
    }
    assert!(seen.lock().unwrap().is_empty());
    for host in ["localhost", "localhost:8088", "127.0.0.1:1", "[::1]:8088", "[::1]"] {
        let req = Request::builder().uri("/otel/api/v1/traces").header(header::HOST, host);
        assert_eq!(send(&app, req).await.status(), StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn a_token_is_refused_on_a_public_address() {
    let cfg = Config::new("http://x:1", Some("t".into())).unwrap();
    let public: SocketAddr = "0.0.0.0:8088".parse().unwrap();
    assert!(check_exposure(&cfg, &public).unwrap_err().to_string().contains("refusing"));
    assert!(check_exposure(&cfg, &"127.0.0.1:8088".parse().unwrap()).is_ok());
    assert!(check_exposure(&cfg, &"[::1]:8088".parse().unwrap()).is_ok());
    let open = Config::new("http://x:1", None).unwrap();
    assert!(check_exposure(&open, &public).is_ok());
}

fn login_req(token: &str) -> Request {
    write(Method::POST, "/web/login").body(Body::from(serde_json::json!({ "token": token }).to_string())).unwrap()
}

#[tokio::test]
async fn sign_in_checks_the_token_and_keeps_it_out_of_page_code() {
    let (app, seen) = with_login().await;

    // Not signed in.
    let res = send(&app, get("/otel/api/v1/traces")).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(seen.lock().unwrap().is_empty());
    let body = text(send(&app, get("/web/session")).await).await;
    let session: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(session["mode"], "login");
    assert_eq!(session["authenticated"], false);
    assert_eq!(session["server_url"], serde_json::Value::Null, "the upstream stays private until signed in");

    // A wrong token gets no cookie.
    let res = app.clone().oneshot(login_req("nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(res.headers().get(header::SET_COOKIE).is_none());

    // A good token gets an HttpOnly, SameSite=Strict cookie with a random id: other
    // ports on this host see the cookie, so it must not hold the token in any form.
    let res = app.clone().oneshot(login_req(GOOD)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(!cookie.contains("Secure"), "this machine, plain HTTP");
    assert!(!cookie.contains(GOOD));
    use base64::Engine;
    assert!(!cookie.contains(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(GOOD)), "{cookie}");
    let pair = cookie.split(';').next().unwrap().to_string();

    // The cookie becomes the bearer token and does not travel upstream.
    let res = send(&app, get("/otel/api/v1/traces").header(header::COOKIE, &pair)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let last = seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(last.auth.as_deref(), Some("Bearer good-token"));
    assert_eq!(last.cookie, None);
    let body = text(send(&app, get("/web/session").header(header::COOKIE, &pair)).await).await;
    assert!(!body.contains(GOOD), "session never shows the token");

    // A cookie set from another port cannot shadow ours.
    let both = format!("mallard_session=junk; {pair}");
    assert_eq!(send(&app, get("/otel/api/v1/traces").header(header::COOKIE, &both)).await.status(), StatusCode::OK);

    // Sign out clears it and ends the session on the server.
    let res = send(&app, write(Method::POST, "/web/logout").header(header::COOKIE, &pair)).await;
    assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    let res = send(&app, get("/otel/api/v1/traces").header(header::COOKIE, &pair)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "the old cookie no longer works");
}

#[tokio::test]
async fn a_made_up_cookie_is_not_a_session() {
    let (app, seen) = with_login().await;
    let forged = "mallard_session=ZGV2LXRva2Vu";
    let body = text(send(&app, get("/web/session").header(header::COOKIE, forged)).await).await;
    let session: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(session["authenticated"], false);
    assert_eq!(session["server_url"], serde_json::Value::Null);
    let res = send(&app, get("/otel/api/v1/traces").header(header::COOKIE, forged)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn credentials_in_the_server_url_are_never_shown() {
    let cfg = Config::new("http://user:hunter2@127.0.0.1:9", None).unwrap();
    assert_eq!(cfg.display_url(), "http://127.0.0.1:9");
    let (url, _) = upstream().await;
    let with_creds = url.replace("http://", "http://user:hunter2@");
    let app = router(Config::new(&with_creds, Some(GOOD.into())).unwrap()).unwrap();
    let body = text(send(&app, get("/web/session")).await).await;
    assert!(!body.contains("hunter2"), "{body}");
}

#[tokio::test]
async fn other_sites_and_other_ports_may_not_even_read() {
    let (app, seen) = with_token().await;
    for site in ["cross-site", "same-site"] {
        let res = send(&app, get("/otel/api/v1/logs?q=secret").header("sec-fetch-site", site)).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{site}");
    }
    assert!(seen.lock().unwrap().is_empty());
    let res = send(&app, get("/otel/api/v1/logs").header("sec-fetch-site", "same-origin")).await;
    assert_eq!(res.status(), StatusCode::OK);
    // A link from anywhere still opens the app.
    let res = send(&app, get("/telemetry/logs").header("sec-fetch-site", "cross-site")).await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn off_this_machine_the_cookie_is_secure_and_prefixed() {
    let (app, _) = with_login().await;
    let body = || Body::from(serde_json::json!({ "token": GOOD }).to_string());
    let req = |host: &str| {
        Request::builder()
            .method(Method::POST)
            .uri("/web/login")
            .header(header::HOST, host)
            .header("x-mallard-web", "1")
            .header(header::CONTENT_TYPE, "application/json")
    };
    // No forwarding headers at all: a real address still means Secure.
    let res = app.clone().oneshot(req("mallard.example").body(body()).unwrap()).await.unwrap();
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(cookie.starts_with("__Host-mallard_session=") && cookie.contains("; Secure"), "{cookie}");
    // A proxy that sets its own Host, in any spelling of the protocol header.
    let proxied = req("127.0.0.1:8088").header("x-forwarded-host", "mallard.example").body(body()).unwrap();
    let res = app.clone().oneshot(proxied).await.unwrap();
    assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().contains("; Secure"));
    let proxied = req("127.0.0.1:8088").header("x-forwarded-proto", "https,http").body(body()).unwrap();
    let res = app.clone().oneshot(proxied).await.unwrap();
    assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().contains("; Secure"));
    // The prefixed cookie works.
    let res = app.clone().oneshot(req("mallard.example").body(body()).unwrap()).await.unwrap();
    let pair = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    let read = Request::builder()
        .uri("/otel/api/v1/traces")
        .header(header::HOST, "mallard.example")
        .header(header::COOKIE, pair);
    assert_eq!(send(&app, read).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn login_needs_the_write_header() {
    let (app, _) = with_login().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri("/web/login")
        .header(header::HOST, "mallard.example")
        .header(header::ORIGIN, "https://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::json!({ "token": GOOD }).to_string()))
        .unwrap();
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn sign_in_is_refused_when_the_process_holds_a_token() {
    let (app, _) = with_token().await;
    assert_eq!(app.oneshot(login_req(GOOD)).await.unwrap().status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unreachable_server_is_a_clear_error() {
    let app = router(Config::new("http://127.0.0.1:9", Some(GOOD.into())).unwrap()).unwrap();
    let res = send(&app, get("/sentry/v1/info")).await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let body = text(res).await;
    assert!(body.contains("unavailable") && !body.contains(GOOD), "{body}");
}

#[tokio::test]
async fn pages_have_security_headers_and_deep_links_get_the_app() {
    let (app, _) = with_token().await;
    let res = send(&app, get("/telemetry/traces/abc?w=1h")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    let csp = res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
    assert!(csp.contains("script-src 'self'") && csp.contains("frame-ancestors 'none'"), "{csp}");
    assert_eq!(res.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(send(&app, get("/assets/missing.js")).await.status(), StatusCode::NOT_FOUND);
    // A metric name has dots; reloading its page must still give the app.
    let res = send(&app, get("/telemetry/metrics/http.server.requests?w=1h")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    assert_eq!(text(send(&app, get("/healthz")).await).await, "ok");
}

#[test]
fn a_bad_server_url_is_refused_at_start() {
    assert!(Config::new("not a url", None).is_err());
    assert!(Config::new("ftp://x", None).is_err());
    assert_eq!(Config::new("http://x:8080/", None).unwrap().display_url(), "http://x:8080");
}
