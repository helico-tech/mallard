//! The bridge (docs/adr/0009-bridge.md) end to end: Sentry transactions and
//! streamed span items (ADR 0013) become spans in the OTel API, OTel
//! exceptions become Sentry issues linked to their trace, and an exception a
//! Sentry SDK also reported is stored once, like a span an app sends both
//! through a Sentry SDK and over OTLP (ADR 0010).

use std::time::Duration;

mod common;

use common::*;
use mallard_client::{
    Client,
    mallard_otel_client::api::{SpanKind, SpanStatus},
    mallard_sentry_client::types::{CreateProject, IssueQuery, Project},
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

/// The same three spans as [`transaction`], as a span streaming envelope.
fn span_envelope(trace: &str) -> String {
    let t = now_secs() - 1.0;
    let attr = |v: &str| json!({"type": "string", "value": v});
    let common = |op: &str| {
        json!({"sentry.op": attr(op), "sentry.segment.name": attr("GET /users/:id"),
            "sentry.segment.id": attr("a1b2c3d4e5f60718"), "sentry.release": attr("shop@1.2.3"),
            "sentry.environment": attr("production"), "sentry.sdk.name": attr("sentry.javascript.node"),
            "sentry.sdk.version": attr("11.10.0")})
    };
    let payload = json!({"version": 2, "items": [
        {"trace_id": trace, "span_id": "1111111111111111", "parent_span_id": "a1b2c3d4e5f60718",
         "name": "query", "status": "ok", "is_segment": false,
         "start_timestamp": t + 0.001, "end_timestamp": t + 0.002, "attributes": common("middleware.express")},
        {"trace_id": trace, "span_id": "2222222222222222", "parent_span_id": "a1b2c3d4e5f60718",
         "name": "SELECT * FROM users WHERE id = $1", "status": "error", "is_segment": false,
         "start_timestamp": t + 0.01, "end_timestamp": t + 0.24, "attributes": common("db")},
        {"trace_id": trace, "span_id": "a1b2c3d4e5f60718", "name": "GET /users/:id", "status": "error",
         "is_segment": true, "start_timestamp": t, "end_timestamp": t + 0.25, "attributes": common("http.server")}
    ]});
    let header = json!({"trace": {"trace_id": trace, "public_key": "x", "release": "shop@1.2.3"}});
    let item = json!({"type": "span", "item_count": 3, "content_type": "application/vnd.sentry.items.span.v2+json"});
    format!("{header}\n{item}\n{payload}\n")
}

/// A span envelope captured from @sentry/node 11.10 running
/// `e2e/sentry/fixtures/node-tracing-app.mjs` (only host details and most
/// request headers removed), moved to `trace` and to the last few seconds.
fn captured_span_envelope(trace: &str) -> String {
    let text =
        include_str!("fixtures/sentry-node-11-spans.envelope").replace("96840ee330eb4f48adfeda513ab13381", trace);
    let mut lines = text.lines();
    let (header, item) = (lines.next().unwrap(), lines.next().unwrap());
    let mut payload: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let spans = payload["items"].as_array_mut().unwrap();
    let last = spans.iter().map(|s| s["end_timestamp"].as_f64().unwrap()).fold(0.0, f64::max);
    let shift = now_secs() - 1.0 - last;
    for s in spans {
        for k in ["start_timestamp", "end_timestamp"] {
            s[k] = json!(s[k].as_f64().unwrap() + shift);
        }
    }
    format!("{header}\n{item}\n{payload}\n")
}

/// An INP span envelope captured from @sentry/browser 10.78 in headless
/// Chromium with `sendDefaultPii` (issue 0094; nothing removed), moved to the
/// last few seconds. A whole number of seconds keeps the digits of its times.
fn captured_inp_envelope() -> String {
    let text = include_str!("fixtures/sentry-browser-10-inp.envelope");
    let mut lines = text.lines();
    let (header, item) = (lines.next().unwrap(), lines.next().unwrap());
    let mut span: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let shift = (now_secs() - 1.0 - span["timestamp"].as_f64().unwrap()).round();
    for k in ["start_timestamp", "timestamp"] {
        span[k] = json!(span[k].as_f64().unwrap() + shift);
    }
    format!("{header}\n{item}\n{span}\n")
}

async fn post_envelope(http: &reqwest::Client, url: &str, project: &Project, body: String) {
    let r = http
        .post(format!("{url}/sentry/api/{}/envelope/", project.id))
        .header("X-Sentry-Auth", format!("Sentry sentry_version=7, sentry_key={}", project.public_key))
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "sentry ingest: {}", r.status());
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
async fn sentry_span_items_become_traces() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| c.bridge.dedupe_window = Duration::from_millis(200)).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let shop = project(&client, "shop").await;
    let http = reqwest::Client::new();
    let trace = "abf92f3577b34da6a3ce929d0e0e4742";

    // An INP span of @sentry/browser 10 (standalone: no content type), trimmed;
    // `captured_browser_10_inp_span_becomes_an_otel_span` sends a real one.
    let inp_trace = "bbf92f3577b34da6a3ce929d0e0e4743";
    let t = now_secs() - 1.0;
    let inp = json!({"trace_id": inp_trace, "span_id": "1111111111111111",
        "description": "body > main > button#checkout", "op": "ui.interaction.click",
        "origin": "auto.http.browser.inp", "start_timestamp": t, "timestamp": t + 0.232,
        "is_segment": true, "segment_id": "1111111111111111", "exclusive_time": 232,
        "measurements": {"inp": {"value": 232, "unit": "millisecond"}},
        "data": {"sentry.op": "ui.interaction.click", "sentry.origin": "auto.http.browser.inp",
            "release": "shop-web@2.4.0", "environment": "production", "transaction": "/checkout"}});
    let header = json!({"sent_at": "2026-11-01T12:00:00.000Z",
        "trace": {"trace_id": inp_trace, "public_key": shop.public_key, "environment": "production", "release": "shop-web@2.4.0"}});
    post_envelope(&http, &srv.url, &shop, format!("{header}\n{{\"type\":\"span\"}}\n{inp}\n")).await;
    // A span item with a content type we do not know is not stored.
    let future_trace = "cbf92f3577b34da6a3ce929d0e0e4744";
    let future = json!({"trace_id": future_trace, "span_id": "1111111111111111", "timestamp": t});
    let v9 = json!({"type": "span", "content_type": "application/vnd.sentry.items.span.v9+json"});
    post_envelope(&http, &srv.url, &shop, format!("{{}}\n{v9}\n{future}\n")).await;
    // Garbage in a span item does not fail the request.
    let v2 = json!({"type": "span", "content_type": "application/vnd.sentry.items.span.v2+json"});
    post_envelope(&http, &srv.url, &shop, format!("{{}}\n{v2}\nnot json\n")).await;

    post_envelope(&http, &srv.url, &shop, captured_span_envelope(trace)).await;
    // The error the SDK captured in the charge span.
    let err = error_event(trace, "88bd0a55715169b9", "Error");
    send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "event", &err).await;

    let t = eventually("the five spans and the error", || async {
        let t = client.trace_overview(trace, None).await.unwrap();
        (t.spans.len() == 5 && t.errors.len() == 1).then_some(t)
    })
    .await;
    let span = |id: &str| t.spans.iter().find(|s| s.span_id == id).unwrap();

    let root = span("adaa534ad3e8ef99");
    assert_eq!(root.name, "checkout client");
    assert_eq!(root.parent_span_id, None);
    assert_eq!(root.service_name, "shop", "service = project slug");
    assert_eq!(root.kind, SpanKind::Internal);
    assert_eq!(root.status_code, SpanStatus::Ok);
    assert_eq!(root.resource_attributes.get("service.version").map(String::as_str), Some("tracing-app@1.0.0"));
    assert_eq!(root.resource_attributes.get("deployment.environment.name").map(String::as_str), Some("e2e"));
    assert_eq!(root.scope_name, "sentry.javascript.node");
    assert_eq!(root.scope_version, "11.10.0");

    let fetch = span("95c5b31c975f2037");
    assert_eq!(fetch.kind, SpanKind::Client, "sentry.kind");
    assert_eq!(fetch.parent_span_id.as_deref(), Some("adaa534ad3e8ef99"));
    assert_eq!(fetch.attributes.get("sentry.segment.name").map(String::as_str), Some("checkout client"));

    let server = span("bc2f788536782290");
    assert_eq!(server.name, "GET");
    assert_eq!(server.kind, SpanKind::Server);
    assert_eq!(server.parent_span_id.as_deref(), Some("95c5b31c975f2037"));
    assert_eq!(server.status_code, SpanStatus::Error);
    assert_eq!(server.status_message, "internal_error");
    assert_eq!(server.attributes.get("sentry.op").map(String::as_str), Some("http.server"));
    assert_eq!(server.attributes.get("http.response.status_code").map(String::as_str), Some("500"));
    assert_eq!(server.attributes.get("http.request.header.accept").map(String::as_str), Some(r#"["*/*"]"#));
    assert!(!server.attributes.contains_key("sentry.release"), "on the resource");

    assert_eq!(span("8074ae85bb09e3f5").kind, SpanKind::Client, "from op db.query");
    let charge = span("88bd0a55715169b9");
    assert_eq!(charge.name, "charge card");
    assert_eq!(charge.kind, SpanKind::Internal);
    assert_eq!(charge.parent_span_id.as_deref(), Some("bc2f788536782290"));
    assert_eq!(
        t.errors[0].span_id.as_deref(),
        Some("88bd0a55715169b9"),
        "the error sits under the span it was thrown in"
    );

    let inp = eventually("the INP span", || async {
        client.trace_overview(inp_trace, None).await.unwrap().spans.into_iter().next()
    })
    .await;
    assert_eq!(inp.name, "body > main > button#checkout");
    assert_eq!(inp.service_name, "shop");
    assert_eq!(inp.resource_attributes.get("service.version").map(String::as_str), Some("shop-web@2.4.0"));
    assert_eq!(inp.attributes.get("browser.web_vital.inp.value").map(String::as_str), Some("232"));
    assert!(client.trace_overview(future_trace, None).await.unwrap().spans.is_empty(), "unknown content type");
    srv.stop().await;
}

/// ADR 0022 against what a running SDK sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captured_browser_10_inp_span_becomes_an_otel_span() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| c.bridge.dedupe_window = Duration::from_millis(200)).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let shop = project(&client, "shop").await;
    post_envelope(&reqwest::Client::new(), &srv.url, &shop, captured_inp_envelope()).await;

    let trace = "b806bb9d3a1344b491944a0f8a35a997";
    let inp = eventually("the INP span", || async {
        client.trace_overview(trace, None).await.unwrap().spans.into_iter().next()
    })
    .await;
    assert_eq!(inp.span_id, "9d4ccd2b8dce23b0");
    assert_eq!(inp.trace_id, trace);
    assert_eq!(inp.parent_span_id, None, "no span was active when the SDK reported it");
    assert_eq!(inp.name, "body > main#app > button#checkout.primary");
    assert_eq!(inp.kind, SpanKind::Internal);
    assert_eq!(inp.duration_ns, 344_000_000);
    assert_eq!(inp.status_code, SpanStatus::Unset, "the SDK sets no status");

    assert_eq!(inp.service_name, "shop", "service = project slug");
    assert_eq!(inp.resource_attributes.get("service.version").map(String::as_str), Some("shop-web@2.4.0"));
    assert_eq!(inp.resource_attributes.get("deployment.environment.name").map(String::as_str), Some("e2e"));
    assert_eq!(inp.scope_name, "sentry", "the envelope has no sdk header");
    assert_eq!(inp.scope_version, "");

    let attr = |k: &str| inp.attributes.get(k).map(String::as_str);
    assert_eq!(attr("browser.web_vital.inp.value"), Some("344"));
    assert_eq!(attr("sentry.op"), Some("ui.interaction.click"));
    assert_eq!(attr("sentry.origin"), Some("auto.http.browser.inp"));
    assert_eq!(attr("sentry.exclusive_time"), Some("344"));
    assert_eq!(attr("transaction"), Some("/"));
    assert!(attr("user_agent.original").is_some_and(|ua| ua.contains("HeadlessChrome/")));
    assert_eq!(attr("client.address"), Some("127.0.0.1"), "`{{auto}}` is the address of the request");
    assert_eq!(attr("release"), None, "on the resource");
    assert_eq!(attr("environment"), None, "on the resource");
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

/// A project slug may hold `_`, which the slug rule turns into `-`. Its service's
/// exceptions go to that project and open no project `my-app` (issue 0142).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn otel_exceptions_find_a_project_with_an_underscore() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| {
        c.bridge.dedupe_window = Duration::from_millis(100);
        c.bridge.default_project = "catch-all".into();
    })
    .await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let req = CreateProject { name: "Mine".into(), slug: Some("my_app".into()), platform: None };
    let mine = client.sentry.create_project(&req).await.unwrap();

    let span = otel_span("5bf92f3577b34da6a3ce929d0e0e4738", "000000000000000a", "work", Some("ValueError"));
    let spans = json!({"resourceSpans": [{
        "resource": {"attributes": [kv("service.name", "my_app")]},
        "scopeSpans": [{"spans": [span]}]}]});
    let http = reqwest::Client::new();
    assert!(http.post(format!("{}/v1/traces", srv.otlp_url)).json(&spans).send().await.unwrap().status().is_success());

    let issues = eventually("the issue in project my_app", || async {
        let list = client.sentry.issues("my_app", &IssueQuery::default()).await.ok()?;
        (!list.issues.is_empty()).then_some(list.issues)
    })
    .await;
    assert_eq!(issues[0].project_id, mine.id);
    let slugs: Vec<String> = client.sentry.projects().await.unwrap().into_iter().map(|p| p.slug).collect();
    assert_eq!(slugs, ["my_app"], "no second project, and not the default one");
    srv.stop().await;
}

/// An app on @sentry/node 8+ that also exports OTLP sends its root span and
/// one child both ways (same ids); its db span only goes to Sentry. Each
/// span is stored once, the OTLP copy where there is one, whichever copy
/// arrives first and whether the Sentry copy is a transaction or streamed.
async fn spans_sent_both_ways_are_stored_once(trace: &str, sentry_first: bool, streamed: bool) {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| c.bridge.dedupe_window = Duration::from_secs(1)).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let shop = project(&client, "shop").await;
    let http = reqwest::Client::new();
    let send_sentry = async || {
        if streamed {
            post_envelope(&http, &srv.url, &shop, span_envelope(trace)).await;
        } else {
            send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "transaction", &transaction(trace)).await;
        }
    };
    let otlp = vec![
        otel_span(trace, "a1b2c3d4e5f60718", "GET /users/:id", None),
        otel_span(trace, "1111111111111111", "query", None),
    ];
    if sentry_first {
        send_sentry().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        post_spans(&http, &srv.otlp_url, otlp).await;
    } else {
        post_spans(&http, &srv.otlp_url, otlp).await;
        eventually("the OTLP spans", || async {
            (client.trace_overview(trace, None).await.unwrap().spans.len() == 2).then_some(())
        })
        .await;
        send_sentry().await;
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
    spans_sent_both_ways_are_stored_once("9bf92f3577b34da6a3ce929d0e0e4741", false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spans_sent_both_ways_sentry_first() {
    spans_sent_both_ways_are_stored_once("8bf92f3577b34da6a3ce929d0e0e4740", true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamed_spans_sent_both_ways_otlp_first() {
    spans_sent_both_ways_are_stored_once("cbf92f3577b34da6a3ce929d0e0e4744", false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamed_spans_sent_both_ways_sentry_first() {
    spans_sent_both_ways_are_stored_once("dbf92f3577b34da6a3ce929d0e0e4745", true, true).await;
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
    let streamed_trace = "ebf92f3577b34da6a3ce929d0e0e4746";
    send_envelope_item(&http, &srv.url, shop.id, &shop.public_key, "transaction", &transaction(tx_trace)).await;
    post_envelope(&http, &srv.url, &shop, span_envelope(streamed_trace)).await;
    post_spans(&http, &srv.otlp_url, vec![otel_span(otel_trace, "000000000000000a", "POST /checkout", Some("E"))])
        .await;
    // The OTLP span itself is stored; nothing crosses over.
    eventually("the OTLP span", || async {
        (client.trace_overview(otel_trace, None).await.unwrap().spans.len() == 1).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(client.trace_overview(tx_trace, None).await.unwrap().spans.is_empty());
    assert!(client.trace_overview(streamed_trace, None).await.unwrap().spans.is_empty());
    assert!(client.trace_overview(otel_trace, None).await.unwrap().errors.is_empty());
    assert_eq!(client.sentry.projects().await.unwrap().len(), 1, "no project for service checkout");
    srv.stop().await;
}
