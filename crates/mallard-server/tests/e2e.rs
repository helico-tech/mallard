//! End to end: one mallard process, Sentry events and OTLP spans and
//! logs written concurrently into the same DuckLake catalog, read back per
//! trace through mallard-client, and still there after a restart.

use std::time::{Duration, Instant};

mod common;

use common::*;
use futures::future::join_all;
use mallard_client::{Client, mallard_sentry_client::types::CreateProject};

const TRACES: usize = 40;

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
    // server.metrics_addr: /metrics there, not on the public port.
    assert_eq!(reqwest::get(&srv.metrics_url).await.unwrap().status(), 200);
    assert_eq!(reqwest::get(format!("{}/metrics", srv.url)).await.unwrap().status(), 404);
    // CORS preflight for browser OTLP exporters.
    let preflight = http
        .request(reqwest::Method::OPTIONS, format!("{}/v1/traces", srv.otlp_url))
        .header("origin", "https://app.example")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.headers()["access-control-allow-origin"], "https://app.example");

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
