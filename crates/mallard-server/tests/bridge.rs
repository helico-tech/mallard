//! The bridge (docs/adr/0009-bridge.md) end to end: Sentry transactions
//! become spans in the OTel API, OTel exceptions become Sentry issues
//! linked to their trace, and an exception a Sentry SDK also reported is
//! stored once, like a span an app sends both as a Sentry transaction and
//! over OTLP (ADR 0010).

use std::time::Duration;

mod common;

use common::*;
use mallard_client::{
    Client,
    odl_client::api::{SpanKind, SpanStatus},
    sdl_client::types::{CreateProject, IssueQuery, Project},
};
use serde_json::{Value, json};

async fn project(client: &Client, name: &str) -> Project {
    let req = CreateProject { name: name.into(), slug: None, platform: Some("node".into()) };
    client.sentry.create_project(&req).await.unwrap()
}

fn now_secs() -> f64 {
    chrono::Utc::now().timestamp_micros() as f64 / 1e6
}

/// A @sentry/node style transaction: an HTTP server root and two children.
fn transaction(trace: &str) -> Value {
    let t = now_secs() - 1.0;
    json!({
        "type": "transaction", "transaction": "GET /users/:id", "platform": "node",
        "event_id": uuid::Uuid::new_v4().simple().to_string(),
        "start_timestamp": t, "timestamp": t + 0.25,
        "environment": "production", "release": "shop@1.2.3", "server_name": "web-1",
        "sdk": {"name": "sentry.javascript.node", "version": "8.35.0"},
        "tags": {"tenant": "acme"},
        "contexts": {"trace": {"trace_id": trace, "span_id": "a1b2c3d4e5f60718", "op": "http.server",
            "status": "internal_error", "origin": "auto.http.otel.http",
            "data": {"http.route": "/users/:id", "http.status_code": 500}}},
        "spans": [
            {"span_id": "1111111111111111", "parent_span_id": "a1b2c3d4e5f60718", "trace_id": trace,
             "op": "middleware.express", "description": "query", "status": "ok",
             "start_timestamp": t + 0.001, "timestamp": t + 0.002},
            {"span_id": "2222222222222222", "parent_span_id": "a1b2c3d4e5f60718", "trace_id": trace,
             "op": "db", "description": "SELECT * FROM users WHERE id = $1", "status": "internal_error",
             "data": {"db.system": "postgresql"}, "start_timestamp": t + 0.01, "timestamp": t + 0.24}
        ]
    })
}

fn error_event(trace: &str, span: &str, ty: &str) -> Value {
    json!({
        "event_id": uuid::Uuid::new_v4().simple().to_string(),
        "timestamp": now_secs(), "platform": "node", "level": "error",
        "exception": {"values": [{"type": ty, "value": "boom",
            "stacktrace": {"frames": [{"function": "handler", "filename": "app.js", "lineno": 3}]}}]},
        "contexts": {"trace": {"trace_id": trace, "span_id": span}},
    })
}

fn kv(k: &str, v: &str) -> Value {
    json!({"key": k, "value": {"stringValue": v}})
}

fn resource() -> Value {
    json!({"attributes": [kv("service.name", "checkout"), kv("service.version", "2.4.1"),
        kv("deployment.environment.name", "production"), kv("telemetry.sdk.language", "python")]})
}

/// An OTLP/JSON span; with `exception`, it carries a recorded Python exception.
fn otel_span(trace: &str, span: &str, name: &str, exception: Option<&str>) -> Value {
    let ns = chrono::Utc::now().timestamp_nanos_opt().unwrap();
    let events: Vec<Value> = exception
        .map(|ty| {
            json!({"timeUnixNano": (ns + 1_000_000).to_string(), "name": "exception", "attributes": [
                kv("exception.type", ty), kv("exception.message", "cart 7 is empty"),
                kv("exception.stacktrace", &format!(
                    "Traceback (most recent call last):\n  File \"/app/shop/orders.py\", line 88, in place_order\n    raise {ty}(\"cart 7 is empty\")\n{ty}: cart 7 is empty\n")),
            ]})
        })
        .into_iter()
        .collect();
    json!({"traceId": trace, "spanId": span, "name": name, "kind": 2,
        "startTimeUnixNano": ns.to_string(), "endTimeUnixNano": (ns + 5_000_000).to_string(),
        "status": {"code": if exception.is_some() { 2 } else { 0 }}, "events": events})
}

async fn post_spans(http: &reqwest::Client, otlp: &str, spans: Vec<Value>) {
    let body = json!({"resourceSpans": [{"resource": resource(), "scopeSpans": [{"spans": spans}]}]});
    let r = http.post(format!("{otlp}/v1/traces")).json(&body).send().await.unwrap();
    assert!(r.status().is_success(), "otlp: {}", r.status());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sentry_transactions_become_traces() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let shop = project(&client, "shop").await;
    let http = reqwest::Client::new();
    let trace = "4bf92f3577b34da6a3ce929d0e0e4736";

    send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "transaction", &transaction(trace)).await;
    // An error of the same request, captured in the database span.
    let err = error_event(trace, "2222222222222222", "DbError");
    send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "event", &err).await;

    let t = eventually("the transaction's spans and the error", || async {
        let t = client.trace_overview(trace, None).await.unwrap();
        (t.spans.len() == 3 && t.errors.len() == 1).then_some(t)
    })
    .await;
    let span = |id: &str| t.spans.iter().find(|s| s.span_id == id).unwrap();
    let root = span("a1b2c3d4e5f60718");
    assert_eq!(root.name, "GET /users/:id");
    assert_eq!(root.service_name, "shop", "service = project slug");
    assert_eq!(root.kind, SpanKind::Server);
    assert_eq!(root.status_code, SpanStatus::Error);
    assert_eq!(root.status_message, "internal_error");
    assert_eq!(root.duration_ns, 250_000_000);
    assert_eq!(root.attributes.get("http.route").map(String::as_str), Some("/users/:id"));
    assert_eq!(root.attributes.get("tenant").map(String::as_str), Some("acme"));
    assert_eq!(root.resource_attributes.get("service.version").map(String::as_str), Some("shop@1.2.3"));
    assert_eq!(root.scope_name, "sentry.javascript.node");
    let db = span("2222222222222222");
    assert_eq!(db.name, "SELECT * FROM users WHERE id = $1");
    assert_eq!(db.kind, SpanKind::Client);
    assert_eq!(db.parent_span_id.as_deref(), Some("a1b2c3d4e5f60718"));
    assert_eq!(span("1111111111111111").status_code, SpanStatus::Ok);
    assert_eq!(t.errors[0].span_id.as_deref(), Some("2222222222222222"), "the error sits under the db span");
    srv.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn otel_exceptions_become_issues_once() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| c.bridge.dedupe_window = Duration::from_secs(1)).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let web = project(&client, "web").await;
    let http = reqwest::Client::new();
    let trace = "5bf92f3577b34da6a3ce929d0e0e4737";
    let (only_otel, sentry_first, sentry_after) = ("000000000000000a", "000000000000000b", "000000000000000c");

    // The app's Sentry SDK reports span b before the OTel export, span c
    // right after it (within the dedupe window). Span a only goes via OTel.
    let sdk = |span: &str| error_event(trace, span, "TypeError");
    send_envelope_item(&http, &srv.url, web.id, &web.public_key, "event", &sdk(sentry_first)).await;
    post_spans(
        &http,
        &srv.otlp_url,
        vec![
            otel_span(trace, only_otel, "POST /checkout", Some("ValueError")),
            otel_span(trace, sentry_first, "charge", Some("KeyError")),
            otel_span(trace, sentry_after, "notify", Some("OSError")),
        ],
    )
    .await;
    send_envelope_item(&http, &srv.url, web.id, &web.public_key, "event", &sdk(sentry_after)).await;
    // An exception in a log record, linked to the same trace.
    let ns = chrono::Utc::now().timestamp_nanos_opt().unwrap().to_string();
    let logs = json!({"resourceLogs": [{"resource": resource(), "scopeLogs": [{"logRecords": [{
        "timeUnixNano": ns, "severityNumber": 17, "body": {"stringValue": "refund failed"},
        "traceId": trace, "spanId": "000000000000000d",
        "attributes": [kv("exception.type", "RuntimeError"), kv("exception.message", "gateway down")]}]}]}]});
    assert!(http.post(format!("{}/v1/logs", srv.otlp_url)).json(&logs).send().await.unwrap().status().is_success());

    // The service's project is created; the issue is linked to the trace.
    let issues = eventually("the issues of service checkout", || async {
        let list = client.sentry.issues("checkout", &IssueQuery::default()).await.ok()?;
        (list.issues.len() == 2).then_some(list.issues)
    })
    .await;
    assert!(issues.iter().any(|i| i.title == "ValueError: cart 7 is empty" && i.platform == "python"));
    let t = client.trace_overview(trace, None).await.unwrap();
    let e = t.errors.iter().find(|e| e.span_id.as_deref() == Some(only_otel)).expect("in the trace view");
    let ev = client.sentry.event("checkout", &e.event_id, Some(&e.issue_id)).await.unwrap();
    assert_eq!(ev.payload["contexts"]["trace"]["trace_id"], trace);
    assert_eq!(ev.payload["contexts"]["trace"]["span_id"], only_otel);
    assert_eq!(ev.payload["environment"], "production");
    assert_eq!(ev.payload["release"], "2.4.1");
    assert_eq!(ev.payload["platform"], "python");
    let frames = &ev.payload["exception"]["values"][0]["stacktrace"]["frames"];
    assert_eq!(frames[0]["function"], "place_order", "{frames}");
    assert!(issues.iter().any(|i| i.title == "RuntimeError: gateway down"), "from the log record");

    // Long after the window: one event per span, the Sentry SDK's where it sent one.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let t = client.trace_overview(trace, None).await.unwrap();
    let mut seen: Vec<(String, String)> =
        t.errors.iter().map(|e| (e.span_id.clone().unwrap_or_default(), e.title.clone())).collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            (only_otel.to_string(), "ValueError: cart 7 is empty".to_string()),
            (sentry_first.to_string(), "TypeError: boom".to_string()),
            (sentry_after.to_string(), "TypeError: boom".to_string()),
            ("000000000000000d".to_string(), "RuntimeError: gateway down".to_string()),
        ]
    );
    srv.stop().await;
}

/// An app on @sentry/node 8+ that also exports OTLP sends its root span and
/// one child both ways (same ids); its db span only goes to Sentry. Each
/// span is stored once, the OTLP copy where there is one, whichever copy
/// arrives first.
async fn spans_sent_both_ways_are_stored_once(sentry_first: bool) {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| c.bridge.dedupe_window = Duration::from_secs(1)).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let shop = project(&client, "shop").await;
    let http = reqwest::Client::new();
    let trace = if sentry_first { "8bf92f3577b34da6a3ce929d0e0e4740" } else { "9bf92f3577b34da6a3ce929d0e0e4741" };
    let otlp = vec![
        otel_span(trace, "a1b2c3d4e5f60718", "GET /users/:id", None),
        otel_span(trace, "1111111111111111", "query", None),
    ];
    if sentry_first {
        send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "transaction", &transaction(trace)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        post_spans(&http, &srv.otlp_url, otlp).await;
    } else {
        post_spans(&http, &srv.otlp_url, otlp).await;
        eventually("the OTLP spans", || async {
            (client.trace_overview(trace, None).await.unwrap().spans.len() == 2).then_some(())
        })
        .await;
        send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "transaction", &transaction(trace)).await;
    }

    eventually("three spans", || async {
        (client.trace_overview(trace, None).await.unwrap().spans.len() >= 3).then_some(())
    })
    .await;
    // Long after the window and a flush: still one row per span.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let t = client.trace_overview(trace, None).await.unwrap();
    let mut services: Vec<(&str, &str)> =
        t.spans.iter().map(|s| (s.span_id.as_str(), s.service_name.as_str())).collect();
    services.sort();
    assert_eq!(
        services,
        [("1111111111111111", "checkout"), ("2222222222222222", "shop"), ("a1b2c3d4e5f60718", "checkout")],
        "the OTLP copies, plus the span only Sentry sent"
    );
    srv.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spans_sent_both_ways_otlp_first() {
    spans_sent_both_ways_are_stored_once(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spans_sent_both_ways_sentry_first() {
    spans_sent_both_ways_are_stored_once(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bridge_can_be_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| {
        c.bridge.transactions = false;
        c.bridge.exceptions = false;
    })
    .await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let shop = project(&client, "shop").await;
    let http = reqwest::Client::new();
    let (tx_trace, otel_trace) = ("6bf92f3577b34da6a3ce929d0e0e4738", "7bf92f3577b34da6a3ce929d0e0e4739");
    send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "transaction", &transaction(tx_trace)).await;
    post_spans(&http, &srv.otlp_url, vec![otel_span(otel_trace, "000000000000000a", "POST /checkout", Some("E"))])
        .await;
    // The OTLP span itself is stored; nothing crosses over.
    eventually("the OTLP span", || async {
        (client.trace_overview(otel_trace, None).await.unwrap().spans.len() == 1).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(client.trace_overview(tx_trace, None).await.unwrap().spans.is_empty());
    assert!(client.trace_overview(otel_trace, None).await.unwrap().errors.is_empty());
    assert_eq!(client.sentry.projects().await.unwrap().len(), 1, "no project for service checkout");
    srv.stop().await;
}
