//! Shared by the integration tests: a mallard server in this process on
//! port-0 listeners, and senders for Sentry events and OTLP data.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use mallard_server::{Config, Listeners, Platform};
use serde_json::json;
use tokio_util::sync::CancellationToken;

pub const TOKEN: &str = "test-token";

/// Bind all listeners on port 0 first, so parallel tests never race for ports.
pub async fn listeners() -> Listeners {
    let l = || tokio::net::TcpListener::bind("127.0.0.1:0");
    Listeners {
        http: l().await.unwrap(),
        otlp_http: l().await.unwrap(),
        otlp_grpc: l().await.unwrap(),
        metrics: Some(l().await.unwrap()),
    }
}

pub fn config(dir: &std::path::Path, l: &Listeners) -> Config {
    let mut c = Config::default();
    c.server.public_url = format!("http://{}", l.http.local_addr().unwrap());
    c.server.otlp_http_addr = l.otlp_http.local_addr().unwrap().to_string();
    c.server.metrics_addr = l.metrics.as_ref().unwrap().local_addr().unwrap().to_string();
    c.server.api_tokens = vec![TOKEN.into()];
    c.storage.catalog = dir.join("catalog.ducklake").display().to_string();
    c.storage.data_path = format!("{}/lake/", dir.display());
    c.otel.ingest.flush_interval = std::time::Duration::from_millis(200);
    c
}

pub struct Running {
    pub url: String,
    pub metrics_url: String,
    pub otlp_url: String,
    pub stop: CancellationToken,
    pub task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

pub async fn start(dir: &std::path::Path) -> Running {
    let l = listeners().await;
    let cfg = config(dir, &l);
    let url = cfg.server.public_url.clone();
    let otlp_url = format!("http://{}", cfg.server.otlp_http_addr);
    let metrics_url = format!("http://{}/metrics", cfg.server.metrics_addr);
    // A recorder that is not installed globally: tests must not share one.
    let metrics = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder().handle();
    let platform = Platform::new(cfg).await.expect("platform starts").with_metrics(metrics);
    let stop = CancellationToken::new();
    let task = tokio::spawn(platform.serve_on(l, stop.clone()));
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while http.get(format!("{url}/healthz")).send().await.is_err() {
        assert!(Instant::now() < deadline, "server did not come up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Running { url, metrics_url, otlp_url, stop, task }
}

impl Running {
    pub async fn stop(self) {
        self.stop.cancel();
        self.task.await.unwrap().expect("clean shutdown");
    }
}

pub fn ids(i: usize) -> (String, String) {
    (format!("{:032x}", 0xabc0_0000_u128 + i as u128), format!("{:016x}", 0x5000 + i as u64))
}

pub async fn send_sentry_event(http: &reqwest::Client, url: &str, project: u32, key: &str, i: usize) {
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

pub async fn send_otlp(http: &reqwest::Client, otlp_url: &str, i: usize) {
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
