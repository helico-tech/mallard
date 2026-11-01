//! In-process integration tests: the server runs through the library API,
//! events come from the official Rust `sentry` SDK and raw HTTP.

use sdl_server::{Config, Host, Server, Standalone};
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
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut cfg = Standalone::new(
            &dir.path().join("storage").display().to_string(),
            &dir.path().join("catalog.ducklake").display().to_string(),
            TOKEN,
        );
        cfg.public_url = format!("http://{addr}");
        cfg.batch_window = Some(Duration::from_millis(20));
        let server = Server::new(&cfg).await.unwrap();
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
    let (s, _) = t.api(reqwest::Method::GET, "/v1/projects/web/events?before=garbage", None).await;
    assert_eq!(s, 400);

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
