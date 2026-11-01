//! End-to-end: OTLP in through the HTTP router, data flushed to a real
//! DuckLake (DuckDB catalog + local files), queried through the API router.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use odl_api::*;
use odl_server::{Server, config::Config, store};
use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    common::v1::{AnyValue, KeyValue, any_value},
    logs::v1::{LogRecord as OtlpLog, ResourceLogs, ScopeLogs},
    metrics::v1::{
        AggregationTemporality, Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint, ResourceMetrics,
        ScopeMetrics, Sum, metric::Data, number_data_point,
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
    assert_eq!(root.kind, "SERVER");
    assert_eq!(root.status_code, "ERROR");
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
    assert_eq!(counter.metric_type, "sum");
    assert_eq!(counter.series_count, 2);
    assert_eq!(counter.temporality, "cumulative");
    assert!(list.iter().any(|m| m.metric_type == "histogram"));

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
    let lake = store::Lake::open(&cfg).unwrap();
    let conn = lake.connect().unwrap();
    // Write 6 small batches directly: two days ago and now.
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
        let c = odl_server::otlp::convert::convert_traces(&req);
        let tx = conn.unchecked_transaction().unwrap();
        let mut app = conn.appender_to_catalog_and_db("spans", "lake", "otel").unwrap();
        for (_, b) in c.batches {
            app.append_record_batch(b).unwrap();
        }
        app.flush().unwrap();
        drop(app);
        tx.commit().unwrap();
    }
    let files = |c: &duckdb::Connection| -> i64 {
        c.query_row(
            "SELECT count(*) FROM __ducklake_metadata_lake.ducklake_data_file WHERE end_snapshot IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(files(&conn), 6);
    let detail = store::maintenance::compact(&conn, "otel", Some(16_000_000)).unwrap();
    assert!(detail.contains("merged"), "{detail}");
    assert_eq!(files(&conn), 2, "one file per day partition after merge");
    let detail = store::maintenance::retention(&conn, "otel", &cfg.maintenance, &cfg.retention).unwrap();
    assert!(detail.contains("deleted 6 expired rows"), "{detail}");
    let n: i64 = conn.query_row("SELECT count(*) FROM spans", [], |r| r.get(0)).unwrap();
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
        let lake = store::Lake::open(&cfg).unwrap();
        lake.connect().unwrap().execute_batch("ALTER TABLE spans ADD COLUMN future_column VARCHAR").unwrap();
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
    let conn = store::open_connection(&cfg).unwrap();
    let server = Server::with_connection(cfg, conn.try_clone().unwrap()).await.unwrap();
    // New connections can no longer select the lake schema, so the flusher fails to start.
    conn.execute_batch("DETACH lake").unwrap();
    let r = tokio::time::timeout(Duration::from_secs(20), server.serve(CancellationToken::new()))
        .await
        .expect("serve must return when the background jobs fail");
    assert!(r.is_err());
}
