//! End to end: one mallard process, Sentry events and OTLP spans and
//! logs written concurrently into the same DuckLake catalog, read back per
//! trace through mallard-client, and still there after a restart.

use std::time::{Duration, Instant};

use futures::future::join_all;
use mallard_client::{Client, sdl_client::types::CreateProject};
use mallard_server::{Config, Listeners, Platform};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "test-token";
const TRACES: usize = 40;

/// Bind all three on port 0 first, so parallel tests never race for ports.
async fn listeners() -> Listeners {
    let l = || tokio::net::TcpListener::bind("127.0.0.1:0");
    Listeners { http: l().await.unwrap(), otlp_http: l().await.unwrap(), otlp_grpc: l().await.unwrap() }
}

fn config(dir: &std::path::Path, l: &Listeners) -> Config {
    let mut c = Config::default();
    c.server.public_url = format!("http://{}", l.http.local_addr().unwrap());
    c.server.otlp_http_addr = l.otlp_http.local_addr().unwrap().to_string();
    c.server.api_tokens = vec![TOKEN.into()];
    c.storage.catalog = dir.join("catalog.ducklake").display().to_string();
    c.storage.data_path = format!("{}/lake/", dir.display());
    c.otel.ingest.flush_interval = "200ms".into();
    c
}

struct Running {
    url: String,
    otlp_url: String,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(dir: &std::path::Path) -> Running {
    let l = listeners().await;
    let cfg = config(dir, &l);
    let url = cfg.server.public_url.clone();
    let otlp_url = format!("http://{}", cfg.server.otlp_http_addr);
    let platform = Platform::new(cfg).await.expect("platform starts");
    let stop = CancellationToken::new();
    let task = tokio::spawn(platform.serve_on(l, stop.clone()));
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while http.get(format!("{url}/healthz")).send().await.is_err() {
        assert!(Instant::now() < deadline, "server did not come up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Running { url, otlp_url, stop, task }
}

impl Running {
    async fn stop(self) {
        self.stop.cancel();
        self.task.await.unwrap().expect("clean shutdown");
    }
}

fn ids(i: usize) -> (String, String) {
    (format!("{:032x}", 0xabc0_0000_u128 + i as u128), format!("{:016x}", 0x5000 + i as u64))
}

async fn send_sentry_event(http: &reqwest::Client, url: &str, project: u32, key: &str, i: usize) {
    let (trace, span) = ids(i);
    let event_id = uuid::Uuid::new_v4().simple().to_string();
    let event = json!({
        "event_id": event_id,
        "timestamp": chrono::Utc::now().timestamp(),
        "platform": "node",
        "level": "error",
        "exception": {"values": [{"type": "TypeError", "value": format!("boom {}", i % 3),
            "stacktrace": {"frames": [{"function": "handler", "filename": "app.js", "lineno": 3}]}}]},
        "contexts": {"trace": {"trace_id": trace, "span_id": span}},
        "user": {"ip_address": "{{auto}}"},
    });
    let body = format!("{}\n{}\n{}\n", json!({"event_id": event_id}), json!({"type": "event"}), event);
    let r = http
        .post(format!("{url}/sentry/api/{project}/envelope/"))
        .header("X-Sentry-Auth", format!("Sentry sentry_version=7, sentry_key={key}"))
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "sentry ingest: {}", r.status());
}

async fn send_otlp(http: &reqwest::Client, otlp_url: &str, i: usize) {
    let (trace, span) = ids(i);
    let ns = chrono::Utc::now().timestamp_nanos_opt().unwrap();
    let resource = json!({"attributes": [{"key": "service.name", "value": {"stringValue": "checkout"}}]});
    let spans = json!({"resourceSpans": [{"resource": resource, "scopeSpans": [{"spans": [{
        "traceId": trace, "spanId": span, "name": "POST /pay", "kind": 2,
        "startTimeUnixNano": ns.to_string(), "endTimeUnixNano": (ns + 5_000_000).to_string(),
        "status": {"code": 2}}]}]}]});
    let logs = json!({"resourceLogs": [{"resource": resource, "scopeLogs": [{"logRecords": [{
        "timeUnixNano": ns.to_string(), "severityNumber": 17, "severityText": "ERROR",
        "body": {"stringValue": format!("payment {i} failed")}, "traceId": trace, "spanId": span}]}]}]});
    for (path, body) in [("traces", spans), ("logs", logs)] {
        let r = http.post(format!("{otlp_url}/v1/{path}")).json(&body).send().await.unwrap();
        assert!(r.status().is_success(), "otlp {path}: {}", r.status());
    }
}

/// Polls until every trace shows its span, error and log.
async fn wait_for_all(client: &Client) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let all = join_all((0..TRACES).map(|i| async move {
            let t = client.trace_overview(&ids(i).0, None).await.unwrap();
            t.spans.len() == 1 && t.errors.len() == 1 && t.logs.len() == 1
        }))
        .await;
        let done = all.iter().filter(|ok| **ok).count();
        if done == TRACES {
            return;
        }
        assert!(Instant::now() < deadline, "only {done}/{TRACES} traces complete");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn errors_and_telemetry_share_one_lake() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let project = client
        .sentry
        .create_project(&CreateProject { name: "shop".into(), slug: None, platform: Some("node".into()) })
        .await
        .unwrap();
    assert!(project.dsn.contains("/sentry/"), "DSN points at the nested Sentry router: {}", project.dsn);

    // Both libraries write to the same catalog at the same time.
    let http = reqwest::Client::new();
    let sends = (0..TRACES).map(|i| {
        let (http, url, otlp, key) = (http.clone(), srv.url.clone(), srv.otlp_url.clone(), project.public_key.clone());
        async move {
            tokio::join!(send_sentry_event(&http, &url, project.id, &key, i), send_otlp(&http, &otlp, i));
        }
    });
    join_all(sends).await;
    wait_for_all(&client).await;

    let t = client.trace_overview(&ids(7).0, None).await.unwrap();
    // `{{auto}}` is filled from the peer address, so ConnectInfo reaches sentry.
    let ev =
        client.sentry.event(&project.id.to_string(), &t.errors[0].event_id, Some(&t.errors[0].issue_id)).await.unwrap();
    assert_eq!(ev.payload["user"]["ip_address"], "127.0.0.1", "client IP: {}", ev.payload["user"]);
    assert_eq!(t.spans[0].span_id, ids(7).1);
    assert_eq!(t.errors[0].span_id.as_deref(), Some(ids(7).1.as_str()));
    assert_eq!(t.logs[0].body, "payment 7 failed");

    let ready = reqwest::get(format!("{}/readyz", srv.url)).await.unwrap();
    assert_eq!(ready.status(), 200);

    // Unknown trace: empty, not an error.
    let none = client.trace_overview(&"f".repeat(32), None).await.unwrap();
    assert!(none.spans.is_empty() && none.errors.is_empty() && none.logs.is_empty());

    // Bad token is rejected on both sides.
    let bad = Client::new(&srv.url, Some("wrong")).unwrap();
    assert!(bad.sentry.projects().await.is_err());
    assert!(bad.otel.stats().await.is_err());

    // Shutdown flushes both; a new process on the same lake sees everything.
    srv.stop().await;
    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    wait_for_all(&client).await;
    srv.stop().await;
}
