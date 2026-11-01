//! End-to-end: OTLP in through the HTTP router, data flushed to a real
//! DuckLake (DuckDB catalog + local files), queried through the API router.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use odl_api::*;
use odl_server::{Server, config::Config};
use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    common::v1::{AnyValue, KeyValue, any_value},
    logs::v1::{LogRecord as OtlpLog, ResourceLogs, ScopeLogs},
    metrics::v1::{
        AggregationTemporality, Exemplar, ExponentialHistogram, ExponentialHistogramDataPoint, Gauge, Histogram,
        HistogramDataPoint, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, Summary, SummaryDataPoint,
        exemplar, exponential_histogram_data_point::Buckets, metric::Data, number_data_point,
        summary_data_point::ValueAtQuantile,
    },
    resource::v1::Resource,
    trace::v1::{ResourceSpans, ScopeSpans, Span, Status, span},
};
use prost::Message;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn kv(k: &str, v: &str) -> KeyValue {
    KeyValue {
        key: k.into(),
        value: Some(AnyValue { value: Some(any_value::Value::StringValue(v.into())) }),
        ..Default::default()
    }
}

fn resource(service: &str) -> Option<Resource> {
    Some(Resource { attributes: vec![kv("service.name", service), kv("host.name", "h1")], ..Default::default() })
}

fn now_ns() -> u64 {
    chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64
}

const TRACE: [u8; 16] =
    [0x0a, 0xf7, 0x65, 0x19, 0x16, 0xcd, 0x43, 0xdd, 0x84, 0x48, 0xeb, 0x21, 0x1c, 0x80, 0x31, 0x9c];
const TRACE_HEX: &str = "0af7651916cd43dd8448eb211c80319c";

fn traces_req(t0: u64) -> ExportTraceServiceRequest {
    let root = Span {
        trace_id: TRACE.to_vec(),
        span_id: vec![0, 0, 0, 0, 0, 0, 0, 1],
        name: "GET /checkout".into(),
        kind: 2,
        start_time_unix_nano: t0,
        end_time_unix_nano: t0 + 120_000_000,
        attributes: vec![kv("http.route", "/checkout"), kv("http.response.status_code", "500")],
        status: Some(Status { code: 2, message: "boom".into() }),
        events: vec![span::Event {
            time_unix_nano: t0 + 1000,
            name: "exception".into(),
            attributes: vec![kv("exception.type", "Boom")],
            ..Default::default()
        }],
        links: vec![span::Link { trace_id: vec![7; 16], span_id: vec![0, 0, 0, 0, 0, 0, 0, 9], ..Default::default() }],
        ..Default::default()
    };
    let child = Span {
        trace_id: TRACE.to_vec(),
        span_id: vec![0xff, 0, 0, 0, 0, 0, 0, 2],
        parent_span_id: vec![0, 0, 0, 0, 0, 0, 0, 1],
        name: "SELECT orders".into(),
        kind: 3,
        start_time_unix_nano: t0 + 10_000_000,
        end_time_unix_nano: t0 + 60_000_000,
        ..Default::default()
    };
    let other_service = Span {
        trace_id: TRACE.to_vec(),
        span_id: vec![0, 0, 0, 0, 0, 0, 0, 3],
        parent_span_id: vec![0xff, 0, 0, 0, 0, 0, 0, 2],
        name: "query".into(),
        kind: 2,
        start_time_unix_nano: t0 + 20_000_000,
        end_time_unix_nano: t0 + 50_000_000,
        ..Default::default()
    };
    ExportTraceServiceRequest {
        resource_spans: vec![
            ResourceSpans {
                resource: resource("checkout"),
                scope_spans: vec![ScopeSpans { spans: vec![root, child], ..Default::default() }],
                ..Default::default()
            },
            ResourceSpans {
                resource: resource("db"),
                scope_spans: vec![ScopeSpans { spans: vec![other_service], ..Default::default() }],
                ..Default::default()
            },
        ],
    }
}

fn logs_req(t0: u64) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: resource("checkout"),
            scope_logs: vec![ScopeLogs {
                log_records: vec![
                    OtlpLog {
                        time_unix_nano: t0,
                        severity_number: 17,
                        severity_text: "ERROR".into(),
                        body: Some(AnyValue { value: Some(any_value::Value::StringValue("payment FAILED".into())) }),
                        trace_id: TRACE.to_vec(),
                        span_id: vec![0, 0, 0, 0, 0, 0, 0, 1],
                        attributes: vec![kv("order.id", "42")],
                        ..Default::default()
                    },
                    OtlpLog {
                        time_unix_nano: t0 + 1_000_000,
                        severity_number: 9,
                        body: Some(AnyValue { value: Some(any_value::Value::StringValue("all good".into())) }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

fn metrics_req(t0: u64, n: u64) -> ExportMetricsServiceRequest {
    let start = t0 - 600_000_000_000;
    let pt = |v: f64, t: u64, route: &str| NumberDataPoint {
        attributes: vec![kv("route", route)],
        start_time_unix_nano: start,
        time_unix_nano: t,
        value: Some(number_data_point::Value::AsDouble(v)),
        ..Default::default()
    };
    let mut gauge_pts = vec![];
    let mut counter_pts = vec![];
    let mut hist_pts = vec![];
    for i in 0..n {
        let t = t0 - (n - 1 - i) * 60_000_000_000;
        gauge_pts.push(pt(10.0 + i as f64, t, "/a"));
        // 100 requests per minute -> rate 100/60 per second
        counter_pts.push(pt(100.0 * (i + 1) as f64, t, "/a"));
        counter_pts.push(pt(50.0 * (i + 1) as f64, t, "/b"));
        hist_pts.push(HistogramDataPoint {
            attributes: vec![kv("route", "/a")],
            start_time_unix_nano: start,
            time_unix_nano: t,
            count: 100 * (i + 1),
            sum: Some(1000.0 * (i + 1) as f64),
            // each minute: 50 in (0,10], 50 in (10,20]
            bucket_counts: vec![50 * (i + 1), 50 * (i + 1), 0],
            explicit_bounds: vec![10.0, 20.0],
            ..Default::default()
        });
    }
    let metric = |name: &str, data: Data| Metric {
        name: name.into(),
        unit: "1".into(),
        description: format!("{name} description"),
        data: Some(data),
        ..Default::default()
    };
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource("checkout"),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![
                    metric("queue.size", Data::Gauge(Gauge { data_points: gauge_pts })),
                    metric(
                        "http.requests",
                        Data::Sum(Sum {
                            data_points: counter_pts,
                            aggregation_temporality: AggregationTemporality::Cumulative as i32,
                            is_monotonic: true,
                        }),
                    ),
                    metric(
                        "http.duration",
                        Data::Histogram(Histogram {
                            data_points: hist_pts,
                            aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        }),
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

struct Harness {
    otlp: Router,
    api: Router,
    shutdown: CancellationToken,
    bg: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

async fn harness(token: Option<&str>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.storage.catalog = dir.path().join("catalog.ducklake").to_string_lossy().into();
    cfg.storage.data_path = format!("{}/", dir.path().join("lake").to_string_lossy());
    cfg.storage.data_inlining_row_limit = 0;
    cfg.ingest.flush_interval = Duration::from_millis(100);
    cfg.maintenance.enabled = false;
    if let Some(t) = token {
        cfg.auth.api_tokens = vec![t.into()];
        cfg.query.enable_sql = true;
    }
    let server = Arc::new(Server::new(cfg).await.unwrap());
    let shutdown = CancellationToken::new();
    let s2 = server.clone();
    let sd = shutdown.clone();
    let bg = tokio::spawn(async move { s2.run_background(async move { sd.cancelled().await }).await.unwrap() });
    Harness { otlp: server.otlp_http_router(), api: server.api_router(), shutdown, bg, _dir: dir }
}

impl Harness {
    /// GET a list and its `x-next-cursor` header.
    async fn page<T: serde::de::DeserializeOwned>(&self, path: &str) -> (Vec<T>, Option<String>) {
        let resp = self.api.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "GET {path}");
        let cursor = resp.headers().get(NEXT_CURSOR_HEADER).map(|v| v.to_str().unwrap().to_string());
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (serde_json::from_slice(&body).unwrap(), cursor)
    }

    /// Every page of a list, following the cursor (`path` ends in a query).
    async fn all_pages<T: serde::de::DeserializeOwned>(&self, path: &str) -> Vec<Vec<T>> {
        let mut pages = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let p = match &cursor {
                Some(c) => format!("{path}&cursor={}", c.replace(':', "%3A")),
                None => path.to_string(),
            };
            let (items, next) = self.page(&p).await;
            pages.push(items);
            match next {
                Some(c) => cursor = Some(c),
                None => return pages,
            }
            assert!(pages.len() < 100, "cursor loop");
        }
    }

    async fn post(&self, path: &str, ct: &str, body: Vec<u8>) -> (StatusCode, Vec<u8>) {
        let req = Request::post(path).header(header::CONTENT_TYPE, ct).body(Body::from(body)).unwrap();
        let resp = self.otlp.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        (status, to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec())
    }

    async fn get_raw(&self, path: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut b = Request::get(path);
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = self.api.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        (status, String::from_utf8(to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap())
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> T {
        let (s, body) = self.get_raw(path, None).await;
        assert_eq!(s, StatusCode::OK, "GET {path}: {body}");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("GET {path}: {e}: {body}"))
    }

    /// Poll until `f` returns Some or time out.
    async fn eventually<T, F, Fut>(&self, f: F) -> T
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        for _ in 0..100 {
            if let Some(v) = f().await {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("condition not met in time");
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.bg.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn traces_roundtrip() {
    let h = harness(None).await;
    let t0 = now_ns() - 5_000_000_000;
    let (s, _) = h.post("/v1/traces", "application/x-protobuf", traces_req(t0).encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);

    let trace: Trace = h
        .eventually(|| async {
            let (s, body) = h.get_raw(&format!("/api/v1/traces/{TRACE_HEX}?since=1h"), None).await;
            if s != StatusCode::OK && s != StatusCode::NOT_FOUND {
                panic!("{s}: {body}");
            }
            (s == StatusCode::OK).then(|| serde_json::from_str(&body).unwrap())
        })
        .await;
    assert_eq!(trace.trace_id, TRACE_HEX);
    assert_eq!(trace.spans.len(), 3);
    let root = &trace.spans[0];
    assert_eq!(root.span_id, "0000000000000001");
    assert_eq!(root.parent_span_id, None);
    assert_eq!(root.service_name, "checkout");
    assert_eq!(root.kind, SpanKind::Server);
    assert_eq!(root.status_code, SpanStatus::Error);
    assert_eq!(root.duration_ns, 120_000_000);
    assert_eq!(root.attributes["http.route"], "/checkout");
    assert_eq!(root.resource_attributes["host.name"], "h1");
    assert_eq!(root.events.len(), 1);
    assert_eq!(root.events[0].attributes["exception.type"], "Boom");
    assert_eq!(root.links[0].trace_id, "07070707070707070707070707070707");
    assert_eq!(root.links[0].span_id, "0000000000000009");
    let child = trace.spans.iter().find(|s| s.name == "SELECT orders").unwrap();
    assert_eq!(child.span_id, "ff00000000000002", "high bit must survive UBIGINT");
    assert_eq!(child.parent_span_id.as_deref(), Some("0000000000000001"));

    let services: Vec<ServiceSummary> = h.get("/api/v1/services?since=1h").await;
    let checkout = services.iter().find(|s| s.name == "checkout").unwrap();
    assert_eq!(checkout.red.span_count, 1, "entry spans only");
    assert_eq!(checkout.red.error_count, 1);
    assert!(services.iter().any(|s| s.name == "db"));

    let ops: Vec<OperationSummary> = h.get("/api/v1/operations?since=1h&service=checkout&entry_only=false").await;
    assert_eq!(ops.len(), 2);

    let found: Vec<TraceSummary> = h.get("/api/v1/traces?since=1h&service=db").await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].trace_id, TRACE_HEX);
    assert_eq!(found[0].span_count, 3);
    assert_eq!(found[0].root_service.as_deref(), Some("checkout"));
    assert_eq!(found[0].services, vec!["checkout".to_string(), "db".to_string()]);
    assert!((found[0].duration_ms - 120.0).abs() < 0.01);

    let none: Vec<TraceSummary> = h.get("/api/v1/traces?since=1h&min_duration_ms=500").await;
    assert!(none.is_empty());
    let by_attr: Vec<TraceSummary> = h.get("/api/v1/traces?since=1h&attributes=http.route%3D/checkout").await;
    assert_eq!(by_attr.len(), 1);
    let errs: Vec<TraceSummary> = h.get("/api/v1/traces?since=1h&errors_only=true&limit=5").await;
    assert_eq!(errs.len(), 1);

    let series: SpanSeries = h.get("/api/v1/spans/series?since=1h&service=checkout").await;
    assert_eq!(series.points.iter().map(|p| p.count).sum::<u64>(), 1);

    // A span by its id; the high bit survives the round trip.
    let span: odl_api::Span = h.get("/api/v1/spans/ff00000000000002?since=1h").await;
    assert_eq!((span.trace_id.as_str(), span.name.as_str()), (TRACE_HEX, "SELECT orders"));
    assert_eq!(span.parent_span_id.as_deref(), Some("0000000000000001"));
    let span: odl_api::Span = h.get("/api/v1/spans/0000000000000003").await;
    assert_eq!(span.service_name, "db", "default window: a week");
    let (s, _) = h.get_raw("/api/v1/spans/00000000000000ab?since=1h", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h.get_raw("/api/v1/spans/xyz?since=1h", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, _) = h.get_raw("/api/v1/traces/nothex?since=1h", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.get_raw("/api/v1/traces/00000000000000000000000000000001?since=1h", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h.get_raw("/api/v1/traces?since=99999d", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn json_logs_and_rejections() {
    let h = harness(None).await;
    let t0 = now_ns() - 1_000_000_000;
    // OTLP/JSON encoding (hex ids, camelCase).
    let json = serde_json::to_vec(&logs_req(t0)).unwrap();
    let (s, body) = h.post("/v1/logs", "application/json", json).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let logs: Vec<LogRecord> = h
        .eventually(|| async {
            let v: Vec<LogRecord> = h.get("/api/v1/logs?since=1h").await;
            (v.len() == 2).then_some(v)
        })
        .await;
    assert_eq!(logs[0].body, "all good", "newest first");
    let errs: Vec<LogRecord> = h.get("/api/v1/logs?since=1h&min_severity=17").await;
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].trace_id.as_deref(), Some(TRACE_HEX));
    assert_eq!(errs[0].span_id.as_deref(), Some("0000000000000001"));
    assert_eq!(errs[0].attributes["order.id"], "42");
    let q: Vec<LogRecord> = h.get("/api/v1/logs?since=1h&q=failed").await;
    assert_eq!(q.len(), 1, "case-insensitive substring");
    let pct: Vec<LogRecord> = h.get("/api/v1/logs?since=1h&q=%25").await;
    assert!(pct.is_empty(), "% is not a wildcard");
    let by_trace: Vec<LogRecord> = h.get(&format!("/api/v1/logs?since=1h&trace_id={TRACE_HEX}")).await;
    assert_eq!(by_trace.len(), 1);
    let series: LogSeries = h.get("/api/v1/logs/series?since=1h").await;
    let total: u64 = series.points.iter().map(|p| p.total).sum();
    let errors: u64 = series.points.iter().map(|p| p.error).sum();
    assert_eq!((total, errors), (2, 1));

    // Invalid payloads and media types.
    let (s, _) = h.post("/v1/logs", "application/x-protobuf", vec![0xff, 0xff, 0xff]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.post("/v1/logs", "text/plain", vec![]).await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // Spans with invalid ids are reported as partial success.
    let mut bad = traces_req(t0);
    bad.resource_spans[0].scope_spans[0].spans[0].trace_id = vec![1, 2, 3];
    let (s, body) = h.post("/v1/traces", "application/json", serde_json::to_vec(&bad).unwrap()).await;
    assert_eq!(s, StatusCode::OK);
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("rejectedSpans") || body.contains("rejected_spans"), "{body}");
    h.stop().await;
}

/// Protojson writes NaN and infinities as strings; they used to fail the
/// whole request with 400 (otel issue 0004).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn json_non_finite_doubles_are_accepted() {
    let h = harness(None).await;
    let t = now_ns() - 1_000_000_000;
    let logs = format!(
        r#"{{"resourceLogs":[{{"resource":{{"attributes":[{{"key":"service.name","value":{{"stringValue":"nan"}}}}]}},
          "scopeLogs":[{{"logRecords":[{{"timeUnixNano":"{t}","body":{{"doubleValue":"NaN"}},
            "attributes":[{{"key":"inf","value":{{"doubleValue":"Infinity"}}}},
              {{"key":"arr","value":{{"arrayValue":{{"values":[{{"doubleValue":"-Infinity"}},{{"doubleValue":1.5}}]}}}}}}]}}]}}]}}]}}"#
    );
    let (s, body) = h.post("/v1/logs", "application/json", logs.into_bytes()).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let metrics = format!(
        r#"{{"resourceMetrics":[{{"scopeMetrics":[{{"metrics":[{{"name":"nan.gauge",
          "gauge":{{"dataPoints":[{{"timeUnixNano":"{t}","asDouble":"NaN",
            "attributes":[{{"key":"k","value":{{"doubleValue":"Infinity"}}}}]}}]}}}}]}}]}}]}}"#
    );
    let (s, body) = h.post("/v1/metrics", "application/json", metrics.into_bytes()).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let logs: Vec<LogRecord> = h
        .eventually(|| async {
            let v: Vec<LogRecord> = h.get("/api/v1/logs?since=1h&service=nan").await;
            (v.len() == 1).then_some(v)
        })
        .await;
    assert_eq!(logs[0].body, "NaN");
    assert_eq!(logs[0].attributes["inf"], "Infinity");
    assert_eq!(logs[0].attributes["arr"], r#"["-Infinity",1.5]"#);
    h.eventually(|| async {
        let v: Vec<MetricInfo> = h.get("/api/v1/metrics?since=1h").await;
        v.iter().any(|m| m.name == "nan.gauge").then_some(())
    })
    .await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metrics_queries() {
    let h = harness(None).await;
    let t0 = now_ns() - 1_000_000_000;
    let (s, _) = h.post("/v1/metrics", "application/x-protobuf", metrics_req(t0, 10).encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);

    let list: Vec<MetricInfo> = h
        .eventually(|| async {
            let v: Vec<MetricInfo> = h.get("/api/v1/metrics?since=1h").await;
            (v.len() == 3).then_some(v)
        })
        .await;
    let counter = list.iter().find(|m| m.name == "http.requests").unwrap();
    assert_eq!(counter.metric_type, MetricType::Sum);
    assert_eq!(counter.series_count, 2);
    assert_eq!(counter.temporality, Temporality::Cumulative);
    assert!(list.iter().any(|m| m.metric_type == MetricType::Histogram));

    // Counter rate per route: /a grows 100/min, /b 50/min.
    let r: MetricQueryResult = h.get("/api/v1/metrics/query?name=http.requests&since=15m&step=1m&group_by=route").await;
    assert_eq!(r.agg, "rate");
    assert_eq!(r.series.len(), 2);
    let a = r.series.iter().find(|s| s.labels["route"] == "/a").unwrap();
    assert!(!a.points.is_empty());
    for p in &a.points {
        assert!((p.value - 100.0 / 60.0).abs() < 1e-6, "rate {}", p.value);
    }
    let total: MetricQueryResult =
        h.get("/api/v1/metrics/query?name=http.requests&since=15m&step=1m&agg=increase").await;
    assert_eq!(total.series.len(), 1);
    assert!(total.series[0].points.iter().all(|p| (p.value - 150.0).abs() < 1e-6));

    let g: MetricQueryResult = h.get("/api/v1/metrics/query?name=queue.size&since=15m&step=1m&agg=max").await;
    assert_eq!(g.series[0].points.last().unwrap().value, 19.0);

    // Histogram: each minute 50 values in (0,10] and 50 in (10,20] -> p50=10, p75=15.
    let p: MetricQueryResult = h.get("/api/v1/metrics/query?name=http.duration&since=15m&step=1m&agg=p75").await;
    assert!(!p.series[0].points.is_empty());
    for pt in &p.series[0].points {
        assert!((pt.value - 15.0).abs() < 1e-6, "p75 {}", pt.value);
    }
    let avg: MetricQueryResult = h.get("/api/v1/metrics/query?name=http.duration&since=15m&step=1m&agg=avg").await;
    assert!(avg.series[0].points.iter().all(|p| (p.value - 10.0).abs() < 1e-6));

    let (s, _) = h.get_raw("/api/v1/metrics/query?name=queue.size&since=15m&agg=p95", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.get_raw("/api/v1/metrics/query?name=nope&since=15m", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_sql_stats_and_maintenance() {
    let h = harness(Some("s3cret")).await;
    let t0 = now_ns() - 1_000_000_000;
    for i in 0..5 {
        let (s, _) = h.post("/v1/traces", "application/x-protobuf", traces_req(t0 + i).encode_to_vec()).await;
        assert_eq!(s, StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let (s, _) = h.get_raw("/api/v1/services", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h.get_raw("/api/v1/health", None).await;
    assert_eq!(s, StatusCode::OK, "health needs no token");
    let (s, body) = h.get_raw("/openapi.json", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("/api/v1/traces/{trace_id}"));

    // Stats: several flushes -> several files.
    let stats: odl_api::Stats = h
        .eventually(|| async {
            let (_, body) = h.get_raw("/api/v1/stats", Some("s3cret")).await;
            let st: odl_api::Stats = serde_json::from_str(&body).unwrap();
            let spans = st.storage.tables.iter().find(|t| t.name == "spans").unwrap().rows;
            (spans == 15).then_some(st)
        })
        .await;
    let spans = stats.storage.tables.iter().find(|t| t.name == "spans").unwrap();
    assert!(spans.files >= 2, "expected several small files, got {}", spans.files);
    assert_eq!(stats.ingest.signals[0].accepted, 15);
    let caps = stats.capabilities.expect("capabilities");
    assert!(caps.sql && !caps.ingest_auth, "{caps:?}");
    assert_eq!(caps.flush_interval_secs, 0.1);
    assert_eq!(caps.max_window_secs, Config::default().query.max_window.as_secs());
    assert_eq!(caps.retention_secs.traces, Config::default().retention.traces.as_secs());

    // SQL endpoint
    let sql = |q: &str| {
        let body = serde_json::to_vec(&SqlRequest { query: q.into(), limit: Some(2) }).unwrap();
        Request::post("/api/v1/sql")
            .header(header::AUTHORIZATION, "Bearer s3cret")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    let resp = h
        .api
        .clone()
        .oneshot(sql("SELECT service_name, count(*) AS n FROM spans GROUP BY 1 ORDER BY 1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: SqlResponse = serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(r.columns[0].name, "service_name");
    assert_eq!(r.rows[0], vec![serde_json::json!("checkout"), serde_json::json!(10)]);
    let resp = h.api.clone().oneshot(sql("SELECT * FROM read_text('/etc/passwd')")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = h.api.clone().oneshot(sql("SELECT 1 AS x -- trailing comment")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "trailing comments must not break the wrapper");
    let resp = h.api.clone().oneshot(sql("SELECT no_such_column FROM spans")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "user SQL errors are 400");
    let e: ApiError = serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(!e.message.contains("DESCRIBE"), "wrapper not leaked: {}", e.message);
    let (s, body) = h.get_raw("/api/v1/logs?min_severity=300", Some("s3cret")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let e: ApiError = serde_json::from_str(&body).expect("JSON error body for bad query params");
    assert_eq!(e.error, "bad_request");
    let resp = h.api.clone().oneshot(sql("SELECT * FROM range(10)")).await.unwrap();
    let r: SqlResponse = serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(r.truncated);

    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compaction_and_retention() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.storage.catalog = dir.path().join("c.ducklake").to_string_lossy().into();
    cfg.storage.data_path = format!("{}/", dir.path().join("lake").to_string_lossy());
    cfg.storage.data_inlining_row_limit = 0;
    // Of `spans`: with inlining off, the lease row in `meta` has a file too.
    let files = |c: &mallard_duck::duckdb::Connection| -> i64 {
        c.query_row(
            "SELECT count(*) FROM __ducklake_metadata_lake.ducklake_data_file f \
             JOIN __ducklake_metadata_lake.ducklake_table t USING (table_id) \
             WHERE f.end_snapshot IS NULL AND t.end_snapshot IS NULL AND t.table_name = 'spans'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    {
        let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
        // Creates the tables.
        drop(Server::with_connection(cfg.clone(), conn.try_clone().unwrap()).await.unwrap());
        // Write 6 small batches directly: ten days ago and now.
        let old = now_ns() - 10 * 86_400_000_000_000;
        for (i, t) in [old, old + 1, now_ns(), now_ns(), now_ns(), now_ns()].iter().enumerate() {
            let mut req = traces_req(*t);
            for rs in &mut req.resource_spans {
                for ss in &mut rs.scope_spans {
                    for s in &mut ss.spans {
                        s.span_id[0] = i as u8 + 1;
                    }
                }
            }
            let tx = conn.unchecked_transaction().unwrap();
            let mut app = conn.appender_to_catalog_and_db("spans", "lake", "otel").unwrap();
            for b in odl_server::__convert_traces(&req) {
                app.append_record_batch(b).unwrap();
            }
            app.flush().unwrap();
            drop(app);
            tx.commit().unwrap();
        }
        assert_eq!(files(&conn), 6);
    }
    // The DuckDB-file catalog is free again: run maintenance as the CLI does.
    let c = cfg.clone();
    let done = tokio::task::spawn_blocking(move || odl_server::run_maintenance(&c, false)).await.unwrap().unwrap();
    assert!(done[0].contains("merged"), "{done:?}");
    assert!(done[1].contains("deleted 6 expired rows"), "{done:?}");
    let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
    assert_eq!(files(&conn), 1, "one file per day partition after merge; the old day is gone");
    let n: i64 = conn.query_row("SELECT count(*) FROM lake.otel.spans", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 12);
    let deletes: i64 =
        conn.query_row("SELECT count(*) FROM __ducklake_metadata_lake.ducklake_delete_file", [], |r| r.get(0)).unwrap();
    assert_eq!(deletes, 0, "day-aligned retention drops whole files");
}

/// A newer version added a column (rolling upgrade): this version must still
/// start, write and read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tolerates_columns_added_by_newer_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.storage.catalog = dir.path().join("c.ducklake").to_string_lossy().into();
    cfg.storage.data_path = format!("{}/", dir.path().join("lake").to_string_lossy());
    cfg.ingest.flush_interval = Duration::from_millis(100);
    cfg.maintenance.enabled = false;
    {
        let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
        drop(Server::with_connection(cfg.clone(), conn.try_clone().unwrap()).await.unwrap());
        conn.execute_batch("ALTER TABLE lake.otel.spans ADD COLUMN future_column VARCHAR").unwrap();
    }
    let server = Arc::new(Server::new(cfg).await.unwrap());
    let shutdown = CancellationToken::new();
    let (s2, sd) = (server.clone(), shutdown.clone());
    let bg = tokio::spawn(async move { s2.run_background(async move { sd.cancelled().await }).await.unwrap() });
    let req = Request::post("/v1/traces")
        .header(header::CONTENT_TYPE, "application/x-protobuf")
        .body(Body::from(traces_req(now_ns() - 1_000_000_000).encode_to_vec()))
        .unwrap();
    assert_eq!(server.otlp_http_router().oneshot(req).await.unwrap().status(), StatusCode::OK);
    let api = server.api_router();
    let mut found = false;
    for _ in 0..50 {
        let resp = api
            .clone()
            .oneshot(Request::get(format!("/api/v1/traces/{TRACE_HEX}?since=1h")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        if resp.status() == StatusCode::OK {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "spans written despite the extra column");
    shutdown.cancel();
    bg.await.unwrap();
}

/// One histogram series (route /a) with exemplars on two points a minute
/// apart, and a counter with one exemplar.
fn exemplars_req(t0: u64) -> ExportMetricsServiceRequest {
    let ex = |value: f64, trace: Option<[u8; 16]>, span: u64| Exemplar {
        // zero time: the point's time
        time_unix_nano: 0,
        value: Some(exemplar::Value::AsDouble(value)),
        trace_id: trace.map(|t| t.to_vec()).unwrap_or_default(),
        span_id: span.to_be_bytes().to_vec(),
        filtered_attributes: vec![kv("user.id", "u7")],
    };
    let mut other = TRACE;
    other[15] = 0x01;
    let hist = |t: u64, count: u64, exemplars: Vec<Exemplar>| HistogramDataPoint {
        attributes: vec![kv("route", "/a")],
        start_time_unix_nano: t0 - 600_000_000_000,
        time_unix_nano: t,
        count,
        sum: Some(count as f64),
        bucket_counts: vec![count, 0],
        explicit_bounds: vec![10.0],
        exemplars,
        ..Default::default()
    };
    let metric = |name: &str, data: Data| Metric { name: name.into(), data: Some(data), ..Default::default() };
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource("checkout"),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![
                    metric(
                        "http.duration",
                        Data::Histogram(Histogram {
                            data_points: vec![
                                hist(t0 - 60_000_000_000, 1, vec![ex(3.0, Some(TRACE), 1)]),
                                // The highest value with a trace wins; 100 has no trace.
                                hist(t0, 3, vec![ex(5.0, Some(TRACE), 1), ex(9.0, Some(other), 2), ex(100.0, None, 3)]),
                            ],
                            aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        }),
                    ),
                    metric(
                        "http.requests",
                        Data::Sum(Sum {
                            data_points: vec![NumberDataPoint {
                                time_unix_nano: t0,
                                value: Some(number_data_point::Value::AsInt(1)),
                                exemplars: vec![ex(1.0, Some(TRACE), 1)],
                                ..Default::default()
                            }],
                            aggregation_temporality: AggregationTemporality::Cumulative as i32,
                            is_monotonic: true,
                        }),
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

/// Exemplars are stored per point and returned by `metrics/query` on request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exemplars_link_metrics_to_traces() {
    let h = harness(None).await;
    let t0 = now_ns() - 1_000_000_000;
    let (s, _) = h.post("/v1/metrics", "application/x-protobuf", exemplars_req(t0).encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);
    h.eventually(|| async {
        let v: Vec<MetricInfo> = h.get("/api/v1/metrics?since=1h").await;
        (v.len() == 2).then_some(())
    })
    .await;
    let q = "/api/v1/metrics/query?name=http.duration&since=15m&step=1m&group_by=route";
    let plain: MetricQueryResult = h.get(q).await;
    assert!(plain.exemplars.is_empty(), "only on request");
    let r: MetricQueryResult = h.get(&format!("{q}&exemplars=true")).await;
    let got: Vec<(f64, &str, Option<&str>)> =
        r.exemplars.iter().map(|e| (e.value, e.trace_id.as_str(), e.span_id.as_deref())).collect();
    assert_eq!(
        got,
        vec![
            (3.0, TRACE_HEX, Some("0000000000000001")),
            (9.0, "0af7651916cd43dd8448eb211c803101", Some("0000000000000002"))
        ]
    );
    let e = &r.exemplars[1];
    assert_eq!(e.labels["route"], "/a");
    assert_eq!(e.attributes["user.id"], "u7");
    assert_eq!(e.time.timestamp_micros(), (t0 / 1000) as i64);
    // Filters apply to exemplars too.
    let none: MetricQueryResult = h.get(&format!("{q}&exemplars=true&attributes=route=/b")).await;
    assert!(none.exemplars.is_empty());
    let c: MetricQueryResult =
        h.get("/api/v1/metrics/query?name=http.requests&since=15m&step=1m&agg=increase&exemplars=true").await;
    assert_eq!(c.exemplars.len(), 1);
    assert!(c.exemplars[0].labels.is_empty());
    h.stop().await;
}

/// Exemplars of delta and cumulative series in one metric: each series keeps
/// its own, and the rates next to them still follow each series' temporality.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exemplars_in_a_metric_with_mixed_temporality() {
    let h = harness(None).await;
    let t0 = now_ns() - 1_000_000_000;
    let minute = 60_000_000_000;
    let ex = |value: f64, span: u64| Exemplar {
        value: Some(exemplar::Value::AsDouble(value)),
        trace_id: TRACE.to_vec(),
        span_id: span.to_be_bytes().to_vec(),
        ..Default::default()
    };
    let num = |src: &str, t: u64, v: i64, exemplars: Vec<Exemplar>| NumberDataPoint {
        attributes: vec![kv("src", src)],
        start_time_unix_nano: t0 - 10 * minute,
        time_unix_nano: t,
        value: Some(number_data_point::Value::AsInt(v)),
        exemplars,
        ..Default::default()
    };
    let hist = |src: &str, t: u64, n: u64, exemplars: Vec<Exemplar>| HistogramDataPoint {
        attributes: vec![kv("src", src)],
        start_time_unix_nano: t0 - 10 * minute,
        time_unix_nano: t,
        count: n,
        bucket_counts: vec![n, 0],
        explicit_bounds: vec![10.0],
        exemplars,
        ..Default::default()
    };
    let sum = |data_points, t: AggregationTemporality| {
        Data::Sum(Sum { data_points, aggregation_temporality: t as i32, is_monotonic: true })
    };
    let histogram = |data_points, t: AggregationTemporality| {
        Data::Histogram(Histogram { data_points, aggregation_temporality: t as i32 })
    };
    let metric = |name: &str, data: Data| Metric { name: name.into(), data: Some(data), ..Default::default() };
    use AggregationTemporality::{Cumulative, Delta};
    let req = ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource("checkout"),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![
                    // 30 per minute as deltas, 60 per minute cumulative.
                    metric(
                        "jobs",
                        sum(vec![num("delta", t0 - minute, 30, vec![]), num("delta", t0, 30, vec![ex(2.0, 1)])], Delta),
                    ),
                    metric(
                        "jobs",
                        sum(
                            vec![
                                num("cumulative", t0 - minute, 60, vec![]),
                                num("cumulative", t0, 120, vec![ex(7.0, 2)]),
                            ],
                            Cumulative,
                        ),
                    ),
                    metric(
                        "latency",
                        histogram(
                            vec![hist("delta", t0 - minute, 10, vec![]), hist("delta", t0, 10, vec![ex(3.0, 3)])],
                            Delta,
                        ),
                    ),
                    metric(
                        "latency",
                        histogram(
                            vec![
                                hist("cumulative", t0 - minute, 10, vec![]),
                                hist("cumulative", t0, 20, vec![ex(8.0, 4)]),
                            ],
                            Cumulative,
                        ),
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let (s, _) = h.post("/v1/metrics", "application/x-protobuf", req.encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);
    h.eventually(|| async {
        let v: Vec<MetricInfo> = h.get("/api/v1/metrics?since=1h").await;
        (v.len() == 2).then_some(())
    })
    .await;
    for (name, agg, delta, cumulative, last_rate) in
        [("jobs", "rate", 2.0, 7.0, (0.5, 1.0)), ("latency", "rate", 3.0, 8.0, (10.0 / 60.0, 10.0 / 60.0))]
    {
        let q = format!("/api/v1/metrics/query?name={name}&since=15m&step=1m&agg={agg}&exemplars=true");
        let by: MetricQueryResult = h.get(&format!("{q}&group_by=src")).await;
        let of = |src: &str| {
            let e: Vec<_> = by.exemplars.iter().filter(|e| e.labels["src"] == src).map(|e| e.value).collect();
            let series = by.series.iter().find(|s| s.labels["src"] == src).expect(src);
            (e, series.points.last().unwrap().value)
        };
        let (e, rate) = of("delta");
        assert_eq!(e, vec![delta], "{name}");
        assert!((rate - last_rate.0).abs() < 1e-6, "{name} delta rate {rate}");
        let (e, rate) = of("cumulative");
        assert_eq!(e, vec![cumulative], "{name}");
        assert!((rate - last_rate.1).abs() < 1e-6, "{name} cumulative rate {rate}");
        // Not grouped: one series, so the highest of both in the bucket.
        let all: MetricQueryResult = h.get(&q).await;
        let got: Vec<f64> = all.exemplars.iter().map(|e| e.value).collect();
        assert_eq!(got, vec![cumulative], "{name}");
    }
    h.stop().await;
}

/// A catalog created before exemplars were stored: start-up adds the
/// column, old rows read as no exemplars, and new rows keep theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_adds_exemplars_to_an_old_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = lake_config(dir.path());
    let t0 = now_ns() - 1_000_000_000;
    {
        // metric_points as created by versions without exemplars, with one row.
        let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
        conn.execute_batch(
            "CREATE SCHEMA lake.otel;
             CREATE TABLE lake.otel.metric_points (
                time TIMESTAMP, start_time TIMESTAMP, service_name VARCHAR, metric_name VARCHAR,
                metric_description VARCHAR, metric_unit VARCHAR, temporality VARCHAR, series_id UBIGINT,
                flags UINTEGER, scope_name VARCHAR, scope_version VARCHAR,
                resource_attributes MAP(VARCHAR, VARCHAR), attributes MAP(VARCHAR, VARCHAR),
                metric_type VARCHAR, is_monotonic BOOLEAN, value DOUBLE)
             PARTITIONED BY (epoch_day(time)) SORTED BY (metric_name, series_id, time);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lake.otel.metric_points VALUES (make_timestamp(?) - INTERVAL 2 MINUTE, NULL, 'checkout', \
             'http.requests', '', '', 'cumulative', 42, 0, '', '', MAP {}, MAP {}, 'sum', true, 0)",
            [(t0 / 1000) as i64],
        )
        .unwrap();
    }
    let server = Arc::new(Server::new(cfg.clone()).await.unwrap());
    let shutdown = CancellationToken::new();
    let (s2, sd) = (server.clone(), shutdown.clone());
    let bg = tokio::spawn(async move { s2.run_background(async move { sd.cancelled().await }).await.unwrap() });
    let req = Request::post("/v1/metrics")
        .header(header::CONTENT_TYPE, "application/x-protobuf")
        .body(Body::from(exemplars_req(t0).encode_to_vec()))
        .unwrap();
    assert_eq!(server.otlp_http_router().oneshot(req).await.unwrap().status(), StatusCode::OK);
    let api = server.api_router();
    let path = "/api/v1/metrics/query?name=http.requests&since=15m&step=1m&agg=max&exemplars=true";
    let mut result = None;
    for _ in 0..50 {
        let resp = api.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        // Points and exemplars are separate reads, so a flush between them can
        // give the new exemplar without the new point: wait for both.
        if let Ok(r) = serde_json::from_slice::<MetricQueryResult>(&body)
            && !r.exemplars.is_empty()
            && r.series.first().is_some_and(|s| s.points.len() >= 2)
        {
            result = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let r = result.expect("new rows with exemplars are written to the old table");
    assert_eq!(r.exemplars.len(), 1);
    let points: Vec<f64> = r.series[0].points.iter().map(|p| p.value).collect();
    assert_eq!(points, vec![0.0, 1.0], "the old row is still read");
    shutdown.cancel();
    bg.await.unwrap();
    // Compaction copes with files from before and after the new column.
    drop(api);
    drop(server);
    let c = cfg.clone();
    tokio::task::spawn_blocking(move || odl_server::run_maintenance(&c, false)).await.unwrap().unwrap();
    let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
    let got: (i64, i64) = conn
        .query_row(
            "SELECT count(*), sum(len(exemplars))::BIGINT FROM lake.otel.metric_points WHERE metric_name = 'http.requests'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(got, (2, 1));
}

/// A tiny request that would expand enormously is refused before allocating.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refuses_expansion_bombs() {
    let h = harness(None).await;
    let mut req = traces_req(now_ns());
    let big = "x".repeat(64 * 1024);
    let span = req.resource_spans[0].scope_spans[0].spans[0].clone();
    req.resource_spans[0].resource.as_mut().unwrap().attributes.push(kv("big", &big));
    req.resource_spans[0].scope_spans[0].spans = (0..20_000)
        .map(|i| {
            let mut s = span.clone();
            s.span_id = (i as u64 + 1).to_be_bytes().to_vec();
            s.events.clear();
            s.attributes.clear();
            s
        })
        .collect();
    let body = req.encode_to_vec();
    assert!(body.len() < 32_000_000);
    let (s, _) = h.post("/v1/traces", "application/x-protobuf", body).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    h.stop().await;
}

/// If the background jobs cannot start, `serve` must stop the receivers and
/// return the error instead of accepting data that is never flushed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_stops_when_background_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.storage.catalog = dir.path().join("c.ducklake").to_string_lossy().into();
    cfg.storage.data_path = format!("{}/", dir.path().join("lake").to_string_lossy());
    cfg.server.otlp_grpc_addr = "127.0.0.1:0".into();
    cfg.server.otlp_http_addr = "127.0.0.1:0".into();
    cfg.server.api_addr = "127.0.0.1:0".into();
    let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
    let server = Server::with_connection(cfg, conn.try_clone().unwrap()).await.unwrap();
    // New connections can no longer select the lake schema, so the flusher fails to start.
    conn.execute_batch("DETACH lake").unwrap();
    let r = tokio::time::timeout(Duration::from_secs(20), server.serve(CancellationToken::new()))
        .await
        .expect("serve must return when the background jobs fail");
    assert!(r.is_err());
}

fn lake_config(dir: &std::path::Path) -> Config {
    let mut cfg = Config::default();
    cfg.storage.catalog = dir.join("c.ducklake").to_string_lossy().into();
    cfg.storage.data_path = format!("{}/", dir.join("lake").to_string_lossy());
    cfg.storage.data_inlining_row_limit = 0;
    cfg.ingest.flush_interval = Duration::from_millis(100);
    cfg.maintenance.enabled = false;
    cfg
}

/// The token is checked before the body is read: a client without a token
/// cannot make the server buffer a body.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_ingest_checks_the_token_before_reading_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lake_config(dir.path());
    cfg.auth.ingest_tokens = vec!["in".into()];
    let server = Server::new(cfg).await.unwrap();
    // A body that never ends: reading it would hang.
    let body = Body::from_stream(futures::stream::pending::<Result<Vec<u8>, std::io::Error>>());
    let req = Request::post("/v1/traces").header(header::CONTENT_TYPE, "application/x-protobuf").body(body).unwrap();
    let resp = tokio::time::timeout(Duration::from_secs(5), server.otlp_http_router().oneshot(req))
        .await
        .expect("answered without reading the body")
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Browser exporters send a CORS preflight without a token; it is answered
/// before the token check, and the real request still needs the token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn otlp_http_answers_cors_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lake_config(dir.path());
    cfg.auth.ingest_tokens = vec!["in".into()];
    let server = Server::new(cfg).await.unwrap();
    let preflight = Request::options("/v1/traces")
        .header(header::ORIGIN, "https://app.example")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type,authorization")
        .body(Body::empty())
        .unwrap();
    let resp = server.otlp_http_router().oneshot(preflight).await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "https://app.example");
    let post = Request::post("/v1/traces")
        .header(header::ORIGIN, "https://app.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let resp = server.otlp_http_router().oneshot(post).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "https://app.example");
}

/// OTLP/gRPC through a real tonic client: the token is checked before the
/// message is decoded, and an accepted export is queryable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grpc_roundtrip_and_auth_before_decoding() {
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lake_config(dir.path());
    cfg.auth.ingest_tokens = vec!["in".into()];
    cfg.ingest.max_request_bytes = mallard_common::ByteSize(64 * 1024);
    let server = Arc::new(Server::new(cfg).await.unwrap());
    let shutdown = CancellationToken::new();
    let (s2, sd) = (server.clone(), shutdown.clone());
    let bg = tokio::spawn(async move { s2.run_background(async move { sd.cancelled().await }).await.unwrap() });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let grpc = tonic::transport::Server::builder().add_routes(server.grpc_routes());
    let sd = shutdown.clone();
    let grpc_task = tokio::spawn(async move {
        grpc.serve_with_incoming_shutdown(tonic::transport::server::TcpIncoming::from(listener), sd.cancelled_owned())
            .await
    });
    let mut client = TraceServiceClient::connect(format!("http://{addr}")).await.unwrap();

    // Larger than max_request_bytes and without a token: refused for the token,
    // so the message was never decoded.
    let mut big = traces_req(now_ns());
    big.resource_spans[0].resource.as_mut().unwrap().attributes.push(kv("big", &"x".repeat(100 * 1024)));
    let err = client.export(big).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated, "{err}");

    let mut req = tonic::Request::new(traces_req(now_ns() - 1_000_000_000));
    req.metadata_mut().insert("authorization", "Bearer in".parse().unwrap());
    client.export(req).await.expect("authorized export");
    let api = server.api_router();
    let mut found = false;
    for _ in 0..50 {
        let get = Request::get(format!("/api/v1/traces/{TRACE_HEX}?since=1h")).body(Body::empty()).unwrap();
        if api.clone().oneshot(get).await.unwrap().status() == StatusCode::OK {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "trace sent over gRPC is queryable");
    shutdown.cancel();
    grpc_task.await.unwrap().unwrap();
    bg.await.unwrap();
}

/// Every path in the OpenAPI document is served by the router.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_documented_route_is_served() {
    let h = harness(None).await;
    let spec = h.get_raw("/openapi.json", None).await.1;
    let spec: serde_json::Value = serde_json::from_str(&spec).unwrap();
    let paths = spec["paths"].as_object().unwrap();
    assert!(paths.len() >= 12);
    for (path, ops) in paths {
        let uri = path.replace("{trace_id}", TRACE_HEX).replace("{span_id}", "0000000000000001");
        for method in ops.as_object().unwrap().keys() {
            let req = Request::builder()
                .method(method.to_uppercase().as_str())
                .uri(&uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let resp = h.api.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            // A handler's 404 (e.g. unknown trace) has an error body; a missing route has none.
            assert!(
                status != StatusCode::NOT_FOUND || serde_json::from_slice::<ApiError>(&body).is_ok(),
                "{method} {path} is documented but not served"
            );
            assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        }
    }
    h.stop().await;
}

/// Delta sums, exponential histograms and summaries: ingest and query.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delta_sums_exp_histograms_and_summaries() {
    let h = harness(None).await;
    let t0 = now_ns() - 1_000_000_000;
    let start = t0 - 600_000_000_000;
    let (mut delta, mut exp, mut summary) = (vec![], vec![], vec![]);
    for i in 0..10u64 {
        let t = t0 - (9 - i) * 60_000_000_000;
        // 30 jobs per minute, reported as deltas.
        delta.push(NumberDataPoint {
            start_time_unix_nano: t - 60_000_000_000,
            time_unix_nano: t,
            value: Some(number_data_point::Value::AsInt(30)),
            ..Default::default()
        });
        // Cumulative, scale 0 (base 2): bucket 3 = (8, 16]; 10 values of 12 per minute.
        exp.push(ExponentialHistogramDataPoint {
            start_time_unix_nano: start,
            time_unix_nano: t,
            count: 10 * (i + 1),
            sum: Some(120.0 * (i + 1) as f64),
            scale: 0,
            positive: Some(Buckets { offset: 3, bucket_counts: vec![10 * (i + 1)] }),
            ..Default::default()
        });
        // Cumulative count/sum; quantiles as reported by the client.
        summary.push(SummaryDataPoint {
            start_time_unix_nano: start,
            time_unix_nano: t,
            count: 10 * (i + 1),
            sum: 50.0 * (i + 1) as f64,
            quantile_values: vec![
                ValueAtQuantile { quantile: 0.5, value: 4.0 },
                ValueAtQuantile { quantile: 0.99, value: 9.0 },
            ],
            ..Default::default()
        });
    }
    let metric = |name: &str, data: Data| Metric { name: name.into(), data: Some(data), ..Default::default() };
    let req = ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource("worker"),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![
                    metric(
                        "jobs.done",
                        Data::Sum(Sum {
                            data_points: delta,
                            aggregation_temporality: AggregationTemporality::Delta as i32,
                            is_monotonic: true,
                        }),
                    ),
                    metric(
                        "rpc.latency",
                        Data::ExponentialHistogram(ExponentialHistogram {
                            data_points: exp,
                            aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        }),
                    ),
                    metric("gc.pause", Data::Summary(Summary { data_points: summary })),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let (s, _) = h.post("/v1/metrics", "application/x-protobuf", req.encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);
    let list: Vec<MetricInfo> = h
        .eventually(|| async {
            let v: Vec<MetricInfo> = h.get("/api/v1/metrics?since=1h").await;
            (v.len() == 3).then_some(v)
        })
        .await;
    let types: Vec<_> = list.iter().map(|m| (m.name.as_str(), m.metric_type, m.temporality)).collect();
    assert!(types.contains(&("jobs.done", MetricType::Sum, Temporality::Delta)), "{types:?}");
    assert!(types.contains(&("rpc.latency", MetricType::ExponentialHistogram, Temporality::Cumulative)));
    assert!(types.contains(&("gc.pause", MetricType::Summary, Temporality::Unspecified)));

    let all_near = |r: &MetricQueryResult, want: f64| {
        assert!(!r.series.is_empty() && !r.series[0].points.is_empty(), "{r:?}");
        for p in &r.series[0].points {
            assert!((p.value - want).abs() < 1e-6, "{} {}: {} != {want}", r.name, r.agg, p.value);
        }
    };
    let q = |name: &str, agg: &str| format!("/api/v1/metrics/query?name={name}&since=15m&step=1m&agg={agg}");
    all_near(&h.get(&q("jobs.done", "rate")).await, 0.5);
    all_near(&h.get(&q("jobs.done", "increase")).await, 30.0);
    all_near(&h.get(&q("rpc.latency", "p50")).await, 12.0);
    all_near(&h.get(&q("rpc.latency", "avg")).await, 12.0);
    all_near(&h.get(&q("rpc.latency", "count")).await, 10.0);
    all_near(&h.get(&q("gc.pause", "p99")).await, 9.0);
    all_near(&h.get(&q("gc.pause", "avg")).await, 5.0);
    h.stop().await;
}

/// Traces and logs page through a cursor: every item once, in order, also
/// when many items share a timestamp.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursors_page_through_traces_and_logs() {
    let h = harness(None).await;
    let t0 = now_ns() - 5_000_000_000;
    // 7 traces; traces 0-3 have the same start time (ties on the cursor time).
    let mut req = traces_req(t0);
    let template = req.resource_spans[0].scope_spans[0].spans[0].clone();
    req.resource_spans.truncate(1);
    req.resource_spans[0].scope_spans[0].spans = (0..7u8)
        .map(|i| {
            let mut s = template.clone();
            s.trace_id = vec![i + 1; 16];
            s.span_id = vec![0, 0, 0, 0, 0, 0, 0, i + 1];
            let t = t0 + if i < 4 { 0 } else { i as u64 * 1_000_000 };
            (s.start_time_unix_nano, s.end_time_unix_nano) = (t, t + 1_000_000);
            s
        })
        .collect();
    let (s, _) = h.post("/v1/traces", "application/x-protobuf", req.encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);
    // 9 logs, all at the same time: only the row id orders them.
    let mut logs = logs_req(t0);
    let rec = logs.resource_logs[0].scope_logs[0].log_records[1].clone();
    logs.resource_logs[0].scope_logs[0].log_records = (0..9)
        .map(|i| {
            let mut r = rec.clone();
            r.body = Some(AnyValue { value: Some(any_value::Value::StringValue(format!("same time {i}"))) });
            r
        })
        .collect();
    let (s, _) = h.post("/v1/logs", "application/x-protobuf", logs.encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);
    h.eventually(|| async {
        let t: Vec<TraceSummary> = h.get("/api/v1/traces?since=1h").await;
        let l: Vec<LogRecord> = h.get("/api/v1/logs?since=1h").await;
        (t.len() == 7 && l.len() == 9).then_some(())
    })
    .await;

    let pages: Vec<Vec<TraceSummary>> = h.all_pages("/api/v1/traces?since=1h&limit=2").await;
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 2, 1], "the last page has no cursor");
    let ids: Vec<String> = pages.iter().flatten().map(|t| t.trace_id.clone()).collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 7, "each trace once: {ids:?}");
    assert_eq!(ids[0], "07070707070707070707070707070707", "newest first");
    let exact: Vec<Vec<TraceSummary>> = h.all_pages("/api/v1/traces?since=1h&limit=7").await;
    assert_eq!(exact.len(), 1, "a full last page has no cursor");

    for order in ["desc", "asc"] {
        let pages: Vec<Vec<LogRecord>> = h.all_pages(&format!("/api/v1/logs?since=1h&limit=4&order={order}")).await;
        assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [4, 4, 1], "{order}");
        let bodies: std::collections::BTreeSet<String> = pages.iter().flatten().map(|r| r.body.clone()).collect();
        assert_eq!(bodies.len(), 9, "{order}: each record once: {bodies:?}");
    }
    // The cursor keeps the filters: only the 9 INFO records match.
    let pages: Vec<Vec<LogRecord>> = h.all_pages("/api/v1/logs?since=1h&limit=5&q=same").await;
    assert_eq!(pages.iter().map(Vec::len).sum::<usize>(), 9);

    for bad in ["nope", "1:", "1:zz", "x%3Aab"] {
        let (s, _) = h.get_raw(&format!("/api/v1/logs?since=1h&cursor={bad}"), None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (s, _) = h.get_raw("/api/v1/traces?since=1h&cursor=1%3Aab", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a trace cursor needs a trace id");
    h.stop().await;
}

/// Attribute keys and values per signal, and the schema the SQL endpoint sees.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attributes_and_schema() {
    let h = harness(None).await;
    let t0 = now_ns() - 1_000_000_000;
    for (path, body) in [
        ("/v1/traces", traces_req(t0).encode_to_vec()),
        ("/v1/logs", logs_req(t0).encode_to_vec()),
        ("/v1/metrics", metrics_req(t0, 3).encode_to_vec()),
    ] {
        assert_eq!(h.post(path, "application/x-protobuf", body).await.0, StatusCode::OK, "{path}");
    }
    let spans: AttributeStats = h
        .eventually(|| async {
            let a: AttributeStats = h.get("/api/v1/attributes?signal=spans&since=1h").await;
            let l: AttributeStats = h.get("/api/v1/attributes?signal=logs&since=1h").await;
            let m: AttributeStats = h.get("/api/v1/attributes?signal=metrics&since=1h").await;
            (a.total_rows == 3 && l.total_rows == 2 && m.total_rows > 0).then_some(a)
        })
        .await;
    assert_eq!(spans.sampled_rows, 3);
    let count = |s: &AttributeStats, k: &str| s.items.iter().find(|a| a.key == k).map(|a| a.count);
    // Resource attributes count too; most frequent first.
    assert_eq!(count(&spans, "service.name"), Some(3));
    assert_eq!(count(&spans, "host.name"), Some(3));
    assert_eq!(count(&spans, "http.route"), Some(1));
    assert!(spans.items.iter().all(|a| a.value.is_none()));
    assert!(spans.items.windows(2).all(|w| w[0].count >= w[1].count), "{spans:?}");

    let svc: AttributeStats = h.get("/api/v1/attributes?signal=spans&since=1h&service=db").await;
    assert_eq!((svc.total_rows, count(&svc, "http.route")), (1, None));
    let values: AttributeStats = h.get("/api/v1/attributes?signal=spans&since=1h&key=service.name").await;
    let v: Vec<(Option<&str>, u64)> = values.items.iter().map(|a| (a.value.as_deref(), a.count)).collect();
    assert_eq!(v, [(Some("checkout"), 2), (Some("db"), 1)]);
    assert_eq!(values.total_rows, 3, "rows that have the key");
    let one: AttributeStats = h.get("/api/v1/attributes?signal=spans&since=1h&key=service.name&limit=1").await;
    assert_eq!(one.items.len(), 1);

    let logs: AttributeStats = h.get("/api/v1/attributes?signal=logs&since=1h&key=order.id").await;
    assert_eq!((logs.items[0].value.as_deref(), logs.total_rows), (Some("42"), 1));

    // Metrics: all point tables, one metric.
    let m: AttributeStats = h.get("/api/v1/attributes?signal=metrics&since=1h&metric=http.requests&key=route").await;
    let v: Vec<(Option<&str>, u64)> = m.items.iter().map(|a| (a.value.as_deref(), a.count)).collect();
    assert_eq!(v, [(Some("/a"), 3), (Some("/b"), 3)]);
    let hist: AttributeStats = h.get("/api/v1/attributes?signal=metrics&since=1h&metric=http.duration").await;
    assert_eq!((hist.total_rows, count(&hist, "route")), (3, Some(3)));

    for bad in ["", "signal=events", "signal=spans&metric=x", "signal=spans&key=", "signal=spans&limit=0"] {
        let (s, body) = h.get_raw(&format!("/api/v1/attributes?since=1h&{bad}"), None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }

    let schema: Schema = h.get("/api/v1/schema").await;
    let names: Vec<&str> = schema.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["spans", "logs", "metric_points", "metric_histograms", "metric_exp_histograms", "metric_summaries"]
    );
    let spans = &schema.tables[0];
    assert_eq!(spans.time_column, "start_time");
    assert_eq!((spans.columns[0].name.as_str(), spans.columns[0].data_type.as_str()), ("start_time", "TIMESTAMP"));
    assert!(spans.columns.iter().any(|c| c.name == "attributes" && c.data_type.starts_with("MAP")), "{spans:?}");
    h.stop().await;
}

/// One metric with a delta and a cumulative series: each series is handled
/// by its own temporality (otel issue 0008).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_temporality_in_one_metric() {
    let h = harness(None).await;
    let t0 = now_ns() - 1_000_000_000;
    let start = t0 - 600_000_000_000;
    let src = |s: &str| vec![kv("src", s)];
    let (mut delta, mut cum, mut hdelta, mut hcum) = (vec![], vec![], vec![], vec![]);
    for i in 0..10u64 {
        let t = t0 - (9 - i) * 60_000_000_000;
        // 30 per minute as deltas.
        delta.push(NumberDataPoint {
            attributes: src("delta"),
            start_time_unix_nano: t - 60_000_000_000,
            time_unix_nano: t,
            value: Some(number_data_point::Value::AsInt(30)),
            ..Default::default()
        });
        // 60 per minute, cumulative, restarting after the fifth minute.
        let n = if i < 5 { i + 1 } else { i - 4 };
        cum.push(NumberDataPoint {
            attributes: src("cumulative"),
            start_time_unix_nano: if i < 5 { start } else { t - 60_000_000_000 },
            time_unix_nano: t,
            value: Some(number_data_point::Value::AsInt(60 * n as i64)),
            ..Default::default()
        });
        // Histograms: 10 observations per minute in each series.
        hdelta.push(HistogramDataPoint {
            attributes: src("delta"),
            start_time_unix_nano: t - 60_000_000_000,
            time_unix_nano: t,
            count: 10,
            bucket_counts: vec![10, 0],
            explicit_bounds: vec![10.0],
            ..Default::default()
        });
        hcum.push(HistogramDataPoint {
            attributes: src("cumulative"),
            start_time_unix_nano: start,
            time_unix_nano: t,
            count: 10 * (i + 1),
            bucket_counts: vec![10 * (i + 1), 0],
            explicit_bounds: vec![10.0],
            ..Default::default()
        });
    }
    let sum = |data_points, t: AggregationTemporality| {
        Data::Sum(Sum { data_points, aggregation_temporality: t as i32, is_monotonic: true })
    };
    let hist = |data_points, t: AggregationTemporality| {
        Data::Histogram(Histogram { data_points, aggregation_temporality: t as i32 })
    };
    let metric = |name: &str, data: Data| Metric { name: name.into(), data: Some(data), ..Default::default() };
    let req = ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource("worker"),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![
                    metric("jobs.mixed", sum(delta, AggregationTemporality::Delta)),
                    metric("jobs.mixed", sum(cum, AggregationTemporality::Cumulative)),
                    metric("latency.mixed", hist(hdelta, AggregationTemporality::Delta)),
                    metric("latency.mixed", hist(hcum, AggregationTemporality::Cumulative)),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let (s, _) = h.post("/v1/metrics", "application/x-protobuf", req.encode_to_vec()).await;
    assert_eq!(s, StatusCode::OK);
    h.eventually(|| async {
        let v: Vec<MetricInfo> = h.get("/api/v1/metrics?since=1h").await;
        (v.iter().map(|m| m.point_count).sum::<u64>() == 40).then_some(())
    })
    .await;

    let catalog: Vec<MetricInfo> = h.get("/api/v1/metrics?since=1h").await;
    for m in &catalog {
        assert_eq!(m.temporality, Temporality::Mixed, "{}", m.name);
    }
    assert_eq!(catalog.len(), 2);

    let per_src = |r: MetricQueryResult, want: [(&str, f64); 2]| {
        assert_eq!(r.series.len(), 2, "{r:?}");
        for (src, v) in want {
            let s = r.series.iter().find(|s| s.labels["src"] == src).unwrap();
            assert!(s.points.len() >= 9, "{} {src}: {:?}", r.name, s.points);
            for p in &s.points {
                assert!((p.value - v).abs() < 1e-6, "{} {} {src}: {} != {v}", r.name, r.agg, p.value);
            }
        }
    };
    let q =
        |name: &str, agg: &str| format!("/api/v1/metrics/query?name={name}&since=15m&step=1m&agg={agg}&group_by=src");
    per_src(h.get(&q("jobs.mixed", "increase")).await, [("delta", 30.0), ("cumulative", 60.0)]);
    per_src(h.get(&q("jobs.mixed", "rate")).await, [("delta", 0.5), ("cumulative", 1.0)]);
    per_src(h.get(&q("latency.mixed", "count")).await, [("delta", 10.0), ("cumulative", 10.0)]);
    h.stop().await;
}
