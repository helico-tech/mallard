//! Rendering tests (TestBackend) and key handling with fixture data.

use chrono::{TimeDelta, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use odl_client::{Client, api::*};
use ratatui::{Terminal, backend::TestBackend};

use super::*;
use crate::tui::{views::metrics::ChartState, waterfall::tests::span};

fn key(c: KeyCode) -> Event {
    Event::Key(KeyEvent::new(c, KeyModifiers::NONE))
}

fn ch(c: char) -> Event {
    key(KeyCode::Char(c))
}

fn red(n: u64, e: u64) -> RedStats {
    RedStats {
        span_count: n,
        error_count: e,
        error_rate: e as f64 / n as f64,
        rate_per_sec: n as f64 / 3600.0,
        p50_ms: 12.5,
        p95_ms: 180.0,
        p99_ms: 420.0,
        max_ms: 900.0,
    }
}

fn stats() -> Stats {
    Stats {
        version: "0.1.0".into(),
        uptime_secs: 3725,
        ingest: IngestStats {
            signals: ["traces", "logs", "metrics"]
                .iter()
                .map(|s| SignalIngestStats {
                    signal: s.to_string(),
                    requests: 10,
                    accepted: 1234,
                    rejected: 0,
                })
                .collect(),
            buffered_rows: 120,
            buffered_bytes: 1_000_000,
            max_buffer_bytes: 512_000_000,
            flushes: 42,
            flush_errors: 0,
            last_flush: Some(FlushInfo {
                at: Utc::now(),
                rows: 500,
                duration_ms: 21.0,
            }),
            last_flush_error: None,
        },
        storage: StorageStats {
            catalog_type: "duckdb".into(),
            data_path: "data/lake/".into(),
            snapshot_id: 77,
            tables: vec![TableStats {
                name: "spans".into(),
                rows: 100_000,
                files: 4,
                bytes: 9_000_000,
                inlined_rows: 300,
            }],
            last_maintenance: Some(MaintenanceInfo {
                task: "compaction".into(),
                at: Utc::now(),
                duration_ms: 120.0,
                ok: true,
                detail: "merged 12 files into 1".into(),
            }),
        },
    }
}

fn log(i: i64, sev: u8, body: &str, trace: Option<&str>) -> LogRecord {
    use chrono::TimeZone;
    let t = Utc.timestamp_opt(1_761_900_000 + i, 0).unwrap();
    LogRecord {
        time: t,
        observed_time: t,
        service_name: "checkout".into(),
        severity_number: sev,
        severity_text: String::new(),
        body: body.into(),
        trace_id: trace.map(str::to_string),
        span_id: trace.map(|_| "0000000000000001".to_string()),
        event_name: String::new(),
        scope_name: "app".into(),
        attributes: [("order.id".to_string(), "42".to_string())].into(),
        resource_attributes: Default::default(),
    }
}

const TID: &str = "0af7651916cd43dd8448eb211c80319c";

fn fixture() -> Embedded {
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    e.stats = Some(stats());
    e.ingest_rate = Some(250.0);
    e.overview.set_services(vec![
        ServiceSummary {
            name: "frontend".into(),
            red: red(1000, 12),
        },
        ServiceSummary {
            name: "checkout".into(),
            red: red(200, 20),
        },
        ServiceSummary {
            name: "payment".into(),
            red: red(50, 0),
        },
    ]);
    let now = Utc::now();
    e.overview.span_series = Some(SpanSeries {
        step_secs: 60,
        points: (0..60)
            .map(|i| SpanSeriesPoint {
                time: now - TimeDelta::minutes(60 - i),
                count: 100 + (i as u64 % 7) * 10,
                errors: i as u64 % 3,
                p50_ms: 10.0,
                p95_ms: 100.0 + i as f64,
                p99_ms: 300.0,
            })
            .collect(),
    });
    let ls = LogSeries {
        step_secs: 60,
        points: (0..60)
            .map(|i| LogSeriesPoint {
                time: now - TimeDelta::minutes(60 - i),
                total: 50,
                debug: 5,
                info: 30,
                warn: 10,
                error: 5,
            })
            .collect(),
    };
    e.overview.log_series = Some(ls.clone());
    e.overview.refreshed = Some(Instant::now());
    e.traces.set_list(vec![TraceSummary {
        trace_id: TID.into(),
        start_time: now,
        duration_ms: 123.4,
        root_service: Some("frontend".into()),
        root_name: Some("POST /checkout".into()),
        span_count: 7,
        error_count: 2,
        services: vec!["frontend".into(), "payment".into()],
    }]);
    e.traces.refreshed = Some(Instant::now());
    e.logs.series = Some(ls);
    e.logs.loaded = true;
    e.logs.requested = true;
    e.logs.refreshed = Some(Instant::now());
    e.logs_tail_loaded(vec![
        log(1, 9, "order accepted", Some(TID)),
        log(2, 13, "slow request took 900ms", None),
        log(3, 17, "payment declined: card expired", Some(TID)),
    ]);
    e.metrics.set_list(vec![MetricInfo {
        name: "app.request.duration".into(),
        metric_type: "histogram".into(),
        unit: "s".into(),
        description: String::new(),
        temporality: "cumulative".into(),
        series_count: 18,
        point_count: 360,
        services: vec!["checkout".into()],
        last_seen: now,
    }]);
    e.metrics.refreshed = Some(Instant::now());
    e
}

fn trace_fixture() -> Trace {
    let mut spans = vec![
        span("0000000000000001", None, 0, 120),
        span("0000000000000002", Some("0000000000000001"), 10, 60),
        span("0000000000000003", Some("0000000000000002"), 20, 30),
    ];
    spans[0].service_name = "frontend".into();
    spans[0].name = "POST /checkout".into();
    spans[2].service_name = "payment".into();
    spans[2].name = "Charge".into();
    spans[2].status_code = "ERROR".into();
    let t2 = spans[2].start_time;
    spans[2].events.push(SpanEvent {
        time: t2,
        name: "exception".into(),
        attributes: [
            (
                "exception.type".to_string(),
                "PaymentDeclinedError".to_string(),
            ),
            (
                "exception.stacktrace".to_string(),
                "at charge (pay.go:42)\nat serve (server.go:9)".to_string(),
            ),
        ]
        .into(),
    });
    spans[0]
        .attributes
        .insert("http.route".into(), "/checkout".into());
    for s in &mut spans {
        s.trace_id = TID.into();
    }
    Trace {
        trace_id: TID.into(),
        spans,
    }
}

fn render(e: &mut Embedded, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| e.draw(f, f.area())).unwrap();
    let buf = t.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

const SIZES: [(u16, u16); 4] = [(60, 20), (80, 24), (120, 40), (200, 60)];

fn all_sizes(e: &mut Embedded) {
    for (w, h) in SIZES {
        render(e, w, h);
    }
    // tiny and degenerate
    render(e, 10, 3);
    render(e, 25, 8);
}

#[test]
fn overview_renders() {
    let mut e = fixture();
    all_sizes(&mut e);
    let s = render(&mut e, 160, 48);
    for needle in [
        "otel-ducklake",
        "Overview",
        "REQUESTS",
        "ERRORS",
        "P95 LATENCY",
        "Services",
        "frontend",
        "checkout",
        "Throughput",
        "Latency",
        "10.00%",
    ] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    // drill into operations
    assert_eq!(e.handle_event(&key(KeyCode::Enter)), Outcome::Consumed);
    assert!(e.overview.ops.is_some());
    e.overview.set_operations(vec![OperationSummary {
        service: "frontend".into(),
        name: "GET /".into(),
        kind: "SERVER".into(),
        red: red(10, 1),
    }]);
    let s = render(&mut e, 160, 48);
    assert!(s.contains("Operations"), "{s}");
    assert!(s.contains("GET /"));
    assert_eq!(e.handle_event(&key(KeyCode::Enter)), Outcome::Consumed);
    assert_eq!(e.view, View::Traces);
    assert_eq!(e.traces.filter.service.as_deref(), Some("frontend"));
    assert_eq!(e.traces.filter.operation.as_deref(), Some("GET /"));
}

#[test]
fn traces_render_and_waterfall() {
    let mut e = fixture();
    e.handle_event(&ch('2'));
    assert_eq!(e.view, View::Traces);
    all_sizes(&mut e);
    let s = render(&mut e, 160, 40);
    for needle in [
        "Traces",
        "POST /checkout",
        "123.4ms",
        "✗ 2",
        "frontend payment",
    ] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    // 'e' asks the host for errors of the selected trace
    assert_eq!(e.handle_event(&ch('e')), Outcome::OpenErrors(TID.into()));
    // open the waterfall
    e.handle_event(&key(KeyCode::Enter));
    assert!(e.traces.detail.is_some());
    let s = render(&mut e, 160, 40);
    assert!(s.contains("loading trace"), "{s}");
    e.traces.set_trace(trace_fixture());
    all_sizes(&mut e);
    let s = render(&mut e, 160, 40);
    for needle in [
        "Trace 0af7651916cd43dd8448eb211c80319c",
        "3 spans",
        "POST /checkout",
        "└─",
        "Charge",
        "█",
        "http.route",
        "120.0ms",
    ] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    // select the failing span: details show the exception
    e.handle_event(&ch('G'));
    let s = render(&mut e, 160, 40);
    assert!(s.contains("PaymentDeclinedError"), "{s}");
    assert!(s.contains("at charge (pay.go:42)"), "{s}");
    assert_eq!(e.handle_event(&ch('e')), Outcome::OpenErrors(TID.into()));
    // l -> logs of this trace
    e.handle_event(&ch('l'));
    assert_eq!(e.view, View::Logs);
    assert_eq!(e.logs.filter.trace.as_deref(), Some(TID));
    // back to the trace, esc returns to the list
    e.handle_event(&ch('2'));
    e.handle_event(&key(KeyCode::Esc));
    assert!(e.traces.detail.is_none());
}

#[test]
fn trace_filter_prompt() {
    let mut e = fixture();
    e.handle_event(&ch('2'));
    e.handle_event(&ch('/'));
    assert!(e.prompt.is_some());
    let s = render(&mut e, 120, 30);
    assert!(s.contains("filter traces"), "{s}");
    for c in "service=payment errors".chars() {
        e.handle_event(&ch(c));
    }
    // keys go to the prompt, not to global shortcuts
    assert_eq!(e.view, View::Traces);
    e.handle_event(&key(KeyCode::Enter));
    assert!(e.prompt.is_none());
    assert_eq!(e.traces.filter.service.as_deref(), Some("payment"));
    assert!(e.traces.filter.errors_only);
    // invalid filter flashes an error and keeps the old one
    e.handle_event(&ch('/'));
    e.handle_event(&key(KeyCode::Char('u')).with_ctrl());
    for c in "bogus".chars() {
        e.handle_event(&ch(c));
    }
    e.handle_event(&key(KeyCode::Enter));
    assert!(e.flash.as_ref().is_some_and(|f| f.1));
    assert_eq!(e.traces.filter.service.as_deref(), Some("payment"));
    e.handle_event(&ch('E'));
    assert!(!e.traces.filter.errors_only);
}

trait Ctrl {
    fn with_ctrl(self) -> Event;
}
impl Ctrl for Event {
    fn with_ctrl(self) -> Event {
        match self {
            Event::Key(mut k) => {
                k.modifiers = KeyModifiers::CONTROL;
                Event::Key(k)
            }
            e => e,
        }
    }
}

#[test]
fn logs_render_and_keys() {
    let mut e = fixture();
    e.handle_event(&ch('3'));
    all_sizes(&mut e);
    let s = render(&mut e, 160, 40);
    for needle in [
        "Volume",
        "error",
        "Logs",
        "following",
        "payment declined: card expired",
        "WARN",
        "ERROR",
        "◆",
    ] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    // follow stays at the end when new records arrive
    assert_eq!(e.logs.sel.selected(), Some(2));
    e.logs_tail_loaded(vec![
        log(4, 9, "new one", None),
        log(3, 17, "payment declined: card expired", Some(TID)),
    ]);
    assert_eq!(e.logs.records.len(), 4, "duplicate dropped");
    assert_eq!(e.logs.sel.selected(), Some(3));
    // pause
    e.handle_event(&ch(' '));
    assert!(!e.logs.follow);
    // details popup
    e.handle_event(&ch('k'));
    e.handle_event(&key(KeyCode::Enter));
    let s = render(&mut e, 160, 40);
    assert!(s.contains("Log record"), "{s}");
    assert!(s.contains("order.id"), "{s}");
    // 'q' inside the popup closes it instead of quitting
    assert_eq!(e.handle_event(&ch('q')), Outcome::Consumed);
    assert!(e.logs.detail.is_none());
    // o on a record with a trace opens the trace
    e.handle_event(&ch('o'));
    assert_eq!(e.view, View::Traces);
    assert_eq!(
        e.traces.detail.as_ref().map(|d| d.trace_id.as_str()),
        Some(TID)
    );
    // severity cycle rewrites the filter
    e.handle_event(&ch('3'));
    e.handle_event(&ch('s'));
    assert_eq!(e.logs.filter.level, Some(9));
    assert!(e.logs.records.is_empty(), "filter change reloads");
}

#[test]
fn metrics_render_and_chart() {
    let mut e = fixture();
    e.handle_event(&ch('4'));
    all_sizes(&mut e);
    let s = render(&mut e, 140, 40);
    for needle in ["Metrics", "app.request.duration", "histogram", "18"] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    e.handle_event(&key(KeyCode::Enter));
    let c: &ChartState = e.metrics.chart.as_ref().unwrap();
    assert_eq!(c.aggs[c.agg], "p95");
    let now = Utc::now();
    e.metrics.set_result(MetricQueryResult {
        name: "app.request.duration".into(),
        metric_type: "histogram".into(),
        unit: "s".into(),
        agg: "p95".into(),
        step_secs: 60,
        series: vec![MetricSeries {
            labels: [("service.name".to_string(), "checkout".to_string())].into(),
            points: (0..30)
                .map(|i| MetricPoint {
                    time: now - TimeDelta::minutes(30 - i),
                    value: 0.1 + i as f64 / 100.0,
                })
                .collect(),
        }],
    });
    all_sizes(&mut e);
    let s = render(&mut e, 140, 40);
    for needle in [
        "app.request.duration · histogram · s",
        "p95",
        "checkout",
        "last",
        "390.0ms",
    ] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    e.handle_event(&ch('a'));
    let c = e.metrics.chart.as_ref().unwrap();
    assert_eq!(c.aggs[c.agg], "p50");
    e.handle_event(&ch('b'));
    for ch_ in "http.route".chars() {
        e.handle_event(&ch(ch_));
    }
    e.handle_event(&key(KeyCode::Enter));
    assert_eq!(e.metrics.chart.as_ref().unwrap().group_by, "http.route");
    e.handle_event(&key(KeyCode::Esc));
    assert!(e.metrics.chart.is_none());
    // live search
    e.handle_event(&ch('/'));
    for ch_ in "zzz".chars() {
        e.handle_event(&ch(ch_));
    }
    assert!(e.metrics.visible().is_empty());
    e.handle_event(&key(KeyCode::Esc));
    e.handle_event(&key(KeyCode::Esc));
    assert_eq!(e.metrics.visible().len(), 1);
}

#[test]
fn status_renders() {
    let mut e = fixture();
    e.handle_event(&ch('5'));
    all_sizes(&mut e);
    let s = render(&mut e, 140, 40);
    for needle in [
        "Ingest",
        "Storage",
        "traces",
        "1234",
        "buffer",
        "duckdb",
        "spans",
        "100.0k",
        "compaction",
        "merged 12 files",
    ] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    e.stats = None;
    e.stats_error = Some("cannot connect to the server".into());
    let s = render(&mut e, 100, 30);
    assert!(s.contains("cannot connect"), "{s}");
}

#[test]
fn global_keys() {
    let mut e = fixture();
    assert_eq!(e.handle_event(&key(KeyCode::F(1))), Outcome::Ignored);
    assert_eq!(e.handle_event(&key(KeyCode::F(2))), Outcome::Ignored);
    assert_eq!(e.handle_event(&ch('z')), Outcome::Ignored);
    assert_eq!(e.handle_event(&Event::FocusGained), Outcome::Ignored);
    let release = Event::Key(KeyEvent {
        kind: KeyEventKind::Release,
        ..KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
    });
    assert_eq!(e.handle_event(&release), Outcome::Ignored);
    assert_eq!(e.handle_event(&key(KeyCode::Tab)), Outcome::Consumed);
    assert_eq!(e.view, View::Traces);
    assert_eq!(e.handle_event(&key(KeyCode::BackTab)), Outcome::Consumed);
    assert_eq!(e.view, View::Overview);
    let before = e.since();
    e.handle_event(&ch('t'));
    assert_ne!(e.since(), before);
    e.handle_event(&ch('T'));
    assert_eq!(e.since(), before);
    e.handle_event(&ch('?'));
    assert!(e.help);
    let s = render(&mut e, 120, 40);
    assert!(s.contains("Keys"), "{s}");
    e.handle_event(&ch('x'));
    assert!(!e.help);
    assert_eq!(e.handle_event(&ch('q')), Outcome::Quit);
    assert_eq!(
        e.handle_event(&key(KeyCode::Char('c')).with_ctrl()),
        Outcome::Quit
    );
}

#[test]
fn open_trace_and_since() {
    let mut e = Embedded::with_since(Client::new("http://127.0.0.1:9", None).unwrap(), "2d");
    assert_eq!(e.since(), "2d");
    assert_eq!(e.window_secs(), 172_800.0);
    e.open_trace("0AF7-6519-16cd43dd8448eb211c80319c");
    assert_eq!(e.view, View::Traces);
    assert_eq!(e.traces.detail.as_ref().unwrap().trace_id, TID);
    e.open_trace("nope");
    assert!(e.flash.as_ref().is_some_and(|f| f.1));
    // tick without a runtime must not panic
    e.tick();
}

#[tokio::test]
async fn tick_reports_connection_errors() {
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    for _ in 0..40 {
        e.tick();
        if e.stats_error.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(e.stats_error.is_some());
    let s = render(&mut e, 100, 30);
    assert!(s.contains("offline"), "{s}");
}

async fn settle(e: &mut Embedded) {
    for _ in 0..12 {
        e.tick();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Render every view against a live server and print the screens.
/// Run with: ODL_TEST_URL=http://localhost:8080 cargo test -p odl-cli live_snapshot -- --ignored --nocapture
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_snapshot() {
    let url = std::env::var("ODL_TEST_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let mut e = Embedded::with_since(Client::new(&url, None).unwrap(), "15m");
    for (k, name) in [
        ('1', "overview"),
        ('2', "traces"),
        ('3', "logs"),
        ('4', "metrics"),
        ('5', "status"),
    ] {
        e.handle_event(&ch(k));
        settle(&mut e).await;
        println!("===== {name} =====\n{}", render(&mut e, 150, 42));
    }
    // waterfall of the newest error trace
    e.handle_event(&ch('2'));
    e.handle_event(&ch('E'));
    settle(&mut e).await;
    e.handle_event(&key(KeyCode::Enter));
    settle(&mut e).await;
    e.handle_event(&ch('j'));
    println!("===== trace waterfall =====\n{}", render(&mut e, 150, 42));
    e.handle_event(&ch('G'));
    println!("===== deepest span =====\n{}", render(&mut e, 150, 42));
    // metric chart grouped by service
    e.handle_event(&ch('4'));
    e.handle_event(&ch('/'));
    for c in "duration".chars() {
        e.handle_event(&ch(c));
    }
    e.handle_event(&key(KeyCode::Enter));
    e.handle_event(&key(KeyCode::Enter));
    settle(&mut e).await;
    e.handle_event(&ch('b'));
    for c in "service.name".chars() {
        e.handle_event(&ch(c));
    }
    e.handle_event(&key(KeyCode::Enter));
    settle(&mut e).await;
    println!("===== metric chart =====\n{}", render(&mut e, 150, 42));
    // logs: errors only, then a record's details
    e.handle_event(&ch('3'));
    for _ in 0..3 {
        e.handle_event(&ch('s'));
    }
    settle(&mut e).await;
    e.handle_event(&key(KeyCode::Enter));
    println!(
        "===== error logs + detail =====\n{}",
        render(&mut e, 150, 42)
    );
    e.handle_event(&key(KeyCode::Esc));
    e.handle_event(&ch('1'));
    println!("===== overview 80x24 =====\n{}", render(&mut e, 80, 24));
}
