//! mallard-otel-loadgen: realistic OTLP/HTTP traffic for benchmarks and demos.
//!
//! Simulates a small web shop: a frontend calls a gateway, which calls a few
//! backend services, which call a database and a cache. Each trace produces
//! 4-12 spans, some log records (correlated by trace id) and periodic metrics
//! (request counter, latency histogram, memory and CPU gauges).
//!
//! Modes:
//! - live (default): `--rate` traces per second with current timestamps.
//! - backfill: `--backfill 24h` generates history spread over the last 24 h at
//!   `--rate` traces per simulated second, sent as fast as the server accepts.

use std::{
    collections::HashMap,
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use clap::Parser;
use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value},
    logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
    metrics::v1::{
        AggregationTemporality, Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint, ResourceMetrics,
        ScopeMetrics, Sum, metric::Data, number_data_point,
    },
    resource::v1::Resource,
    trace::v1::{ResourceSpans, ScopeSpans, Span, Status, span},
};
use prost::Message;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use tokio::sync::Semaphore;

#[derive(Parser, Clone)]
#[command(name = "mallard-otel-loadgen", version, about = "Send realistic OTLP traffic to an OTLP/HTTP endpoint")]
struct Args {
    /// OTLP/HTTP base URL: mallard's otlp_http_addr (or its http_addr).
    #[arg(long, env = "OTLP_ENDPOINT", default_value = "http://localhost:4318")]
    endpoint: String,
    /// Traces per second (live) or per simulated second (backfill).
    #[arg(short, long, default_value_t = 10.0)]
    rate: f64,
    /// How long to run (live mode). `0` = until Ctrl-C.
    #[arg(short, long, default_value = "60s")]
    duration: String,
    /// Generate history over this period instead of live traffic, e.g. `24h`.
    #[arg(long)]
    backfill: Option<String>,
    /// Traces per OTLP request.
    #[arg(short, long, default_value_t = 50)]
    batch: usize,
    /// Concurrent requests in flight.
    #[arg(short, long, default_value_t = 4)]
    concurrency: usize,
    /// Average log records per trace.
    #[arg(long, default_value_t = 2.0)]
    logs_per_trace: f64,
    /// Metric export interval (simulated time).
    #[arg(long, default_value = "15s")]
    metrics_interval: String,
    /// Share of traces that fail.
    #[arg(long, default_value_t = 0.02)]
    error_rate: f64,
    /// gzip request bodies.
    #[arg(long)]
    gzip: bool,
    /// Bearer token for the receiver.
    #[arg(long, env = "ODL_INGEST_TOKEN")]
    token: Option<String>,
    /// Random seed for reproducible data (default: random).
    #[arg(long)]
    seed: Option<u64>,
}

// ---------------------------------------------------------------------------
// Topology
// ---------------------------------------------------------------------------

struct Route {
    service: &'static str,
    name: &'static str,
    http_route: &'static str,
    method: &'static str,
    /// median latency of own work in ms
    base_ms: f64,
    calls: &'static [&'static str],
}

const ROUTES: &[Route] = &[
    Route {
        service: "frontend",
        name: "GET /",
        http_route: "/",
        method: "GET",
        base_ms: 8.0,
        calls: &["gateway:GET /api/products"],
    },
    Route {
        service: "frontend",
        name: "GET /product/{id}",
        http_route: "/product/{id}",
        method: "GET",
        base_ms: 6.0,
        calls: &["gateway:GET /api/products/{id}", "gateway:GET /api/recommendations"],
    },
    Route {
        service: "frontend",
        name: "POST /checkout",
        http_route: "/checkout",
        method: "POST",
        base_ms: 10.0,
        calls: &["gateway:POST /api/checkout"],
    },
    Route {
        service: "frontend",
        name: "GET /search",
        http_route: "/search",
        method: "GET",
        base_ms: 5.0,
        calls: &["gateway:GET /api/search"],
    },
    Route {
        service: "gateway",
        name: "GET /api/products",
        http_route: "/api/products",
        method: "GET",
        base_ms: 2.0,
        calls: &["catalog:ListProducts"],
    },
    Route {
        service: "gateway",
        name: "GET /api/products/{id}",
        http_route: "/api/products/{id}",
        method: "GET",
        base_ms: 2.0,
        calls: &["catalog:GetProduct", "inventory:GetStock"],
    },
    Route {
        service: "gateway",
        name: "GET /api/recommendations",
        http_route: "/api/recommendations",
        method: "GET",
        base_ms: 2.0,
        calls: &["recommendations:Recommend"],
    },
    Route {
        service: "gateway",
        name: "POST /api/checkout",
        http_route: "/api/checkout",
        method: "POST",
        base_ms: 3.0,
        calls: &["checkout:PlaceOrder"],
    },
    Route {
        service: "gateway",
        name: "GET /api/search",
        http_route: "/api/search",
        method: "GET",
        base_ms: 2.0,
        calls: &["search:Query"],
    },
    Route {
        service: "catalog",
        name: "ListProducts",
        http_route: "",
        method: "",
        base_ms: 4.0,
        calls: &["db:SELECT products", "cache:GET"],
    },
    Route {
        service: "catalog",
        name: "GetProduct",
        http_route: "",
        method: "",
        base_ms: 2.0,
        calls: &["cache:GET", "db:SELECT products"],
    },
    Route {
        service: "inventory",
        name: "GetStock",
        http_route: "",
        method: "",
        base_ms: 3.0,
        calls: &["db:SELECT stock"],
    },
    Route {
        service: "recommendations",
        name: "Recommend",
        http_route: "",
        method: "",
        base_ms: 25.0,
        calls: &["catalog:ListProducts"],
    },
    Route {
        service: "checkout",
        name: "PlaceOrder",
        http_route: "",
        method: "",
        base_ms: 15.0,
        calls: &["inventory:ReserveStock", "payment:Charge", "db:INSERT orders", "notification:SendEmail"],
    },
    Route {
        service: "inventory",
        name: "ReserveStock",
        http_route: "",
        method: "",
        base_ms: 5.0,
        calls: &["db:UPDATE stock"],
    },
    Route {
        service: "payment",
        name: "Charge",
        http_route: "",
        method: "",
        base_ms: 120.0,
        calls: &["db:INSERT payments"],
    },
    Route { service: "notification", name: "SendEmail", http_route: "", method: "", base_ms: 30.0, calls: &[] },
    Route { service: "search", name: "Query", http_route: "", method: "", base_ms: 18.0, calls: &["cache:GET"] },
];

/// Leaf client calls: (kind, system, latency ms)
fn leaf(call: &str) -> Option<(&'static str, f64)> {
    match call.split(':').next()? {
        "db" => Some(("postgresql", 3.0)),
        "cache" => Some(("redis", 0.4)),
        _ => None,
    }
}

const SERVICES: &[&str] = &[
    "frontend",
    "gateway",
    "catalog",
    "inventory",
    "recommendations",
    "checkout",
    "payment",
    "notification",
    "search",
];

fn kv(k: &str, v: &str) -> KeyValue {
    KeyValue {
        key: k.into(),
        value: Some(AnyValue { value: Some(any_value::Value::StringValue(v.into())) }),
        ..Default::default()
    }
}

fn kv_i(k: &str, v: i64) -> KeyValue {
    KeyValue {
        key: k.into(),
        value: Some(AnyValue { value: Some(any_value::Value::IntValue(v)) }),
        ..Default::default()
    }
}

fn resource(service: &str) -> Resource {
    Resource {
        attributes: vec![
            kv("service.name", service),
            kv("service.version", "1.4.2"),
            kv("deployment.environment.name", "production"),
            kv("host.name", &format!("{service}-7d9f8b-x2k4p")),
            kv("k8s.namespace.name", "shop"),
            kv("telemetry.sdk.language", if service == "frontend" { "nodejs" } else { "go" }),
        ],
        ..Default::default()
    }
}

/// Log-normal-ish latency with occasional slow outliers.
fn latency(rng: &mut StdRng, median_ms: f64) -> f64 {
    let z: f64 = (0..6).map(|_| rng.random::<f64>()).sum::<f64>() - 3.0; // ~N(0,0.7)
    let mut v = median_ms * (z * 0.5).exp();
    if rng.random_bool(0.01) {
        v *= 5.0 + rng.random::<f64>() * 10.0;
    }
    v.max(0.05)
}

#[derive(Default)]
struct Out {
    spans: HashMap<&'static str, Vec<Span>>,
    logs: HashMap<&'static str, Vec<LogRecord>>,
    /// (service, route, status, duration_ms) of server spans, for metrics
    requests: Vec<(&'static str, &'static str, u16, f64)>,
    n_spans: u64,
    n_logs: u64,
}

struct Gen<'a> {
    rng: &'a mut StdRng,
    trace_id: [u8; 16],
    fail: bool,
    logs_per_span: f64,
    out: &'a mut Out,
}

impl Gen<'_> {
    fn span_id(&mut self) -> Vec<u8> {
        self.rng.random::<u64>().max(1).to_be_bytes().to_vec()
    }

    fn log(&mut self, service: &'static str, t: u64, span_id: &[u8], sev: i32, text: &str, body: String) {
        self.out.n_logs += 1;
        self.out.logs.entry(service).or_default().push(LogRecord {
            time_unix_nano: t,
            observed_time_unix_nano: t,
            severity_number: sev,
            severity_text: text.into(),
            body: Some(AnyValue { value: Some(any_value::Value::StringValue(body)) }),
            trace_id: self.trace_id.to_vec(),
            span_id: span_id.to_vec(),
            attributes: vec![kv("code.function", "handle")],
            ..Default::default()
        });
    }

    /// Generate the span tree for `route` starting at `start`; returns end time.
    fn route(&mut self, route: &Route, start: u64, parent: Option<Vec<u8>>, depth: usize) -> (u64, bool) {
        let id = self.span_id();
        let own = latency(self.rng, route.base_ms);
        let mut t = start + (own * 0.3 * 1e6) as u64;
        let mut failed = false;
        let mut route_children = 0;
        for call in route.calls {
            let (svc, name) = call.split_once(':').unwrap();
            if let Some((system, ms)) = leaf(call) {
                let cid = self.span_id();
                let d = latency(self.rng, ms);
                let end = t + (d * 1e6) as u64;
                self.out.n_spans += 1;
                let mut attrs = vec![kv("db.system.name", system)];
                if system == "postgresql" {
                    attrs.push(kv("db.query.text", &format!("{name} WHERE id = $1")));
                    attrs.push(kv("db.namespace", "shop"));
                }
                self.out.spans.entry(route.service).or_default().push(Span {
                    trace_id: self.trace_id.to_vec(),
                    span_id: cid,
                    parent_span_id: id.clone(),
                    name: if system == "redis" { format!("redis {name}") } else { name.to_string() },
                    kind: 3,
                    start_time_unix_nano: t,
                    end_time_unix_nano: end,
                    attributes: attrs,
                    ..Default::default()
                });
                t = end;
            } else if let Some(r) = ROUTES.iter().find(|r| r.service == svc && r.name == name) {
                if depth > 6 {
                    continue;
                }
                // client span in caller + server span in callee
                let cid = self.span_id();
                let net = (self.rng.random::<f64>() * 0.8 * 1e6) as u64;
                route_children += 1;
                let (end, child_failed) = self.route(r, t + net, Some(cid.clone()), depth + 1);
                failed |= child_failed;
                self.out.n_spans += 1;
                self.out.spans.entry(route.service).or_default().push(Span {
                    trace_id: self.trace_id.to_vec(),
                    span_id: cid,
                    parent_span_id: id.clone(),
                    name: if r.method.is_empty() {
                        format!("{svc}/{name}")
                    } else {
                        format!("{} {}", r.method, r.http_route)
                    },
                    kind: 3,
                    start_time_unix_nano: t,
                    end_time_unix_nano: end + net,
                    attributes: vec![kv("server.address", svc)],
                    status: child_failed.then(|| Status { code: 2, message: String::new() }),
                    ..Default::default()
                });
                t = end + net;
            }
        }
        // In a failing trace the deepest service (no downstream services)
        // fails, and the error propagates up to the callers.
        let own_fail = self.fail && !failed && route_children == 0;
        let failed = failed || own_fail;
        let end = t + (own * 0.7 * 1e6) as u64;
        let status_code: u16 = if failed { if route.service == "payment" { 502 } else { 500 } } else { 200 };
        let mut attrs = vec![];
        if !route.http_route.is_empty() {
            attrs.push(kv("http.request.method", route.method));
            attrs.push(kv("http.route", route.http_route));
            attrs.push(kv_i("http.response.status_code", status_code as i64));
            attrs.push(kv("url.scheme", "https"));
            if route.service == "frontend" {
                attrs.push(kv(
                    "user_agent.original",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15",
                ));
                attrs.push(kv(
                    "client.address",
                    &format!("10.0.{}.{}", self.rng.random_range(0..255), self.rng.random_range(1..255)),
                ));
                // High-cardinality values, as in real traffic.
                let path = route.http_route.replace("{id}", &self.rng.random_range(1..50_000u32).to_string());
                let sid: u64 = self.rng.random();
                attrs.push(kv("url.full", &format!("https://shop.example.com{path}?utm_source=mail&sid={sid:016x}")));
                attrs.push(kv("user.id", &format!("u-{}", self.rng.random_range(1..20_000u32))));
                let rid: u128 = self.rng.random();
                attrs.push(kv("http.request.header.x-request-id", &format!("{rid:032x}")));
            }
        } else {
            attrs.push(kv("rpc.system", "grpc"));
            attrs.push(kv("rpc.service", route.service));
            attrs.push(kv("rpc.method", route.name));
        }
        let mut events = vec![];
        if own_fail {
            events.push(span::Event {
                time_unix_nano: end - 1000,
                name: "exception".into(),
                attributes: vec![
                    kv(
                        "exception.type",
                        if route.service == "payment" { "PaymentDeclinedError" } else { "TimeoutError" },
                    ),
                    kv("exception.message", "upstream request failed"),
                    kv("exception.stacktrace", "at handle (handler.go:42)\nat serve (server.go:118)"),
                ],
                ..Default::default()
            });
        }
        self.out.n_spans += 1;
        self.out.spans.entry(route.service).or_default().push(Span {
            trace_id: self.trace_id.to_vec(),
            span_id: id.clone(),
            parent_span_id: parent.unwrap_or_default(),
            name: route.name.into(),
            kind: 2,
            start_time_unix_nano: start,
            end_time_unix_nano: end,
            attributes: attrs,
            events,
            status: failed.then(|| Status {
                code: 2,
                message: if own_fail { "upstream request failed".into() } else { String::new() },
            }),
            ..Default::default()
        });
        let dur_ms = (end - start) as f64 / 1e6;
        self.out.requests.push((route.service, route.name, status_code, dur_ms));
        // logs
        if self.rng.random_bool(self.logs_per_span.min(1.0)) {
            let user = self.rng.random_range(1..20_000u32);
            let order: u32 = self.rng.random();
            self.log(
                route.service,
                start + 1000,
                &id,
                9,
                "INFO",
                format!("handled {} for user u-{user} order {order:08x} in {:.1}ms", route.name, dur_ms),
            );
        }
        if dur_ms > route.base_ms * 4.0 && self.rng.random_bool(0.5) {
            self.log(route.service, end, &id, 13, "WARN", format!("slow request {} took {:.0}ms", route.name, dur_ms));
        }
        if own_fail {
            self.log(
                route.service,
                end,
                &id,
                17,
                "ERROR",
                format!("{} failed: upstream request failed (status {status_code})", route.name),
            );
        }
        (end, failed)
    }
}

fn gen_traces(rng: &mut StdRng, n: usize, times: &[u64], args: &Args) -> Out {
    let mut out = Out::default();
    let entry: Vec<&Route> = ROUTES.iter().filter(|r| r.service == "frontend").collect();
    // Weighted: browse more than checkout.
    let weights = [40u32, 35, 8, 17];
    let total: u32 = weights.iter().sum();
    for &t in times.iter().take(n) {
        let mut pick = rng.random_range(0..total);
        let mut idx = 0;
        for (i, w) in weights.iter().enumerate() {
            if pick < *w {
                idx = i;
                break;
            }
            pick -= w;
        }
        let trace_id: [u8; 16] = rng.random();
        let fail = rng.random_bool(args.error_rate.clamp(0.0, 1.0)) || (idx == 2 && rng.random_bool(0.05));
        let spans_per_trace = 7.0;
        let mut g =
            Gen { rng, trace_id, fail, logs_per_span: args.logs_per_trace / spans_per_trace * 1.6, out: &mut out };
        g.route(entry[idx], t, None, 0);
    }
    out
}

fn traces_request(out: &mut Out) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: out
            .spans
            .drain()
            .map(|(svc, spans)| ResourceSpans {
                resource: Some(resource(svc)),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope {
                        name: "io.opentelemetry.auto".into(),
                        version: "2.10.0".into(),
                        ..Default::default()
                    }),
                    spans,
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect(),
    }
}

fn logs_request(out: &mut Out) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: out
            .logs
            .drain()
            .map(|(svc, log_records)| ResourceLogs {
                resource: Some(resource(svc)),
                scope_logs: vec![ScopeLogs {
                    scope: Some(InstrumentationScope { name: "app".into(), ..Default::default() }),
                    log_records,
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Metrics state (cumulative)
// ---------------------------------------------------------------------------

const BOUNDS: [f64; 14] = [0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0];

#[derive(Default)]
struct HistState {
    count: u64,
    sum: f64,
    buckets: Vec<u64>,
}

struct MetricsState {
    start_ns: u64,
    counters: HashMap<(&'static str, &'static str, u16), u64>,
    hists: HashMap<(&'static str, &'static str), HistState>,
}

impl MetricsState {
    fn new(start_ns: u64) -> Self {
        Self { start_ns, counters: HashMap::new(), hists: HashMap::new() }
    }

    fn record(&mut self, reqs: &[(&'static str, &'static str, u16, f64)]) {
        for (svc, route, status, ms) in reqs {
            *self.counters.entry((svc, route, *status)).or_default() += 1;
            let h = self
                .hists
                .entry((svc, route))
                .or_insert_with(|| HistState { buckets: vec![0; BOUNDS.len() + 1], ..Default::default() });
            let s = ms / 1000.0;
            h.count += 1;
            h.sum += s;
            let i = BOUNDS.iter().position(|b| s <= *b).unwrap_or(BOUNDS.len());
            h.buckets[i] += 1;
        }
    }

    fn export(&self, rng: &mut StdRng, now_ns: u64) -> (ExportMetricsServiceRequest, u64) {
        let mut points = 0u64;
        let resource_metrics = SERVICES
            .iter()
            .map(|svc| {
                let mut req_pts = vec![];
                for ((s, route, status), n) in &self.counters {
                    if s == svc {
                        req_pts.push(NumberDataPoint {
                            attributes: vec![
                                kv("rpc.method", route),
                                kv_i("http.response.status_code", *status as i64),
                            ],
                            start_time_unix_nano: self.start_ns,
                            time_unix_nano: now_ns,
                            value: Some(number_data_point::Value::AsInt(*n as i64)),
                            ..Default::default()
                        });
                    }
                }
                let mut hist_pts = vec![];
                for ((s, route), h) in &self.hists {
                    if s == svc {
                        hist_pts.push(HistogramDataPoint {
                            attributes: vec![kv("rpc.method", route)],
                            start_time_unix_nano: self.start_ns,
                            time_unix_nano: now_ns,
                            count: h.count,
                            sum: Some(h.sum),
                            bucket_counts: h.buckets.clone(),
                            explicit_bounds: BOUNDS.to_vec(),
                            ..Default::default()
                        });
                    }
                }
                let gauge = |v: f64| NumberDataPoint {
                    time_unix_nano: now_ns,
                    value: Some(number_data_point::Value::AsDouble(v)),
                    ..Default::default()
                };
                let mem = 180e6 + rng.random::<f64>() * 40e6 + (svc.len() as f64 * 10e6);
                let cpu = 0.05 + rng.random::<f64>() * 0.3;
                points += req_pts.len() as u64 + hist_pts.len() as u64 + 2;
                let m = |name: &str, unit: &str, data: Data| Metric {
                    name: name.into(),
                    unit: unit.into(),
                    description: String::new(),
                    data: Some(data),
                    ..Default::default()
                };
                ResourceMetrics {
                    resource: Some(resource(svc)),
                    scope_metrics: vec![ScopeMetrics {
                        scope: Some(InstrumentationScope { name: "app.metrics".into(), ..Default::default() }),
                        metrics: vec![
                            m(
                                "app.requests",
                                "{request}",
                                Data::Sum(Sum {
                                    data_points: req_pts,
                                    aggregation_temporality: AggregationTemporality::Cumulative as i32,
                                    is_monotonic: true,
                                }),
                            ),
                            m(
                                "app.request.duration",
                                "s",
                                Data::Histogram(Histogram {
                                    data_points: hist_pts,
                                    aggregation_temporality: AggregationTemporality::Cumulative as i32,
                                }),
                            ),
                            m("process.memory.usage", "By", Data::Gauge(Gauge { data_points: vec![gauge(mem)] })),
                            m("process.cpu.utilization", "1", Data::Gauge(Gauge { data_points: vec![gauge(cpu)] })),
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                }
            })
            .collect();
        (ExportMetricsServiceRequest { resource_metrics }, points)
    }
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    spans: AtomicU64,
    logs: AtomicU64,
    points: AtomicU64,
    requests: AtomicU64,
    throttled: AtomicU64,
    failed: AtomicU64,
    bytes: AtomicU64,
    latencies: Mutex<Vec<f64>>,
}

struct Sender {
    http: reqwest::Client,
    endpoint: String,
    token: Option<String>,
    gzip: bool,
    sem: Arc<Semaphore>,
    c: Arc<Counters>,
}

impl Sender {
    /// Send with retries on 429/503 (like an OTLP exporter). Returns when sent or dropped.
    async fn send(self: &Arc<Self>, path: &'static str, body: Vec<u8>, items: u64, kind: usize) {
        let permit = self.sem.clone().acquire_owned().await.unwrap();
        let this = self.clone();
        tokio::spawn(async move {
            let _p = permit;
            let body = if this.gzip {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                e.write_all(&body).unwrap();
                e.finish().unwrap()
            } else {
                body
            };
            this.c.bytes.fetch_add(body.len() as u64, Relaxed);
            let mut backoff = Duration::from_millis(250);
            for _ in 0..8 {
                let mut r = this
                    .http
                    .post(format!("{}{}", this.endpoint, path))
                    .header("content-type", "application/x-protobuf")
                    .body(body.clone());
                if this.gzip {
                    r = r.header("content-encoding", "gzip");
                }
                if let Some(t) = &this.token {
                    r = r.bearer_auth(t);
                }
                let t0 = Instant::now();
                let res = r.send().await;
                this.c.requests.fetch_add(1, Relaxed);
                match res {
                    Ok(resp) if resp.status().is_success() => {
                        this.c.latencies.lock().unwrap().push(t0.elapsed().as_secs_f64() * 1000.0);
                        let ctr = [&this.c.spans, &this.c.logs, &this.c.points][kind];
                        ctr.fetch_add(items, Relaxed);
                        return;
                    }
                    Ok(resp) if resp.status() == 429 || resp.status() == 503 => {
                        this.c.throttled.fetch_add(1, Relaxed);
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                    }
                    Ok(resp) => {
                        let s = resp.status();
                        let text = resp.text().await.unwrap_or_default();
                        eprintln!("{path}: HTTP {s}: {}", text.chars().take(200).collect::<String>());
                        this.c.failed.fetch_add(1, Relaxed);
                        return;
                    }
                    Err(e) => {
                        // Retried below; only counted as failed when we give up.
                        eprintln!("{path}: {e} (retrying)");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                    }
                }
            }
            this.c.failed.fetch_add(1, Relaxed);
        });
    }

    async fn drain(&self, concurrency: usize) {
        let _all = self.sem.acquire_many(concurrency as u32).await.unwrap();
    }
}

fn report(c: &Counters, elapsed: f64, prev: &mut (u64, u64, u64, f64)) {
    let (s, l, p) = (c.spans.load(Relaxed), c.logs.load(Relaxed), c.points.load(Relaxed));
    let dt = (elapsed - prev.3).max(1e-9);
    let mut lat = std::mem::take(&mut *c.latencies.lock().unwrap());
    lat.sort_by(f64::total_cmp);
    let pct = |q: f64| {
        if lat.is_empty() { 0.0 } else { lat[((lat.len() - 1) as f64 * q) as usize] }
    };
    println!(
        "{:>6.0}s  spans {:>8.0}/s  logs {:>7.0}/s  points {:>6.0}/s  | total spans {:>10}  logs {:>9}  | req p50 {:>6.1}ms p99 {:>7.1}ms  throttled {}  failed {}",
        elapsed,
        (s - prev.0) as f64 / dt,
        (l - prev.1) as f64 / dt,
        (p - prev.2) as f64 / dt,
        s,
        l,
        pct(0.5),
        pct(0.99),
        c.throttled.load(Relaxed),
        c.failed.load(Relaxed),
    );
    *prev = (s, l, p, elapsed);
}

fn now_ns() -> u64 {
    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.rate <= 0.0 || args.batch == 0 || args.concurrency == 0 {
        bail!("rate, batch and concurrency must be positive");
    }
    let metrics_every = humantime::parse_duration(&args.metrics_interval)?.as_nanos() as u64;
    let sender = Arc::new(Sender {
        http: reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?,
        endpoint: args.endpoint.trim_end_matches('/').to_string(),
        token: args.token.clone(),
        gzip: args.gzip,
        sem: Arc::new(Semaphore::new(args.concurrency)),
        c: Arc::new(Counters::default()),
    });
    let mut rng = match args.seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => rand::make_rng(),
    };
    let started = Instant::now();
    let mut prev = (0, 0, 0, 0.0);
    let mut last_report = Instant::now();

    if let Some(bf) = &args.backfill {
        // Simulated time from now-bf to now, `rate` traces per simulated second.
        let span_ns = humantime::parse_duration(bf)?.as_nanos() as u64;
        let end = now_ns();
        let mut sim = end - span_ns;
        let mut mstate = MetricsState::new(sim);
        let mut next_metrics = sim + metrics_every;
        let step_ns = (args.batch as f64 / args.rate * 1e9) as u64;
        println!("backfilling {bf} at {} traces per simulated second ...", args.rate);
        while sim < end {
            let times: Vec<u64> = (0..args.batch).map(|_| sim + rng.random_range(0..step_ns.max(1))).collect();
            let mut out = gen_traces(&mut rng, args.batch, &times, &args);
            mstate.record(&out.requests);
            let (ns, nl) = (out.n_spans, out.n_logs);
            sender.send("/v1/traces", traces_request(&mut out).encode_to_vec(), ns, 0).await;
            if nl > 0 {
                sender.send("/v1/logs", logs_request(&mut out).encode_to_vec(), nl, 1).await;
            }
            sim += step_ns;
            while next_metrics <= sim.min(end) {
                let (m, n) = mstate.export(&mut rng, next_metrics);
                sender.send("/v1/metrics", m.encode_to_vec(), n, 2).await;
                next_metrics += metrics_every;
            }
            if last_report.elapsed() >= Duration::from_secs(5) {
                report(&sender.c, started.elapsed().as_secs_f64(), &mut prev);
                last_report = Instant::now();
            }
        }
    } else {
        let dur = humantime::parse_duration(&args.duration).unwrap_or(Duration::ZERO);
        let mut mstate = MetricsState::new(now_ns());
        let mut next_metrics = Instant::now() + Duration::from_nanos(metrics_every);
        let interval = Duration::from_secs_f64(args.batch as f64 / args.rate);
        let mut next = Instant::now();
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        println!(
            "sending {} traces/s to {} (batch {}, concurrency {})",
            args.rate, args.endpoint, args.batch, args.concurrency
        );
        loop {
            if !dur.is_zero() && started.elapsed() >= dur {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(next.into()) => {}
                _ = &mut ctrl_c => break,
            }
            next += interval;
            let base = now_ns();
            let spread = interval.as_nanos() as u64;
            let times: Vec<u64> =
                (0..args.batch).map(|_| base.saturating_sub(rng.random_range(0..spread.max(1)))).collect();
            let mut out = gen_traces(&mut rng, args.batch, &times, &args);
            mstate.record(&out.requests);
            let (ns, nl) = (out.n_spans, out.n_logs);
            sender.send("/v1/traces", traces_request(&mut out).encode_to_vec(), ns, 0).await;
            if nl > 0 {
                sender.send("/v1/logs", logs_request(&mut out).encode_to_vec(), nl, 1).await;
            }
            if Instant::now() >= next_metrics {
                let (m, n) = mstate.export(&mut rng, now_ns());
                sender.send("/v1/metrics", m.encode_to_vec(), n, 2).await;
                next_metrics += Duration::from_nanos(metrics_every);
            }
            if last_report.elapsed() >= Duration::from_secs(5) {
                report(&sender.c, started.elapsed().as_secs_f64(), &mut prev);
                last_report = Instant::now();
            }
        }
    }
    sender.drain(args.concurrency).await;
    let secs = started.elapsed().as_secs_f64();
    report(&sender.c, secs, &mut prev);
    let c = &sender.c;
    println!(
        "done in {:.1}s: {} spans, {} logs, {} metric points; avg {:.0} spans/s; {} requests, {:.1} MB sent, {} throttled, {} failed",
        secs,
        c.spans.load(Relaxed),
        c.logs.load(Relaxed),
        c.points.load(Relaxed),
        c.spans.load(Relaxed) as f64 / secs,
        c.requests.load(Relaxed),
        c.bytes.load(Relaxed) as f64 / 1e6,
        c.throttled.load(Relaxed),
        c.failed.load(Relaxed)
    );
    Ok(())
}
