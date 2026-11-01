//! The APIs behind pickers and filter bars, on a running server: `/api/v1/apps`,
//! `/api/v1/find`, tag suggestions, attribute prefixes and repeated `service=`.
//! Service names, tag keys and tag values come from SDKs, so hostile ones are here.

mod common;

use common::*;
use mallard_client::{Client, mallard_sentry_client::types::CreateProject};
use serde_json::{Value, json};

/// A SQL-looking value must come back as data or match nothing.
const EVIL: &str = "x'; DROP TABLE spans; --";

struct Api {
    http: reqwest::Client,
    url: String,
}

impl Api {
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> (u16, Value) {
        let r = self.http.get(format!("{}{path}", self.url)).bearer_auth(TOKEN).query(query).send().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path} {status}: {e}: {text}")))
    }

    async fn ok(&self, path: &str, query: &[(&str, &str)]) -> Value {
        let (status, v) = self.get(path, query).await;
        assert_eq!(status, 200, "{path} {query:?}: {v}");
        v
    }
}

/// OTLP spans of `service` with attributes (one request).
async fn send_spans(http: &reqwest::Client, otlp: &str, service: &str, route: &str, trace: u128) {
    let ns = chrono::Utc::now().timestamp_nanos_opt().unwrap();
    let body = json!({"resourceSpans": [{
    "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": service}}]},
    "scopeSpans": [{"spans": [{
        "traceId": format!("{trace:032x}"), "spanId": format!("{:016x}", trace as u64 + 1), "name": "GET", "kind": 2,
        "startTimeUnixNano": ns.to_string(), "endTimeUnixNano": (ns + 1000).to_string(),
        "attributes": [{"key": "http.route", "value": {"stringValue": route}}, {"key": EVIL, "value": {"stringValue": EVIL}}]
    }]}]}]});
    let r = http.post(format!("{otlp}/v1/traces")).json(&body).send().await.unwrap();
    assert!(r.status().is_success(), "otlp: {}", r.status());
}

fn values(v: &Value) -> Vec<&str> {
    v["items"].as_array().unwrap().iter().filter_map(|i| i["value"].as_str()).collect()
}

fn keys(v: &Value) -> Vec<&str> {
    v["items"].as_array().unwrap().iter().filter_map(|i| i["key"].as_str()).collect()
}

fn services_of(v: &Value) -> Vec<&str> {
    let mut s: Vec<&str> = v.as_array().unwrap().iter().map(|o| o["service"].as_str().unwrap()).collect();
    s.sort();
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apps_find_and_suggestions() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let http = reqwest::Client::new();
    let api = Api { http: http.clone(), url: srv.url.clone() };
    let project = client
        .sentry
        .create_project(&CreateProject { name: "Shop Checkout".into(), slug: Some("checkout".into()), platform: None })
        .await
        .unwrap();
    client
        .sentry
        .create_project(&CreateProject { name: "Quiet".into(), slug: Some("quiet".into()), platform: None })
        .await
        .unwrap();

    // Before any data: every project is an app, nothing else.
    let v = api.ok("/api/v1/apps", &[]).await;
    assert_eq!(v["apps"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(
        v["apps"][1],
        json!({"slug": "quiet", "name": "Quiet", "project": v["apps"][1]["project"], "services": []})
    );

    // One request in both sides, one more service with a second name, one without a project.
    let event_id = "d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0";
    let (trace, span) = ids(0);
    send_otlp(&http, &srv.otlp_url, 0).await;
    let event = json!({
        "event_id": event_id,
        "timestamp": chrono::Utc::now().timestamp(),
        "level": "error",
        "environment": "production",
        "message": "boom",
        "tags": {"tenant": "acme", "Tier": "Gold", "url": EVIL, EVIL: "v"},
        "contexts": {"trace": {"trace_id": trace, "span_id": span}},
    });
    send_envelope_item(&http, &srv.url, project.id, &project.public_key, "event", &event).await;
    let event2 = json!({"event_id": "d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1", "timestamp": chrono::Utc::now().timestamp(),
        "level": "error", "message": "boom", "tags": {"tenant": "acme-eu"}});
    send_envelope_item(&http, &srv.url, project.id, &project.public_key, "event", &event2).await;
    send_spans(&http, &srv.otlp_url, "Checkout", "/pay", 7).await;
    send_spans(&http, &srv.otlp_url, "worker", "/job", 8).await;
    send_spans(&http, &srv.otlp_url, EVIL, "/evil", 9).await;

    // apps: joined on the slug, sorted, with a hostile name as plain data.
    let v = eventually("all services", || async {
        let v = api.ok("/api/v1/apps", &[("since", "1h")]).await;
        (v["apps"].as_array().unwrap().len() == 4 && v["apps"][0]["services"].as_array().unwrap().len() == 2)
            .then_some(v)
    })
    .await;
    assert_eq!(
        v["apps"][0],
        json!({"slug": "checkout", "name": "Shop Checkout", "project": project.id, "services": ["Checkout", "checkout"]})
    );
    let slugs: Vec<&str> = v["apps"].as_array().unwrap().iter().map(|a| a["slug"].as_str().unwrap()).collect();
    assert_eq!(slugs, ["checkout", "quiet", "worker", "x-drop-table-spans"]);
    assert_eq!(v["warnings"], json!([]));
    // The same list from the client the CLI uses.
    let typed = client.apps(&Default::default()).await.unwrap();
    assert_eq!(typed.apps.iter().map(|a| a.slug.as_str()).collect::<Vec<_>>(), slugs);

    // find, 32 hex: the trace with a time, the event (found by its own id) with the slug.
    let v = eventually("the trace with its error", || async {
        let (status, v) = api.get("/api/v1/find", &[("id", &trace)]).await;
        (status == 200 && v["matches"][0]["errors"] == 1 && v["matches"][0]["logs"] == 1).then_some(v)
    })
    .await;
    assert_eq!(v["matches"][0]["kind"], "trace", "{v}");
    assert_eq!(v["matches"][0]["spans"], 1);
    assert!(v["matches"][0]["time"].as_str().is_some_and(|t| t.ends_with('Z')), "{v}");
    let v = api.ok("/api/v1/find", &[("id", event_id)]).await;
    assert_eq!(v["matches"][0]["kind"], "event", "{v}");
    assert_eq!(v["matches"][0]["project"], "checkout");
    assert_eq!(v["matches"][0]["project_id"], project.id);
    // With dashes and in capitals, as pasted from elsewhere.
    let dashed = "D0D0D0D0-D0D0-D0D0-D0D0-D0D0D0D0D0D0";
    assert_eq!(api.ok("/api/v1/find", &[("id", dashed)]).await["matches"][0]["kind"], "event");

    // find, 16 hex: the span, and the issue in every project or only in the one asked for.
    let v = api.ok("/api/v1/find", &[("id", &span)]).await;
    assert_eq!(v["matches"][0]["kind"], "span", "{v}");
    assert_eq!(v["matches"][0]["trace_id"], trace.as_str());
    assert_eq!(v["matches"][0]["service"], "checkout");
    assert!(v["matches"][0]["time"].is_string(), "{v}");
    let issue =
        api.ok("/api/v1/find", &[("id", event_id)]).await["matches"][0]["issue_id"].as_str().unwrap().to_string();
    let v = api.ok("/api/v1/find", &[("id", &issue)]).await;
    assert_eq!(
        (v["matches"][0]["kind"].as_str(), v["matches"][0]["project"].as_str()),
        (Some("issue"), Some("checkout"))
    );
    let v = api.ok("/api/v1/find", &[("id", &issue), ("project", "checkout")]).await;
    assert_eq!(v["matches"][0]["count"], 2, "{v}");
    let (s, v) = api.get("/api/v1/find", &[("id", &issue), ("project", "quiet")]).await;
    assert_eq!((s, v["error"].as_str()), (404, Some("not_found")), "{v}");
    let (s, v) = api.get("/api/v1/find", &[("id", &issue), ("project", EVIL)]).await;
    assert_eq!((s, v["error"].as_str()), (404, Some("unknown_project")), "{v}");

    // Tag keys, with counts; a prefix on the keys, in any case.
    let tags = format!("/sentry/v1/projects/{}/tags", project.id);
    let v = api.ok(&tags, &[("since", "1h")]).await;
    assert_eq!(v["total_rows"], 2, "{v}");
    assert_eq!(v["sampled_rows"], 2);
    assert_eq!(v["items"].as_array().unwrap().iter().find(|i| i["key"] == "tenant").unwrap()["count"], 2, "{v}");
    assert!(keys(&v).contains(&EVIL), "a hostile key is data: {v}");
    assert_eq!(keys(&api.ok(&tags, &[("prefix", "TEN")]).await), ["tenant"]);
    assert_eq!(keys(&api.ok(&tags, &[("prefix", "%")]).await), Vec::<&str>::new(), "no LIKE wildcards");

    // Top values of one key, by slug too, with a prefix in any case.
    let by_slug = "/sentry/v1/projects/checkout/tags";
    let v = api.ok(by_slug, &[("key", "tenant")]).await;
    assert_eq!(values(&v), ["acme", "acme-eu"], "{v}");
    let v = api.ok(by_slug, &[("key", "tenant"), ("prefix", "ACME-")]).await;
    assert_eq!((values(&v), &v["total_rows"]), (vec!["acme-eu"], &json!(1)), "{v}");
    assert_eq!(values(&api.ok(by_slug, &[("key", "Tier"), ("prefix", "g")]).await), ["Gold"]);
    assert_eq!(values(&api.ok(by_slug, &[("key", "tenant"), ("limit", "1")]).await).len(), 1);

    // SQL-looking input is a value like any other.
    assert_eq!(values(&api.ok(by_slug, &[("key", EVIL)]).await), ["v"]);
    assert_eq!(values(&api.ok(by_slug, &[("key", "url"), ("prefix", EVIL)]).await), [EVIL]);
    assert_eq!(values(&api.ok(by_slug, &[("key", "tenant"), ("prefix", "' OR 1=1 --")]).await), Vec::<&str>::new());
    assert_eq!(values(&api.ok(by_slug, &[("key", "tenant"), ("prefix", "a\0b")]).await), Vec::<&str>::new());

    // Telemetry attributes: the same shape, with a prefix too.
    let attrs = "/otel/api/v1/attributes";
    let spans = ("signal", "spans");
    let v = api.ok(attrs, &[spans, ("key", "http.route"), ("prefix", "/P"), ("since", "1h")]).await;
    assert_eq!((values(&v), &v["total_rows"]), (vec!["/pay"], &json!(1)), "{v}");
    let v = api.ok(attrs, &[spans, ("prefix", "HTTP."), ("since", "1h")]).await;
    assert_eq!(keys(&v), ["http.route"], "{v}");
    let v = api.ok(attrs, &[spans, ("key", EVIL), ("prefix", EVIL), ("since", "1h")]).await;
    assert_eq!(v["total_rows"], 3, "{v}");
    let v = api.ok(attrs, &[spans, ("key", "http.route"), ("prefix", "' OR 1=1 --"), ("since", "1h")]).await;
    assert_eq!(v["total_rows"], 0, "{v}");
    // Spans still all there after all that.
    assert!(
        api.ok("/otel/api/v1/services", &[("since", "1h"), ("entry_only", "false")]).await.as_array().unwrap().len()
            >= 3
    );

    // service= once as before, repeated for an app with two names, hostile as data.
    let ops = "/otel/api/v1/operations";
    let since = ("since", "1h");
    let v = api.ok(ops, &[since, ("service", "worker")]).await;
    assert_eq!(services_of(&v), ["worker"]);
    let v = api.ok(ops, &[since, ("service", "worker"), ("service", "Checkout")]).await;
    assert_eq!(services_of(&v), ["Checkout", "worker"]);
    assert_eq!(services_of(&api.ok(ops, &[since, ("service", EVIL)]).await), [EVIL]);
    assert_eq!(services_of(&api.ok(ops, &[since, ("service", "' OR 1=1 --")]).await), Vec::<&str>::new());
    // Percent-encoded names are the same parameter.
    let r = http.get(format!("{}{ops}?since=1h&s%65rvice=worker&service=Check%6Fut", srv.url)).bearer_auth(TOKEN);
    let v: Value = r.send().await.unwrap().json().await.unwrap();
    assert_eq!(services_of(&v), ["Checkout", "worker"], "{v}");
    // Every endpoint that takes a service takes it twice.
    let two = [since, ("service", "worker"), ("service", "Checkout")];
    let with = |extra: (&'static str, &'static str)| -> Vec<(&'static str, &'static str)> {
        two.iter().copied().chain([extra]).collect()
    };
    let traces = api.ok("/otel/api/v1/traces", &two).await;
    assert_eq!(traces.as_array().unwrap().len(), 2, "{traces}");
    let series = api.ok("/otel/api/v1/spans/series", &two).await;
    assert_eq!(series["points"].as_array().unwrap().iter().map(|p| p["count"].as_u64().unwrap()).sum::<u64>(), 2);
    let v = api.ok(attrs, &with(spans)).await;
    assert_eq!(v["total_rows"], 2, "{v}");
    api.ok("/otel/api/v1/logs", &two).await;
    api.ok("/otel/api/v1/logs/series", &two).await;
    api.ok("/otel/api/v1/metrics", &two).await;
    let (s, v) = api.get("/otel/api/v1/metrics/query", &with(("name", "nope"))).await;
    assert_eq!(s, 404, "the service filter passed, the metric is unknown: {v}");

    srv.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_new_route_checks_token_and_input() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    client.sentry.create_project(&CreateProject { name: "Web".into(), slug: None, platform: None }).await.unwrap();
    let http = reqwest::Client::new();
    let api = Api { http: http.clone(), url: srv.url.clone() };
    let routes = [
        "/api/v1/apps",
        "/api/v1/find?id=85fede14250ec6be",
        "/sentry/v1/projects/web/tags",
        "/otel/api/v1/attributes?signal=spans&prefix=a",
        "/otel/api/v1/operations?service=a&service=b",
    ];
    for route in routes {
        let url = format!("{}{route}", srv.url);
        let r = http.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 401, "{route} without a token");
        assert_eq!(r.json::<Value>().await.unwrap()["error"], "unauthorized", "{route}");
        let r = http.get(&url).bearer_auth("wrong").send().await.unwrap();
        assert_eq!(r.status(), 401, "{route} with a wrong token");
    }

    let bad: &[(&str, &[(&str, &str)])] = &[
        ("/api/v1/apps", &[("since", "soon")]),
        ("/api/v1/apps", &[("since", "300000y")]),
        ("/api/v1/apps", &[("from", "yesterday")]),
        ("/api/v1/apps", &[("from", "2026-01-02T00:00:00Z"), ("to", "2026-01-01T00:00:00Z")]),
        ("/api/v1/find", &[]),
        ("/api/v1/find", &[("id", "")]),
        ("/api/v1/find", &[("id", "not-an-id")]),
        ("/api/v1/find", &[("id", "85fede14250ec6bz")]),
        ("/api/v1/find", &[("id", "' OR 1=1 --")]),
        ("/api/v1/find", &[("id", "85fede14250ec6be"), ("since", "soon")]),
        ("/sentry/v1/projects/web/tags", &[("since", "soon")]),
        ("/sentry/v1/projects/web/tags", &[("key", " ")]),
        ("/sentry/v1/projects/web/tags", &[("limit", "0")]),
        ("/sentry/v1/projects/web/tags", &[("limit", "many")]),
        ("/otel/api/v1/operations", &[("since", "soon"), ("service", "a"), ("service", "b")]),
        ("/otel/api/v1/traces", &[("limit", "many"), ("service", "a"), ("service", "b")]),
    ];
    for (path, query) in bad {
        let (status, v) = api.get(path, query).await;
        assert_eq!((status, v["error"].as_str()), (400, Some("bad_request")), "{path} {query:?}: {v}");
    }
    let many: Vec<(&str, &str)> = (0..51).map(|_| ("service", "a")).collect();
    let (status, v) = api.get("/otel/api/v1/operations", &many).await;
    assert_eq!(status, 400, "{v}");
    assert!(v["message"].as_str().unwrap().contains("at most 50"), "{v}");
    let (status, v) = api.get("/sentry/v1/projects/nope/tags", &[]).await;
    assert_eq!(status, 404, "{v}");

    // No match is a 404 with every source read.
    let (status, v) = api.get("/api/v1/find", &[("id", &"f".repeat(32))]).await;
    assert_eq!((status, v["error"].as_str()), (404, Some("not_found")), "{v}");

    // A limit above the most is the most, as on the attributes endpoint.
    api.ok("/sentry/v1/projects/web/tags", &[("limit", "100000")]).await;
    srv.stop().await;
}
