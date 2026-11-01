//! In-process integration tests: the server runs through the library API,
//! events come from the official Rust `sentry` SDK and raw HTTP.

use mallard_duck::{DuckDbConfig, StorageConfig};
use mallard_sentry_server::{Artifacts, Config, Host, Server};
use serde_json::{Value, json};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const TOKEN: &str = "test-token";

struct TestServer {
    base: String,
    server: Server,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start() -> TestServer {
        Self::start_with(|_| {}).await
    }

    /// [`TestServer::start`] with settings changed by `change`. Everything
    /// lives in a temp dir: a DuckDB-file catalog, the lake and the sourcemaps.
    async fn start_with(change: impl FnOnce(&mut Config)) -> TestServer {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut cfg = Config::default();
        cfg.ingest.batch_window = Duration::from_millis(20);
        change(&mut cfg);
        let storage = StorageConfig {
            catalog: dir.path().join("catalog.ducklake").display().to_string(),
            data_path: format!("{}/", dir.path().join("lake").display()),
            ..Default::default()
        };
        std::fs::create_dir_all(&storage.data_path).unwrap();
        let conn = mallard_duck::open_connection(&storage, &DuckDbConfig::default()).unwrap();
        let host = Host {
            public_url: format!("http://{addr}"),
            api_tokens: vec![TOKEN.into()],
            artifacts: Some(Artifacts {
                url: dir.path().join("storage").display().to_string(),
                options: Default::default(),
            }),
            catalog_maintenance: Some(Duration::from_secs(3600)),
            maintenance_threads: 1,
        };
        let server = Server::with_connection(cfg, host, conn).await.unwrap();
        Self::serve(server, listener, dir).await
    }

    async fn serve(server: Server, listener: tokio::net::TcpListener, dir: tempfile::TempDir) -> TestServer {
        let addr = listener.local_addr().unwrap();
        let app = server.router();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        TestServer { base: format!("http://{addr}"), server, http: reqwest::Client::new(), _dir: dir }
    }

    async fn api(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(TOKEN);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    /// Stop the writer thread before the runtime and DuckDB go away.
    async fn stop(self) {
        self.server.shutdown().await;
    }

    async fn get(&self, path: &str) -> Value {
        let (s, v) = self.api(reqwest::Method::GET, path, None).await;
        assert_eq!(s, 200, "GET {path}: {v}");
        v
    }

    async fn create_project(&self, name: &str) -> Value {
        let (s, v) = self.api(reqwest::Method::POST, "/v1/projects", Some(json!({"name": name}))).await;
        assert_eq!(s, 201, "{v}");
        v
    }
}

fn sentry_hub(dsn: &str) -> (Arc<sentry::Client>, Arc<sentry::Hub>) {
    let mut opts = sentry::ClientOptions::default();
    opts.release = Some("api@2.0.0".into());
    opts.environment = Some("test".into());
    opts.dsn = Some(dsn.parse().unwrap());
    // apply_defaults installs the HTTP transport (and default integrations).
    let client = Arc::new(sentry::Client::from(sentry::apply_defaults(opts)));
    let hub = Arc::new(sentry::Hub::new(Some(client.clone()), Arc::new(sentry::Scope::default())));
    (client, hub)
}

#[derive(Debug)]
struct DbError(String);
impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for DbError {}

#[tokio::test(flavor = "multi_thread")]
async fn rust_sdk_events_are_grouped_and_queryable() {
    let t = TestServer::start().await;
    let project = t.create_project("Rust API").await;
    let dsn = project["dsn"].as_str().unwrap().to_string();
    assert!(dsn.starts_with("http://") && dsn.ends_with("/1"), "{dsn}");
    let info = t.get("/v1/info").await;
    assert!(info["catalog"].as_str().unwrap().contains("catalog.ducklake"), "{info}");

    let (client, hub) = sentry_hub(&dsn);
    hub.configure_scope(|s| {
        s.set_tag("tenant", "acme");
        s.set_user(Some(sentry::User { id: Some("u1".into()), ..Default::default() }));
    });
    for i in 0..3 {
        hub.capture_error(&DbError(format!("connection {i} refused")));
    }
    hub.capture_message("cache_miss storm", sentry::Level::Warning);
    assert!(client.flush(Some(Duration::from_secs(10))));

    let issues = t.get("/v1/projects/rust-api/issues").await;
    let list = issues["issues"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{issues}");
    let db =
        list.iter().find(|i| i["exception_type"].as_str().unwrap_or("").contains("DbError")).expect("DbError issue");
    assert_eq!(db["count"], 3, "the same error three times is one issue");
    assert_eq!(db["user_count"], 1);
    let warn = list.iter().find(|i| i["title"] == "cache_miss storm").unwrap();
    assert_eq!(warn["level"], "warning");

    let id = db["id"].as_str().unwrap();
    let detail = t.get(&format!("/v1/projects/rust-api/issues/{id}")).await;
    assert_eq!(detail["first_release"], "api@2.0.0");
    let tags: Vec<&str> = detail["tags"].as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap()).collect();
    for k in ["tenant", "environment", "release", "user", "os.name"] {
        assert!(tags.contains(&k), "missing tag {k} in {tags:?}");
    }
    let ev_id = detail["latest_event_id"].as_str().unwrap();
    let ev = t.get(&format!("/v1/projects/rust-api/events/{ev_id}?issue={id}")).await;
    assert_eq!(ev["event_id"], ev_id);
    assert_eq!(ev["payload"]["platform"], "native");
    assert!(ev["payload"]["sdk"]["name"].as_str().unwrap().starts_with("sentry.rust"));

    let events = t.get(&format!("/v1/projects/rust-api/events?issue={id}")).await;
    assert_eq!(events["events"].as_array().unwrap().len(), 3);

    // Search treats `_` and `%` literally.
    let found = t.get("/v1/projects/rust-api/issues?query=cache_miss").await;
    assert_eq!(found["issues"].as_array().unwrap().len(), 1, "{found}");
    let none = t.get("/v1/projects/rust-api/issues?query=cache%25miss").await;
    assert!(none["issues"].as_array().unwrap().is_empty(), "{none}");

    // Cursor paging returns every event exactly once.
    let mut seen = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            Some(c) => format!("/v1/projects/rust-api/events?limit=1&before={c}"),
            None => "/v1/projects/rust-api/events?limit=1".to_string(),
        };
        let page = t.get(&url).await;
        for e in page["events"].as_array().unwrap() {
            assert!(seen.insert(e["event_id"].as_str().unwrap().to_string()), "duplicate event in paging");
        }
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), 4);

    let stats = t.get("/v1/projects/rust-api/stats?since=1h").await;
    assert_eq!(stats["total_events"], 4);
    assert_eq!(stats["unique_issues"], 2);
    assert_eq!(stats["new_issues"], 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_regression_ignore_and_maintenance() {
    let t = TestServer::start().await;
    let project = t.create_project("web").await;
    let key = project["public_key"].as_str().unwrap();
    let send = |msg: &'static str| {
        let url = format!("{}/api/1/store/?sentry_key={key}&sentry_version=7", t.base);
        let http = t.http.clone();
        async move {
            let r = http.post(url).body(json!({"message": msg, "level": "error"}).to_string()).send().await.unwrap();
            assert_eq!(r.status(), 200);
            r.json::<Value>().await.unwrap()
        }
    };
    send("boom").await;
    let id = t.get("/v1/projects/web/issues").await["issues"][0]["id"].as_str().unwrap().to_string();

    let (s, v) = t
        .api(reqwest::Method::PATCH, &format!("/v1/projects/web/issues/{id}"), Some(json!({"status": "resolved"})))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "resolved");
    assert!(t.get("/v1/projects/web/issues").await["issues"].as_array().unwrap().is_empty());
    assert_eq!(t.get("/v1/projects/web/issues?status=resolved").await["issues"].as_array().unwrap().len(), 1);

    // A new event reopens a resolved issue.
    send("boom").await;
    let issue = t.get(&format!("/v1/projects/web/issues/{id}")).await;
    assert_eq!(issue["status"], "unresolved");
    assert_eq!(issue["activity"][0]["reason"], "regression");
    assert_eq!(issue["count"], 2);

    // Ignored stays ignored.
    t.api(reqwest::Method::PATCH, &format!("/v1/projects/web/issues/{id}"), Some(json!({"status": "ignored"}))).await;
    send("boom").await;
    assert_eq!(t.get(&format!("/v1/projects/web/issues/{id}")).await["status"], "ignored");

    // Flushing inlined rows to Parquet and compacting keeps everything queryable.
    let (s, _) = t.api(reqwest::Method::POST, "/v1/maintenance", None).await;
    assert_eq!(s, 204);
    assert_eq!(t.get(&format!("/v1/projects/web/issues/{id}")).await["count"], 3);
    let parquet = walk(t._dir.path()).into_iter().filter(|p| p.ends_with(".parquet")).count();
    assert!(parquet > 0, "maintenance should write parquet files");
    t.stop().await;
}

/// The endpoints the agent CLI needs (spec S1–S4, S7): reason + idempotent
/// status changes, new/first-release filters, event windows, event lookup
/// across projects, info for setup.
#[tokio::test(flavor = "multi_thread")]
async fn agent_endpoints() {
    let t = TestServer::start().await;
    let project = t.create_project("web").await;
    let key = project["public_key"].as_str().unwrap();
    let send = |msg: &'static str, release: &'static str| {
        let url = format!("{}/api/1/store/?sentry_key={key}&sentry_version=7", t.base);
        let http = t.http.clone();
        async move {
            let body = json!({"message": msg, "level": "error", "release": release});
            let r = http.post(url).body(body.to_string()).send().await.unwrap();
            assert_eq!(r.status(), 200);
            r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string()
        }
    };
    send("old", "web@1").await;
    let event_id = send("new", "web@2").await;
    let issues = t.get("/v1/projects/web/issues").await;
    let id_of = |title: &str| {
        issues["issues"].as_array().unwrap().iter().find(|i| i["title"] == title).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let new_id = id_of("new");

    // S2: first seen in a release, first seen in the window.
    let v = t.get("/v1/projects/web/issues?first_release=web@2").await;
    let titles: Vec<&str> = v["issues"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["new"], "{v}");
    assert_eq!(t.get("/v1/projects/web/issues?new=true&since=1h").await["issues"].as_array().unwrap().len(), 2);
    let hours_ago = |h: i64| urlencode(&(chrono::Utc::now() - chrono::Duration::hours(h)).to_rfc3339());
    let (two, one) = (hours_ago(2), hours_ago(1));
    // Both were first seen in the last day; neither between 2h and 1h ago.
    let v = t.get(&format!("/v1/projects/web/issues?new=true&since=1d&to={}", hours_ago(-1))).await;
    assert_eq!(v["issues"].as_array().unwrap().len(), 2, "{v}");
    let v = t.get(&format!("/v1/projects/web/issues?new=true&from={two}&to={one}")).await;
    assert!(v["issues"].as_array().unwrap().is_empty(), "{v}");

    // S3: a reason, and the same status twice is not a change.
    let path = format!("/v1/projects/web/issues/{new_id}");
    let patch = |body: Value| t.api(reqwest::Method::PATCH, &path, Some(body));
    let (s, v) = patch(json!({"status": "resolved", "reason": "fixed in #123"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!((v["status"].as_str(), v["changed"].as_bool()), (Some("resolved"), Some(true)), "{v}");
    assert_eq!(v["activity"][0]["reason"], "fixed in #123");
    let (_, v) = patch(json!({"status": "resolved", "reason": "again"})).await;
    assert_eq!(v["changed"], false, "{v}");
    assert_eq!(v["activity"].as_array().unwrap().len(), 1, "nothing written: {v}");
    let (s, v) = patch(json!({"status": "ignored", "reason": "x".repeat(501)})).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{v}");
    let detail = t.get(&format!("/v1/projects/web/issues/{new_id}")).await;
    assert!(detail.get("changed").is_none(), "only PATCH answers have `changed`: {detail}");

    // S1: a window on the event list (none by default).
    assert_eq!(t.get("/v1/projects/web/events?since=1h").await["events"].as_array().unwrap().len(), 2);
    let v = t.get(&format!("/v1/projects/web/events?from={two}&to={one}")).await;
    assert!(v["events"].as_array().unwrap().is_empty(), "{v}");

    // S7: an event by id without its project.
    let e = t.get(&format!("/v1/events/{event_id}")).await;
    assert_eq!((e["event_id"].as_str(), e["project_id"].as_u64()), (Some(event_id.as_str()), Some(1)));
    let (s, v) = t.api(reqwest::Method::GET, &format!("/v1/events/{}", "0".repeat(32)), None).await;
    assert_eq!((s, v["error"].as_str()), (404, Some("not_found")), "{v}");

    // S4: what `mallard setup` and `status` need.
    let info = t.get("/v1/info").await;
    assert_eq!(info["public_url"].as_str(), Some(t.base.as_str()), "{info}");
    assert!(info["retention_secs"].as_u64().is_some() && info["max_window_secs"].as_u64().unwrap() > 0, "{info}");
    t.stop().await;
}

/// Store one event through `/api/1/store/` (answered after it is written).
async fn store(t: &TestServer, key: &str, event: Value) {
    let url = format!("{}/api/1/store/?sentry_key={key}&sentry_version=7", t.base);
    let r = t.http.post(url).body(event.to_string()).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

fn ids(v: &Value, list: &str, key: &str) -> Vec<String> {
    v[list].as_array().unwrap_or_else(|| panic!("{v}")).iter().map(|i| i[key].as_str().unwrap().to_string()).collect()
}

/// Agent CLI P1, sentry side: tag and event filters (S9), issue cursors
/// (S10), releases (S14); SQL is off by default (S15).
#[tokio::test(flavor = "multi_thread")]
async fn filters_cursors_and_releases() {
    let t = TestServer::start().await;
    let project = t.create_project("web").await;
    let key = project["public_key"].as_str().unwrap();
    let ev = |msg: &str, release: Option<&str>, env: &str, tags: Value| json!({"message": msg, "level": "error", "release": release, "environment": env, "tags": tags});
    store(&t, key, ev("alpha", Some("web@1"), "prod", json!({"url": "/checkout", "user": "u-1"}))).await;
    store(&t, key, ev("beta", Some("web@2"), "staging", json!({"url": "/cart"}))).await;
    store(&t, key, ev("alpha", Some("web@2"), "prod", json!({"url": "/checkout"}))).await;
    for i in 0..5 {
        store(&t, key, ev(&format!("m{i}"), None, "prod", json!({}))).await;
    }

    // S9: tags on issues (repeat = all must match); counts follow the filter.
    let v = t.get("/v1/projects/web/issues?tag=url:/checkout").await;
    assert_eq!((ids(&v, "issues", "title"), &v["issues"][0]["count"]), (vec!["alpha".to_string()], &json!(2)), "{v}");
    let v = t.get("/v1/projects/web/issues?tag=url:/checkout&tag=user:u-1").await;
    assert_eq!(v["issues"][0]["count"], 1, "{v}");
    let v = t.get("/v1/projects/web/issues?tag=release:web@2&sort=count").await;
    assert_eq!(ids(&v, "issues", "title").len(), 2, "derived tags work too: {v}");
    assert!(t.get("/v1/projects/web/issues?tag=url:/nope").await["issues"].as_array().unwrap().is_empty());
    let (s, v) = t.api(reqwest::Method::GET, "/v1/projects/web/issues?tag=nocolon", None).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{v}");
    let many = (0..11).map(|i| format!("tag=k{i}:v")).collect::<Vec<_>>().join("&");
    assert_eq!(t.api(reqwest::Method::GET, &format!("/v1/projects/web/issues?{many}"), None).await.0, 400);

    // S9: event filters.
    let count = |path: &str| {
        let path = path.to_string();
        let t = &t;
        async move { t.get(&path).await["events"].as_array().unwrap().len() }
    };
    assert_eq!(count("/v1/projects/web/events?tag=url:/checkout").await, 2);
    assert_eq!(count("/v1/projects/web/events?since=1h&tag=url:/cart").await, 1);
    assert_eq!(count("/v1/projects/web/events?release=web@2").await, 2);
    assert_eq!(count("/v1/projects/web/events?environment=staging").await, 1);
    assert_eq!(count("/v1/projects/web/events?environment=prod&release=web@2").await, 1);
    assert_eq!(count("/v1/projects/web/events?query=ALP").await, 2);
    assert_eq!(count("/v1/projects/web/events?query=m_").await, 0, "_ is literal");

    // S10: pages of issues, each issue once, for every sort.
    for sort in ["last_seen", "first_seen", "count", "users"] {
        let mut seen = std::collections::HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            let mut path = format!("/v1/projects/web/issues?limit=3&sort={sort}");
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={}", urlencode(c)));
            }
            let page = t.get(&path).await;
            for id in ids(&page, "issues", "id") {
                assert!(seen.insert(id), "an issue on two pages ({sort})");
            }
            cursor = page["next_cursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), 7, "{sort}");
    }
    let first = t.get("/v1/projects/web/issues?limit=1&sort=count").await;
    let c = urlencode(first["next_cursor"].as_str().unwrap());
    let (s, v) = t.api(reqwest::Method::GET, &format!("/v1/projects/web/issues?cursor={c}"), None).await;
    assert_eq!(s, 400, "a cursor is for one sort: {v}");
    let (s, _) = t.api(reqwest::Method::GET, "/v1/projects/web/issues?cursor=nope", None).await;
    assert_eq!(s, 400);
    let all = t.get("/v1/projects/web/issues").await;
    assert!(all["next_cursor"].is_null(), "no cursor on the last page: {all}");

    // S14: releases from events, newest first.
    let v = t.get("/v1/projects/web/releases?since=1h").await;
    assert_eq!(ids(&v, "releases", "version"), ["web@2", "web@1"], "{v}");
    let r2 = &v["releases"][0];
    assert_eq!((&r2["events"], &r2["issues"], &r2["new_issues"]), (&json!(2), &json!(2), &json!(1)), "{v}");
    assert!(r2["first_seen"].as_str() <= r2["last_seen"].as_str());
    let r1 = &v["releases"][1];
    assert_eq!((&r1["events"], &r1["issues"], &r1["new_issues"]), (&json!(1), &json!(1), &json!(1)), "{v}");
    assert_eq!(t.get("/v1/projects/web/releases?limit=1").await["releases"].as_array().unwrap().len(), 1);
    let hour_ago = urlencode(&(chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339());
    let v = t.get(&format!("/v1/projects/web/releases?to={hour_ago}")).await;
    assert!(v["releases"].as_array().unwrap().is_empty(), "{v}");

    // S15: off unless enabled.
    let (s, v) = t.api(reqwest::Method::POST, "/v1/sql", Some(json!({"query": "SELECT 1"}))).await;
    assert_eq!((s, v["error"].as_str()), (403, Some("disabled")), "{v}");
    assert!(v["message"].as_str().unwrap().contains("enable_sql"), "{v}");
    t.stop().await;
}

/// S15: read-only SQL over the error tables when enabled.
#[tokio::test(flavor = "multi_thread")]
async fn sql_over_error_tables() {
    let t = TestServer::start_with(|c| c.query.enable_sql = true).await;
    let project = t.create_project("web").await;
    let key = project["public_key"].as_str().unwrap();
    for (msg, url) in [("a", "/x"), ("a", "/x"), ("b", "/y")] {
        store(&t, key, json!({"message": msg, "level": "error", "tags": {"url": url}})).await;
    }
    let sql = |query: &str, limit: Option<u32>| {
        t.api(reqwest::Method::POST, "/v1/sql", Some(json!({"query": query, "limit": limit})))
    };
    let (s, v) = sql("SELECT tags['url'] AS url, count(*) AS n FROM events GROUP BY 1 ORDER BY 2 DESC;", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["columns"], json!([{"name": "url", "type": "VARCHAR"}, {"name": "n", "type": "BIGINT"}]));
    assert_eq!(v["rows"], json!([["/x", 2], ["/y", 1]]));
    assert_eq!(v["truncated"], false);
    let (s, v) =
        sql("SELECT count(*) FROM lake.sentry.issues i JOIN issue_activity a USING (project_id, group_hash)", None)
            .await;
    assert_eq!((s, &v["rows"]), (200, &json!([[0]])), "{v}");
    let (s, v) = sql("SELECT title FROM issues ORDER BY title", Some(1)).await;
    assert_eq!((s, &v["rows"], &v["truncated"]), (200, &json!([["a"]]), &json!(true)), "{v}");
    for bad in [
        "SELECT * FROM projects",
        "SELECT * FROM lake.otel.spans",
        "DELETE FROM events",
        "SELECT * FROM read_csv('/etc/passwd')",
        "SELECT getenv('HOME')",
        "SELECT nope FROM events",
    ] {
        let (s, v) = sql(bad, None).await;
        assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{bad}: {v}");
    }
    assert_eq!(sql("SELECT 1", Some(0)).await.0, 400);
    t.stop().await;
}

fn urlencode(s: &str) -> String {
    s.replace('+', "%2B").replace(':', "%3A")
}

fn walk(p: &std::path::Path) -> Vec<String> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path.display().to_string());
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn ingest_auth_compression_cors_and_dedupe() {
    let t = TestServer::start().await;
    let project = t.create_project("web").await;
    let key = project["public_key"].as_str().unwrap();
    let event_id = "fc6d8c0c43fc4630ad850ee518f1b9d0";
    let envelope = format!(
        "{{\"event_id\":\"{event_id}\",\"dsn\":\"http://{key}@x/1\"}}\n{{\"type\":\"event\"}}\n{{\"message\":\"gz\",\"exception\":{{\"values\":[{{\"type\":\"E\",\"value\":\"v\"}}]}}}}\n{{\"type\":\"transaction\"}}\n{{}}\n"
    );
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(envelope.as_bytes()).unwrap();
    let body = gz.finish().unwrap();
    let url = format!("{}/api/1/envelope/", t.base);

    // Key only in the envelope header (as tunnels do), gzip body, text/plain (browser SDK).
    let r = t
        .http
        .post(&url)
        .header("content-encoding", "gzip")
        .header("content-type", "text/plain;charset=UTF-8")
        .header("origin", "https://app.example.com")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["access-control-allow-origin"], "https://app.example.com");
    assert_eq!(r.json::<Value>().await.unwrap()["id"], event_id);

    // Same event again (SDK retry): accepted, not stored twice.
    let r = t.http.post(&url).header("content-encoding", "gzip").body(body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let issues = t.get("/v1/projects/web/issues").await;
    assert_eq!(issues["issues"][0]["count"], 1);

    // Preflight from a browser.
    let r = t
        .http
        .request(reqwest::Method::OPTIONS, &url)
        .header("origin", "https://app.example.com")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type,sentry-trace,baggage")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    // Wrong key, unknown project, garbage.
    let bad = "{}\n{\"type\":\"event\"}\n{}\n";
    let r = t
        .http
        .post(&url)
        .header("x-sentry-auth", "Sentry sentry_key=nope, sentry_version=7")
        .body(bad)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.headers().contains_key("x-sentry-error"));
    let r = t.http.post(format!("{}/api/99/envelope/?sentry_key={key}", t.base)).body(bad).send().await.unwrap();
    assert_eq!(r.status(), 404);
    let r = t.http.post(&url).body(bad).send().await.unwrap();
    assert_eq!(r.status(), 401, "no key at all");
    let r = t.http.post(format!("{url}?sentry_key={key}")).body("not json").send().await.unwrap();
    assert_eq!(r.status(), 400);

    // A cursor that does not parse is an error, not the first page again.
    let (s, v) = t.api(reqwest::Method::GET, "/v1/projects/web/events?before=garbage", None).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{v}");
    // Bad query parameters get the JSON error body too, like the OTel API.
    let (s, v) = t.api(reqwest::Method::GET, "/v1/projects/web/events?limit=many", None).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{v}");
    // Windows: since, from/to and step, bounded by query.max_window.
    let (s, v) = t.api(reqwest::Method::GET, "/v1/projects/web/issues?since=400d", None).await;
    assert_eq!(s, 400, "{v}");
    assert!(v["message"].as_str().unwrap().contains("max_window"), "{v}");
    let to = chrono::Utc::now() + chrono::Duration::minutes(1);
    let from = to - chrono::Duration::hours(2);
    let range = format!("from={}&to={}", from.format("%Y-%m-%dT%H:%M:%SZ"), to.format("%Y-%m-%dT%H:%M:%SZ"));
    let stats = t.get(&format!("/v1/projects/web/stats?{range}&step=10m")).await;
    assert_eq!(stats["interval_seconds"], 600, "{stats}");
    assert_eq!(stats["buckets"].as_array().unwrap().len(), 12, "{stats}");
    let issues = t.get(&format!("/v1/projects/web/issues?{range}")).await;
    assert!(!issues["issues"].as_array().unwrap().is_empty(), "{issues}");
    let old = format!(
        "from={}&to={}",
        (from - chrono::Duration::days(2)).to_rfc3339(),
        (to - chrono::Duration::days(2)).to_rfc3339()
    );
    let none = t.get(&format!("/v1/projects/web/issues?{}", old.replace('+', "%2B"))).await;
    assert!(none["issues"].as_array().unwrap().is_empty(), "`to` bounds the window: {none}");

    // Management API requires the token.
    let r = t.http.get(format!("{}/v1/projects", t.base)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = t.http.get(format!("{}/v1/projects", t.base)).bearer_auth("wrong").send().await.unwrap();
    assert_eq!(r.status(), 401);
    // Meta routes are public.
    assert_eq!(t.http.get(format!("{}/openapi.json", t.base)).send().await.unwrap().status(), 200);
    assert_eq!(t.http.get(format!("{}/readyz", t.base)).send().await.unwrap().status(), 200);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_retries_are_stored_once() {
    let t = TestServer::start().await;
    let project = t.create_project("dup").await;
    let key = project["public_key"].as_str().unwrap();
    let url = format!("{}/api/1/envelope/?sentry_key={key}", t.base);
    let body = "{\"event_id\":\"0f6d8c0c43fc4630ad850ee518f1b9d1\"}\n{\"type\":\"event\"}\n{\"message\":\"dup\"}\n";
    let sends = (0..8).map(|_| t.http.post(&url).body(body).send());
    for r in futures::future::join_all(sends).await {
        assert_eq!(r.unwrap().status(), 200);
    }
    let events = t.get("/v1/projects/dup/events").await;
    assert_eq!(events["events"].as_array().unwrap().len(), 1, "{events}");
    t.stop().await;
}

/// Slug, content type, envelope's `sdk` header and payload of a `span` item.
type SeenSpans = (String, Option<String>, Option<Value>, Vec<u8>);

#[derive(Default)]
struct Hooks {
    transactions: std::sync::Mutex<Vec<mallard_sentry_server::Transaction>>,
    spans: std::sync::Mutex<Vec<SeenSpans>>,
    events: std::sync::Mutex<Vec<(u32, uuid::Uuid, u64)>>,
}
impl mallard_sentry_server::TransactionSink for Hooks {
    fn transaction(&self, t: mallard_sentry_server::Transaction) {
        self.transactions.lock().unwrap().push(t);
    }
}
impl mallard_sentry_server::SpanSink for Hooks {
    fn spans(&self, s: mallard_sentry_server::Spans<'_>) {
        self.spans.lock().unwrap().push((
            s.project_slug,
            s.content_type.map(str::to_string),
            s.envelope_headers.get("sdk").cloned(),
            s.payload.to_vec(),
        ));
    }
}
impl mallard_sentry_server::EventObserver for Hooks {
    fn event_stored(&self, project_id: u32, trace_id: uuid::Uuid, span_id: u64) {
        self.events.lock().unwrap().push((project_id, trace_id, span_id));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn host_hooks_and_in_process_events() {
    let t = TestServer::start().await;
    let project = t.create_project("hooked").await;
    let key = project["public_key"].as_str().unwrap();
    let url = format!("{}/api/1/envelope/?sentry_key={key}", t.base);
    let hooks = Arc::new(Hooks::default());
    t.server.set_transaction_sink(hooks.clone()).unwrap();
    t.server.set_span_sink(hooks.clone()).unwrap();
    assert!(t.server.set_span_sink(hooks.clone()).is_err(), "set once");
    t.server.set_event_observer(hooks.clone()).unwrap();
    assert!(t.server.set_event_observer(hooks.clone()).is_err(), "set once");

    let (trace, span) = ("0af7651916cd43dd8448eb211c80319c", "b7ad6b7169203331");
    let body = format!(
        "{{}}\n{{\"type\":\"transaction\"}}\n{{\"type\":\"transaction\",\"transaction\":\"GET /\"}}\n\
         {{\"type\":\"event\"}}\n{{\"message\":\"m\",\"contexts\":{{\"trace\":{{\"trace_id\":\"{trace}\",\"span_id\":\"{span}\"}}}}}}\n"
    );
    let r = t.http.post(&url).body(body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let tx = hooks.transactions.lock().unwrap().clone();
    assert_eq!(tx.len(), 1);
    assert_eq!((tx[0].project_id, tx[0].project_slug.as_str()), (1, "hooked"));
    assert!(std::str::from_utf8(&tx[0].payload).unwrap().contains("GET /"));
    let seen = hooks.events.lock().unwrap().clone();
    assert_eq!(seen, [(1, uuid::Uuid::parse_str(trace).unwrap(), 0xb7ad6b7169203331)]);

    // Span items go to the sink with their content type and envelope headers.
    let body = "{\"sdk\":{\"name\":\"s\"}}\n{\"type\":\"span\",\"content_type\":\"a/b\"}\n{\"items\":[]}\n";
    assert_eq!(t.http.post(&url).body(body).send().await.unwrap().status(), 200);
    let spans = hooks.spans.lock().unwrap().clone();
    assert_eq!(
        spans,
        [("hooked".to_string(), Some("a/b".to_string()), Some(json!({"name": "s"})), b"{\"items\":[]}".to_vec())]
    );

    // In-process events: grouped like any other, deduped by id, not observed.
    let ev = json!({"event_id": "1f6d8c0c43fc4630ad850ee518f1b9d2", "platform": "python",
        "exception": {"values": [{"type": "ValueError", "value": "bad"}]},
        "contexts": {"trace": {"trace_id": trace, "span_id": "00000000000000aa"}}});
    for _ in 0..2 {
        assert_eq!(t.server.ingest_event(1, ev.clone()).await.unwrap(), "1f6d8c0c43fc4630ad850ee518f1b9d2");
    }
    assert_eq!(t.server.ingest_event(9, ev).await, Err(mallard_sentry_server::IngestError::UnknownProject));
    assert_eq!(hooks.events.lock().unwrap().len(), 1);
    let issues = t.get("/v1/projects/hooked/issues?query=ValueError").await;
    assert_eq!(issues["issues"][0]["count"], 1, "{issues}");

    // Projects for in-process callers.
    assert_eq!(t.server.project_by_slug("hooked"), Some(1));
    let id = t.server.create_project("Checkout", "checkout", Some("python")).await.unwrap();
    assert_eq!(t.server.create_project("Checkout", "checkout", None).await.unwrap(), id, "existing slug");
    assert!(t.server.create_project("x", "42", None).await.is_err(), "numeric slug");
    t.stop().await;
}

/// Build an artifact bundle like sentry-cli does (SYSB header + zip).
fn bundle(debug_id: &str, minified: &str, map: &str) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        let manifest = json!({
            "files": {
                "files/_/_/assets/app.js": {"type": "minified_source", "url": "~/assets/app.js",
                    "headers": {"debug-id": debug_id, "sourcemap": "app.js.map"}},
                "files/_/_/assets/app.js.map": {"type": "source_map", "url": "~/assets/app.js.map",
                    "headers": {"debug-id": debug_id}}
            },
            "org": "default"
        });
        z.start_file("files/_/_/assets/app.js", o).unwrap();
        z.write_all(minified.as_bytes()).unwrap();
        z.start_file("files/_/_/assets/app.js.map", o).unwrap();
        z.write_all(map.as_bytes()).unwrap();
        z.start_file("manifest.json", o).unwrap();
        z.write_all(manifest.to_string().as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let mut out = b"SYSB\x02\x00\x00\x00".to_vec();
    out.extend(buf.into_inner());
    out
}

fn sha1(b: &[u8]) -> String {
    use sha1::Digest;
    hex::encode(sha1::Sha1::digest(b))
}

#[tokio::test(flavor = "multi_thread")]
async fn sourcemap_upload_protocol_and_symbolication() {
    let t = TestServer::start().await;
    t.create_project("web").await;
    let key = t.get("/v1/projects/web").await["public_key"].as_str().unwrap().to_string();
    let debug_id = "c941d872-af1f-4f0c-a7ff-ad3d295fe153";
    // Minified `function n(e){return e.name.toUpperCase()}` from src/app.ts.
    let minified = "function n(e){return e.name.toUpperCase()}\n//# debugId=c941d872-af1f-4f0c-a7ff-ad3d295fe153\n//# sourceMappingURL=app.js.map";
    let map = json!({
        "version": 3, "file": "app.js", "sources": ["../src/app.ts"],
        "sourcesContent": ["// header\nexport function handleClick(user) {\n  return user.name.toUpperCase();\n}\n"],
        "names": ["handleClick", "user"],
        "mappings": "AACA,SAAgBA,EAAYC,GAC1B,OAAOA",
        "debug_id": debug_id
    })
    .to_string();
    let data = bundle(debug_id, minified, &map);
    // Two chunks to exercise ordering.
    let mid = data.len() / 2;
    let chunks = [&data[..mid], &data[mid..]];
    let checksums: Vec<String> = chunks.iter().map(|c| sha1(c)).collect();
    let checksum = sha1(&data);

    let opts = t.get("/api/0/organizations/default/chunk-upload/").await;
    assert_eq!(opts["hashAlgorithm"], "sha1");
    assert_eq!(opts["url"], format!("{}/api/0/organizations/default/chunk-upload/", t.base));
    assert!(opts["accept"].as_array().unwrap().contains(&json!("artifact_bundles_v2")));

    let assemble = json!({"checksum": checksum, "chunks": checksums, "projects": ["web"]});
    let (s, v) = t
        .api(reqwest::Method::POST, "/api/0/organizations/default/artifactbundle/assemble/", Some(assemble.clone()))
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["state"], "not_found");
    assert_eq!(v["missingChunks"].as_array().unwrap().len(), 2);

    // Upload chunks: one gzip'd, one plain, in one multipart request.
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(chunks[0]).unwrap();
    let form = reqwest::multipart::Form::new()
        .part("file_gzip", reqwest::multipart::Part::bytes(gz.finish().unwrap()).file_name(checksums[0].clone()))
        .part("file", reqwest::multipart::Part::bytes(chunks[1].to_vec()).file_name(checksums[1].clone()));
    let r = t.http.post(opts["url"].as_str().unwrap()).bearer_auth(TOKEN).multipart(form).send().await.unwrap();
    assert_eq!(r.status(), 200);

    // A chunk whose name does not match its content is rejected.
    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(b"evil".to_vec()).file_name(checksums[0].clone()));
    let r = t.http.post(opts["url"].as_str().unwrap()).bearer_auth(TOKEN).multipart(form).send().await.unwrap();
    assert_eq!(r.status(), 400);

    let (_, v) = t
        .api(reqwest::Method::POST, "/api/0/organizations/default/artifactbundle/assemble/", Some(assemble.clone()))
        .await;
    assert_eq!(v["state"], "ok", "{v}");
    assert_eq!(v["missingChunks"], json!([]));
    // Idempotent poll.
    let (_, v) =
        t.api(reqwest::Method::POST, "/api/0/organizations/default/artifactbundle/assemble/", Some(assemble)).await;
    assert_eq!(v["state"], "ok");

    let bundles = t.get("/v1/projects/web/sourcemaps").await;
    assert_eq!(bundles[0]["id"], checksum);
    assert_eq!(bundles[0]["debug_ids"][0], debug_id);

    // The same bundle uploaded for a second project is indexed there too.
    t.create_project("api").await;
    let assemble_api = json!({"checksum": checksum, "chunks": checksums, "projects": ["api"]});
    let (_, v) =
        t.api(reqwest::Method::POST, "/api/0/organizations/default/artifactbundle/assemble/", Some(assemble_api)).await;
    // Chunks were deleted after the first assembly, so the CLI is asked for them again.
    assert_eq!(v["state"], "not_found", "{v}");
    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(chunks[0].to_vec()).file_name(checksums[0].clone()))
        .part("file", reqwest::multipart::Part::bytes(chunks[1].to_vec()).file_name(checksums[1].clone()));
    let r = t.http.post(opts["url"].as_str().unwrap()).bearer_auth(TOKEN).multipart(form).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let assemble_api = json!({"checksum": checksum, "chunks": checksums, "projects": ["api"]});
    let (_, v) =
        t.api(reqwest::Method::POST, "/api/0/organizations/default/artifactbundle/assemble/", Some(assemble_api)).await;
    assert_eq!(v["state"], "ok", "{v}");
    assert_eq!(t.get("/v1/projects/api/sourcemaps").await[0]["id"], checksum);
    assert_eq!(t.get("/v1/projects/web/sourcemaps").await.as_array().unwrap().len(), 1);

    // A browser error pointing at the minified file gets symbolicated.
    let event = json!({
        "platform": "javascript",
        "debug_meta": {"images": [{"type": "sourcemap", "code_file": "https://cdn.test/assets/app.js", "debug_id": debug_id}]},
        "exception": {"values": [{"type": "TypeError", "value": "Cannot read properties of undefined (reading 'toUpperCase')",
            "stacktrace": {"frames": [{"abs_path": "https://cdn.test/assets/app.js", "function": "n", "lineno": 1, "colno": 22, "in_app": true}]}}]}
    });
    let r =
        t.http.post(format!("{}/api/1/store/?sentry_key={key}", t.base)).body(event.to_string()).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let id = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    let ev = t.get(&format!("/v1/projects/web/events/{id}")).await;
    let exc = &ev["payload"]["exception"]["values"][0];
    let f = &exc["stacktrace"]["frames"][0];
    assert_eq!(f["function"], "handleClick", "{f}");
    assert_eq!(f["lineno"], 3);
    assert_eq!(f["filename"], "/src/app.ts");
    assert_eq!(f["context_line"], "  return user.name.toUpperCase();");
    assert_eq!(f["data"]["resolved_with"], "debug-id");
    assert_eq!(exc["raw_stacktrace"]["frames"][0]["function"], "n");
    assert_eq!(ev["culprit"], "handleClick(/src/app.ts)");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn embedded_with_shared_connection_and_clean_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    // The embedding app owns the connection and the catalog...
    let conn = mallard_duck::duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "INSTALL ducklake; LOAD ducklake; ATTACH 'ducklake:{}' AS lake (DATA_PATH '{}/');
         CREATE SCHEMA lake.otel; CREATE TABLE lake.otel.spans (trace_id UUID, name VARCHAR);",
        dir.path().join("cat.ducklake").display(),
        dir.path().join("data").display()
    ))
    .unwrap();
    let probe = conn.try_clone().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut cfg = Config::default();
    cfg.ingest.batch_window = Duration::from_millis(20);
    let host = Host {
        public_url: format!("http://{addr}/sentry"),
        api_tokens: vec![TOKEN.into()],
        artifacts: None,
        catalog_maintenance: None,
        maintenance_threads: 0,
    };
    let server = Server::with_connection(cfg, host, conn).await.unwrap();
    // ...and mounts us below a prefix.
    let app = axum::Router::new().nest("/sentry", server.router());
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });
    let base = format!("http://{addr}/sentry");
    let http = reqwest::Client::new();
    let p: Value = http
        .post(format!("{base}/v1/projects"))
        .bearer_auth(TOKEN)
        .json(&json!({"name": "web"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dsn = p["dsn"].as_str().unwrap().to_string();
    assert_eq!(dsn, format!("http://{}@{addr}/sentry/1", p["public_key"].as_str().unwrap()));

    let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    let (client, hub) = sentry_hub(&dsn);
    hub.configure_scope(|s| {
        s.set_context(
            "trace",
            sentry::protocol::Context::Trace(Box::new(sentry::protocol::TraceContext {
                trace_id: trace.parse().unwrap(),
                span_id: "00f067aa0ba902b7".parse().unwrap(),
                ..Default::default()
            })),
        )
    });
    hub.capture_message("joined", sentry::Level::Error);
    assert!(client.flush(Some(Duration::from_secs(10))));

    // Errors join traces by UUID without casts.
    probe.execute("INSERT INTO lake.otel.spans VALUES (?::UUID, 'GET /checkout')", [trace]).unwrap();
    let (name, title): (String, String) = probe
        .query_row(
            "SELECT s.name, e.title FROM lake.sentry.events e JOIN lake.otel.spans s USING (trace_id)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), title.as_str()), ("GET /checkout", "joined"));
    let list: Value = http
        .get(format!("{base}/v1/events?trace_id={trace}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["events"][0]["span_id"], "00f067aa0ba902b7");

    server.shutdown().await;
    server.shutdown().await; // idempotent
    let r = http
        .post(format!("{base}/api/1/store/?sentry_key={}", p["public_key"].as_str().unwrap()))
        .body("{\"message\":\"late\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503, "writes after shutdown are refused");
}

/// Trace lists mark traces with errors: one request for many trace ids.
#[tokio::test(flavor = "multi_thread")]
async fn events_by_many_traces() {
    let t = TestServer::start().await;
    let project = t.create_project("web").await;
    let key = project["public_key"].as_str().unwrap();
    let (a, b, none) =
        ("0af7651916cd43dd8448eb211c80319c", "4bf92f3577b34da6a3ce929d0e0e4736", "11111111111111111111111111111111");
    for (msg, trace, span) in
        [("boom", a, "00f067aa0ba902b7"), ("boom", a, "00f067aa0ba902b8"), ("bang", b, "0000000000000001")]
    {
        let body =
            json!({"message": msg, "level": "error", "contexts": {"trace": {"trace_id": trace, "span_id": span}}});
        let url = format!("{}/api/1/store/?sentry_key={key}&sentry_version=7", t.base);
        assert_eq!(t.http.post(url).body(body.to_string()).send().await.unwrap().status(), 200);
    }

    let v = t.get(&format!("/v1/events/by-trace?trace_id={a},{b},{none},{a}&since=1h")).await;
    let traces = v["traces"].as_object().unwrap();
    assert_eq!(traces.len(), 2, "traces without events are left out: {v}");
    assert_eq!(v["traces"][a]["count"], 2);
    let spans: Vec<&str> =
        v["traces"][a]["events"].as_array().unwrap().iter().map(|e| e["span_id"].as_str().unwrap()).collect();
    assert_eq!(spans.len(), 2);
    assert!(spans.contains(&"00f067aa0ba902b7") && spans.contains(&"00f067aa0ba902b8"), "{spans:?}");
    let e = &v["traces"][b]["events"][0];
    assert_eq!((e["title"].as_str(), e["project_id"].as_u64()), (Some("bang"), Some(1)));
    assert_eq!(e["issue_id"].as_str().unwrap().len(), 16);
    assert_eq!(e["event_id"].as_str().unwrap().len(), 32);

    // An absolute window before the events finds nothing.
    let old =
        t.get(&format!("/v1/events/by-trace?trace_id={a}&from=2020-01-01T00:00:00Z&to=2020-01-02T00:00:00Z")).await;
    assert_eq!(old["traces"], json!({}));

    // Bad ids and too many ids are 400s with the shared error body.
    for q in ["trace_id=nope".to_string(), format!("trace_id={a},"), "trace_id=".into()] {
        let (s, v) = t.api(reqwest::Method::GET, &format!("/v1/events/by-trace?{q}"), None).await;
        assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")), "{q}: {v}");
    }
    let many: Vec<String> = (1..=201).map(|i| format!("{i:032x}")).collect();
    let (s, v) = t.api(reqwest::Method::GET, &format!("/v1/events/by-trace?trace_id={}", many.join(",")), None).await;
    assert_eq!(s, 400, "{v}");
    assert!(v["message"].as_str().unwrap().contains("at most 200"), "{v}");
    t.stop().await;
}

/// Issue 0007: the SDK's clock is two hours ahead. `sent_at` in the envelope
/// tells, so the event lands at the right time, not at the receive time.
#[tokio::test(flavor = "multi_thread")]
async fn envelope_sent_at_corrects_the_client_clock() {
    let t = TestServer::start().await;
    let project = t.create_project("clock").await;
    let (id, dsn) = (project["id"].as_u64().unwrap(), project["dsn"].as_str().unwrap().to_string());
    let key = dsn.split("//").nth(1).unwrap().split('@').next().unwrap().to_string();
    let now = chrono::Utc::now();
    let client_now = now + chrono::Duration::hours(2);
    // Happened ten minutes ago, by the client's clock.
    let happened = client_now - chrono::Duration::minutes(10);
    let body = format!(
        "{}\n{{\"type\":\"event\"}}\n{}\n",
        json!({"event_id": "fc6d8c0c43fc4630ad850ee518f1b9d0", "sent_at": client_now.to_rfc3339()}),
        json!({"message": "drift", "timestamp": happened.to_rfc3339()}),
    );
    let r = t.http.post(format!("{}/api/{id}/envelope/?sentry_key={key}", t.base)).body(body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let list = t.get(&format!("/v1/projects/{id}/issues?since=1h")).await;
    let first_seen: chrono::DateTime<chrono::Utc> = list["issues"][0]["first_seen"].as_str().unwrap().parse().unwrap();
    let off = (first_seen - (now - chrono::Duration::minutes(10))).num_seconds().abs();
    assert!(off < 30, "first seen {first_seen} is {off} s off");
    t.stop().await;
}
