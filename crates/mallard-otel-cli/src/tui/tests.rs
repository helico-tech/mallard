//! Rendering tests (TestBackend) and key handling with fixture data.

use chrono::{TimeDelta, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mallard_otel_client::{Client, api::*};
use ratatui::{Terminal, backend::TestBackend};

use super::*;
use crate::{tui::views::metrics::ChartState, waterfall::tests::span};

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

pub(super) fn stats() -> Stats {
    Stats {
        version: "0.1.0".into(),
        uptime_secs: 3725,
        ingest: IngestStats {
            signals: [Signal::Traces, Signal::Logs, Signal::Metrics]
                .into_iter()
                .map(|signal| SignalIngestStats {
                    signal,
                    requests: 10,
                    accepted: 1234,
                    throttled: 0,
                    throttled_requests: 0,
                    invalid: 0,
                })
                .collect(),
            buffered_rows: 120,
            buffered_bytes: 1_000_000,
            max_buffer_bytes: 512_000_000,
            flushes: 42,
            flush_errors: 0,
            dropped_rows: 0,
            last_flush: Some(FlushInfo { at: Utc::now(), rows: 500, duration_ms: 21.0 }),
            last_flush_error: None,
        },
        storage: StorageStats {
            catalog_type: CatalogType::DuckDb,
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
        capabilities: None,
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
    e.overview.set_services(loader::named(vec![
        ServiceSummary { name: "frontend".into(), red: red(1000, 12) },
        ServiceSummary { name: "checkout".into(), red: red(200, 20) },
        ServiceSummary { name: "payment".into(), red: red(50, 0) },
    ]));
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
    e.logs_tail_loaded(
        vec![
            log(1, 9, "order accepted", Some(TID)),
            log(2, 13, "slow request took 900ms", None),
            log(3, 17, "payment declined: card expired", Some(TID)),
        ],
        false,
    );
    e.metrics.set_list(loader::named(vec![MetricInfo {
        name: "app.request.duration".into(),
        metric_type: MetricType::Histogram,
        unit: "s".into(),
        description: String::new(),
        temporality: Temporality::Cumulative,
        series_count: 18,
        point_count: 360,
        services: vec!["checkout".into()],
        last_seen: now,
    }]));
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
    spans[2].status_code = SpanStatus::Error;
    let t2 = spans[2].start_time;
    spans[2].events.push(SpanEvent {
        time: t2,
        name: "exception".into(),
        attributes: [
            ("exception.type".to_string(), "PaymentDeclinedError".to_string()),
            ("exception.stacktrace".to_string(), "at charge (pay.go:42)\nat serve (server.go:9)".to_string()),
        ]
        .into(),
    });
    spans[0].attributes.insert("http.route".into(), "/checkout".into());
    for s in &mut spans {
        s.trace_id = TID.into();
    }
    Trace { trace_id: TID.into(), spans }
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
        "◆ telemetry",
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
    e.overview.set_operations(loader::named(vec![OperationSummary {
        service: "frontend".into(),
        name: "GET /".into(),
        kind: SpanKind::Server,
        red: red(10, 1),
    }]));
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
    let _ = e.handle_event(&ch('2'));
    assert_eq!(e.view, View::Traces);
    all_sizes(&mut e);
    let s = render(&mut e, 160, 40);
    for needle in ["Traces", "POST /checkout", "123.4ms", "✗ 2", "frontend payment"] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    // 'x' asks the host for errors of the selected trace
    assert_eq!(e.handle_event(&ch('x')), Outcome::Jump(TID.into()));
    // open the waterfall
    let _ = e.handle_event(&key(KeyCode::Enter));
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
    let _ = e.handle_event(&ch('G'));
    let s = render(&mut e, 160, 40);
    assert!(s.contains("PaymentDeclinedError"), "{s}");
    assert!(s.contains("at charge (pay.go:42)"), "{s}");
    assert_eq!(e.handle_event(&ch('x')), Outcome::Jump(TID.into()));
    // l -> logs of this trace
    let _ = e.handle_event(&ch('l'));
    assert_eq!(e.view, View::Logs);
    assert_eq!(e.logs.filter.trace.as_deref(), Some(TID));
    // back to the trace, esc returns to the list
    let _ = e.handle_event(&ch('2'));
    let _ = e.handle_event(&key(KeyCode::Esc));
    assert!(e.traces.detail.is_none());
}

#[test]
fn trace_filter_prompt() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('2'));
    let _ = e.handle_event(&ch('/'));
    assert!(e.prompt.is_some());
    let s = render(&mut e, 120, 30);
    assert!(s.contains("filter traces"), "{s}");
    for c in "service=payment errors".chars() {
        let _ = e.handle_event(&ch(c));
    }
    // keys go to the prompt, not to global shortcuts
    assert_eq!(e.view, View::Traces);
    let _ = e.handle_event(&key(KeyCode::Enter));
    assert!(e.prompt.is_none());
    assert_eq!(e.traces.filter.service.as_deref(), Some("payment"));
    assert!(e.traces.filter.errors_only);
    // invalid filter flashes an error and keeps the old one
    let _ = e.handle_event(&ch('/'));
    let _ = e.handle_event(&key(KeyCode::Char('u')).with_ctrl());
    for c in "bogus".chars() {
        let _ = e.handle_event(&ch(c));
    }
    let _ = e.handle_event(&key(KeyCode::Enter));
    assert!(e.flash.as_ref().is_some_and(|f| f.1));
    assert_eq!(e.traces.filter.service.as_deref(), Some("payment"));
    let _ = e.handle_event(&ch('E'));
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
    let _ = e.handle_event(&ch('3'));
    all_sizes(&mut e);
    let s = render(&mut e, 160, 40);
    for needle in ["Volume", "error", "Logs", "following", "payment declined: card expired", "WARN", "ERROR", "◆"] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    // follow stays at the end when new records arrive
    assert_eq!(e.logs.sel.selected(), Some(2));
    // (duplicates are removed by LogTail; see mallard-otel-client tests)
    e.logs_tail_loaded(vec![log(4, 9, "new one", None)], false);
    assert_eq!(e.logs.records.len(), 4);
    assert_eq!(e.logs.sel.selected(), Some(3));
    // a truncated poll inserts a visible gap marker before the new records
    e.logs_tail_loaded(vec![log(9, 9, "after a burst", None)], true);
    assert_eq!(e.logs.records.len(), 6);
    assert!(matches!(e.logs.records[4], crate::tui::views::logs::LogRow::Gap(_)));
    assert!(render(&mut e, 160, 40).contains("older records skipped"));
    assert_eq!(e.logs.sel.selected(), Some(5));
    // pause
    let _ = e.handle_event(&ch(' '));
    assert!(!e.logs.follow);
    // details popup (up past the new record and the gap marker)
    let _ = e.handle_event(&ch('k'));
    let _ = e.handle_event(&key(KeyCode::Enter));
    assert!(e.logs.detail.is_none(), "no details for the gap marker");
    let _ = e.handle_event(&ch('k'));
    let _ = e.handle_event(&ch('k'));
    let _ = e.handle_event(&key(KeyCode::Enter));
    let s = render(&mut e, 160, 40);
    assert!(s.contains("Log record"), "{s}");
    assert!(s.contains("order.id"), "{s}");
    // 'q' inside the popup closes it instead of quitting
    assert_eq!(e.handle_event(&ch('q')), Outcome::Consumed);
    assert!(e.logs.detail.is_none());
    // o on a record with a trace opens the trace
    let _ = e.handle_event(&ch('o'));
    assert_eq!(e.view, View::Traces);
    assert_eq!(e.traces.detail.as_ref().map(|d| d.trace_id.as_str()), Some(TID));
    // severity cycle rewrites the filter
    let _ = e.handle_event(&ch('3'));
    let _ = e.handle_event(&ch('s'));
    assert_eq!(e.logs.filter.level, Some(9));
    assert!(e.logs.records.is_empty(), "filter change reloads");
}

#[test]
fn metrics_render_and_chart() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('4'));
    all_sizes(&mut e);
    let s = render(&mut e, 140, 40);
    for needle in ["Metrics", "app.request.duration", "histogram", "18"] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    let _ = e.handle_event(&key(KeyCode::Enter));
    let c: &ChartState = e.metrics.chart.as_ref().unwrap();
    assert_eq!(c.aggs[c.agg], "p95");
    let now = Utc::now();
    e.metrics.set_result(MetricQueryResult {
        name: "app.request.duration".into(),
        metric_type: MetricType::Histogram,
        unit: "s".into(),
        agg: "p95".into(),
        step_secs: 60,
        series: vec![MetricSeries {
            labels: [("service.name".to_string(), "checkout".to_string())].into(),
            points: (0..30)
                .map(|i| MetricPoint { time: now - TimeDelta::minutes(30 - i), value: 0.1 + i as f64 / 100.0 })
                .collect(),
        }],
        exemplars: vec![],
    });
    all_sizes(&mut e);
    let s = render(&mut e, 140, 40);
    for needle in ["app.request.duration · histogram · s", "p95", "checkout", "last", "390.0ms"] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    let _ = e.handle_event(&ch('a'));
    let c = e.metrics.chart.as_ref().unwrap();
    assert_eq!(c.aggs[c.agg], "p50");
    let _ = e.handle_event(&ch('b'));
    for ch_ in "http.route".chars() {
        let _ = e.handle_event(&ch(ch_));
    }
    let _ = e.handle_event(&key(KeyCode::Enter));
    assert_eq!(e.metrics.chart.as_ref().unwrap().group_by, "http.route");
    let _ = e.handle_event(&key(KeyCode::Esc));
    assert!(e.metrics.chart.is_none());
    // live search
    let _ = e.handle_event(&ch('/'));
    for ch_ in "zzz".chars() {
        let _ = e.handle_event(&ch(ch_));
    }
    assert!(e.metrics.visible().is_empty());
    let _ = e.handle_event(&key(KeyCode::Esc));
    let _ = e.handle_event(&key(KeyCode::Esc));
    assert_eq!(e.metrics.visible().len(), 1);
}

#[test]
fn status_renders() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('5'));
    all_sizes(&mut e);
    let s = render(&mut e, 140, 40);
    for needle in
        ["Ingest", "Storage", "traces", "1.2k", "buffer", "duckdb", "spans", "100k", "compaction", "merged 12 files"]
    {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
    e.stats = None;
    e.stats_error = Some("cannot connect to the server".into());
    let s = render(&mut e, 100, 30);
    assert!(s.contains("cannot connect"), "{s}");
}

#[test]
fn status_says_what_the_server_allows() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('5'));
    e.stats.as_mut().unwrap().capabilities = Some(mallard_otel_client::api::Capabilities {
        sql: false,
        max_window_secs: 7 * 86_400,
        retention_secs: mallard_otel_client::api::RetentionSecs { traces: 0, logs: 0, metrics: 0 },
        flush_interval_secs: 5.0,
        ingest_auth: true,
    });
    all_sizes(&mut e);
    let s = render(&mut e, 140, 40);
    for needle in ["ad-hoc SQL off", "longest window 7d", "ingest token yes", "new data after about 5s"] {
        assert!(s.contains(needle), "missing {needle:?} in\n{s}");
    }
}

#[test]
fn the_last_row_of_a_full_trace_list_reads_more() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('2'));
    let t = e.traces.list[0].clone();
    e.traces.set_list(vec![t.clone(); 3]);
    let _ = e.handle_event(&ch('G'));
    assert_eq!(e.traces.limit(), 200, "a short list is all there is");
    e.traces.set_list(vec![t.clone(); 200]);
    let _ = e.handle_event(&ch('k'));
    assert_eq!(e.traces.limit(), 200, "not on the last row");
    let _ = e.handle_event(&ch('G'));
    assert_eq!(e.traces.limit(), 400);
    // The server's cap is said, not hidden.
    e.traces.pages = 4;
    e.traces.set_list(vec![t.clone(); 1000]);
    let _ = e.handle_event(&ch('k'));
    let _ = e.handle_event(&ch('G'));
    assert_eq!(e.traces.limit(), 1000);
    assert!(render(&mut e, 140, 40).contains("the first 1000 traces"));
    // Another list starts again at one page.
    e.set_trace_filter("errors");
    assert_eq!(e.traces.limit(), 200);
    e.traces.pages = 1;
    let _ = e.handle_event(&ch('t'));
    assert_eq!(e.traces.limit(), 200, "another window too");
}

/// With a runtime, because `refreshed` only moves when a load starts.
#[tokio::test]
async fn a_grown_trace_list_holds_still_until_r() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('2'));
    let long_ago = Instant::now() - Duration::from_secs(60);
    e.traces.refreshed = Some(long_ago);
    e.refresh_traces(false);
    assert_ne!(e.traces.refreshed, Some(long_ago), "one page stays live");
    assert!(!render(&mut e, 140, 40).contains("paused"));

    e.traces.pages = 1;
    e.traces.refreshed = Some(long_ago);
    e.refresh_traces(false);
    assert_eq!(e.traces.refreshed, Some(long_ago), "past it: no auto refresh");
    assert!(render(&mut e, 140, 40).contains("paused, r refreshes"));

    let _ = e.handle_event(&ch('r'));
    assert_eq!(e.traces.pages, 1, "r keeps the rows");
    e.refresh_traces(false);
    assert!(e.traces.refreshed.is_some_and(|t| t != long_ago), "r reads the whole list");
    let then = e.traces.refreshed;
    e.refresh_traces(false);
    assert_eq!(e.traces.refreshed, then, "once");
}

#[test]
fn a_trace_opens_near_a_time_on_a_span() {
    let mut e = fixture();
    let t = trace_fixture();
    let time = chrono::Utc::now() - chrono::TimeDelta::days(20);
    let span = t.spans.last().unwrap().span_id.clone();
    assert!(e.open_trace_at(&t.trace_id, Some(time), Some(&span)));
    assert_eq!(e.traces.detail.as_ref().unwrap().hint, Some(time));
    e.traces.set_trace(t);
    let d = e.traces.detail.as_ref().unwrap();
    assert_eq!(d.selected_span().map(|s| s.span_id.as_str()), Some(span.as_str()));
}

#[test]
fn status_shows_throttled_requests_in_every_layout() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('5'));
    e.stats.as_mut().unwrap().ingest.signals[0].throttled_requests = 7;
    // The narrowest side-by-side layout, and the widest stacked one.
    for (w, h) in [(120, 30), (119, 30)] {
        let s = render(&mut e, w, h);
        let header = s.lines().find(|l| l.contains("signal")).unwrap();
        for col in ["requests", "accepted", "throttled", "thr reqs", "invalid"] {
            assert!(header.contains(col), "{w} wide: missing {col:?} in\n{s}");
        }
        let traces = s.lines().find(|l| l.contains("traces")).unwrap();
        assert!(traces.contains('7'), "{w} wide:\n{s}");
    }
}

#[test]
fn global_keys() {
    let mut e = fixture();
    assert_eq!(e.handle_event(&key(KeyCode::F(1))), Outcome::Ignored);
    assert_eq!(e.handle_event(&key(KeyCode::F(2))), Outcome::Ignored);
    assert_eq!(e.handle_event(&ch('z')), Outcome::Ignored);
    assert_eq!(e.handle_event(&Event::FocusGained), Outcome::Ignored);
    let release =
        Event::Key(KeyEvent { kind: KeyEventKind::Release, ..KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE) });
    assert_eq!(e.handle_event(&release), Outcome::Ignored);
    assert_eq!(e.handle_event(&key(KeyCode::Tab)), Outcome::Consumed);
    assert_eq!(e.view, View::Traces);
    assert_eq!(e.handle_event(&key(KeyCode::BackTab)), Outcome::Consumed);
    assert_eq!(e.view, View::Overview);
    let before = e.since().to_string();
    let _ = e.handle_event(&ch('t'));
    assert_ne!(e.since(), before);
    let _ = e.handle_event(&ch('T'));
    assert_eq!(e.since(), before);
    let _ = e.handle_event(&ch('?'));
    assert!(e.help);
    let s = render(&mut e, 120, 40);
    assert!(s.contains("Keys"), "{s}");
    let _ = e.handle_event(&ch('x'));
    assert!(!e.help);
    assert_eq!(e.handle_event(&ch('q')), Outcome::Quit);
    assert_eq!(e.handle_event(&key(KeyCode::Char('c')).with_ctrl()), Outcome::Quit);
}

#[test]
fn open_trace_and_since() {
    let mut e = Embedded::with_since(Client::new("http://127.0.0.1:9", None).unwrap(), "2d").unwrap();
    assert_eq!(e.since(), "2d");
    assert_eq!(e.window_secs(), 172_800.0);
    e.open_trace("0AF7-6519-16cd43dd8448eb211c80319c");
    assert_eq!(e.view, View::Traces);
    assert_eq!(e.traces.detail.as_ref().unwrap().trace_id, TID);
    assert!(!e.open_trace("nope"));
    assert!(e.flash.as_ref().is_some_and(|f| f.1));
    assert!(!e.open_trace("0af7651916cd43dd8448eb211c80319z"), "not hex");
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
/// Run with: MALLARD_TEST_URL=http://localhost:8080 cargo test -p mallard-otel-cli live_snapshot -- --ignored --nocapture
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_snapshot() {
    let url = std::env::var("MALLARD_TEST_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let mut e = Embedded::with_since(Client::new(&url, None).unwrap(), "15m").unwrap();
    for (k, name) in [('1', "overview"), ('2', "traces"), ('3', "logs"), ('4', "metrics"), ('5', "status")] {
        let _ = e.handle_event(&ch(k));
        settle(&mut e).await;
        println!("===== {name} =====\n{}", render(&mut e, 150, 42));
    }
    // waterfall of the newest error trace
    let _ = e.handle_event(&ch('2'));
    let _ = e.handle_event(&ch('E'));
    settle(&mut e).await;
    let _ = e.handle_event(&key(KeyCode::Enter));
    settle(&mut e).await;
    let _ = e.handle_event(&ch('j'));
    println!("===== trace waterfall =====\n{}", render(&mut e, 150, 42));
    let _ = e.handle_event(&ch('G'));
    println!("===== deepest span =====\n{}", render(&mut e, 150, 42));
    // metric chart grouped by service
    let _ = e.handle_event(&ch('4'));
    let _ = e.handle_event(&ch('/'));
    for c in "duration".chars() {
        let _ = e.handle_event(&ch(c));
    }
    let _ = e.handle_event(&key(KeyCode::Enter));
    let _ = e.handle_event(&key(KeyCode::Enter));
    settle(&mut e).await;
    let _ = e.handle_event(&ch('b'));
    for c in "service.name".chars() {
        let _ = e.handle_event(&ch(c));
    }
    let _ = e.handle_event(&key(KeyCode::Enter));
    settle(&mut e).await;
    println!("===== metric chart =====\n{}", render(&mut e, 150, 42));
    // logs: errors only, then a record's details
    let _ = e.handle_event(&ch('3'));
    for _ in 0..3 {
        let _ = e.handle_event(&ch('s'));
    }
    settle(&mut e).await;
    let _ = e.handle_event(&key(KeyCode::Enter));
    println!("===== error logs + detail =====\n{}", render(&mut e, 150, 42));
    let _ = e.handle_event(&key(KeyCode::Esc));
    let _ = e.handle_event(&ch('1'));
    println!("===== overview 80x24 =====\n{}", render(&mut e, 80, 24));
}

// Review finding 4: only visible rows are built; scrolling still shows the
// selected record, at the end and in the middle of a long list.
#[test]
fn long_lists_scroll() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('3'));
    let many: Vec<LogRecord> = (0..5000).map(|i| log(i, 9, &format!("record-{i:04}"), None)).collect();
    e.logs_tail_loaded(many, false);
    e.logs.sel.select(Some(e.logs.records.len() - 1));
    let s = render(&mut e, 120, 30);
    assert!(s.contains("record-4999") && !s.contains("record-0000"), "{s}");
    e.logs.sel.select(Some(2500));
    let s = render(&mut e, 120, 30);
    assert!(s.contains("▌") && s.lines().any(|l| l.contains("▌") && l.contains("record-2500")), "{s}");
    // Waterfall: the selected span is drawn when scrolled.
    let spans: Vec<mallard_otel_client::api::Span> =
        (0..3000).map(|i| crate::waterfall::tests::span(&format!("{i:016}"), None, i, 1)).collect();
    let _ = e.handle_event(&ch('2'));
    e.traces.detail = Some(crate::tui::views::traces::Detail::new(TID.into(), None));
    e.traces.detail.as_mut().unwrap().requested = true;
    e.traces.set_trace(Trace { trace_id: TID.into(), spans });
    e.traces.detail.as_mut().unwrap().sel.select(Some(2999));
    let s = render(&mut e, 160, 40);
    assert!(s.lines().any(|l| l.contains("▌") && l.contains("op-0000000000002999")), "{s}");
}

// Review finding 13: windows parse like on the server; a bad one is refused
// and custom windows do not pile up.
#[test]
fn time_windows() {
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    e.set_since("1h30m").unwrap();
    assert_eq!(e.window_secs(), 5400.0);
    assert!(e.set_since("bogus").is_err());
    assert_eq!(e.since(), "1h30m", "a bad window changes nothing");
    for w in ["2h", "3h", "4h"] {
        e.set_since(w).unwrap();
    }
    assert_eq!(e.windows.len(), WINDOWS.len() + 1);
    e.set_since("6h").unwrap();
    assert_eq!(e.window_secs(), 6.0 * 3600.0);
}

// Review finding 18: a service without operations says so instead of
// "loading" forever.
#[test]
fn empty_operations_are_not_loading() {
    let mut e = fixture();
    let _ = e.handle_event(&key(KeyCode::Enter));
    assert!(e.overview.ops.is_some());
    assert!(render(&mut e, 120, 40).contains("loading operations"));
    e.overview.set_operations(vec![]);
    let s = render(&mut e, 120, 40);
    assert!(s.contains("no operations in this window"), "{s}");
}

#[test]
fn nav_on_empty_and_short_lists() {
    let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
    assert_eq!(nav(0, 0, &k(KeyCode::Down), 10), Some(0));
    assert_eq!(nav(0, 0, &k(KeyCode::Char('x')), 10), None);
    assert_eq!(nav(2, 3, &k(KeyCode::Down), 10), Some(2));
    assert_eq!(nav(2, 3, &k(KeyCode::PageUp), 10), Some(0));
    assert_eq!(nav(0, 3, &k(KeyCode::Char('G')), 0), Some(2));
}

/// Review 2.O: the keys both tabs share mean the same here as on the
/// errors tab. `x` jumps, `e` no longer does; `w` is no longer a second
/// window key (it is "raw frames" on the errors tab); `q` leaves a
/// drill-down instead of quitting; help lists the shared keys.
#[test]
fn shared_keys_mean_the_same_as_on_the_errors_tab() {
    let mut e = fixture();
    e.set_errors_enabled(true);
    let _ = e.handle_event(&ch('2'));
    assert_eq!(e.handle_event(&ch('e')), Outcome::Ignored);
    assert_eq!(e.handle_event(&ch('x')), Outcome::Jump(TID.into()));
    let since = e.since().to_string();
    assert_eq!(e.handle_event(&ch('w')), Outcome::Ignored);
    assert_eq!(e.since(), since);
    let _ = e.handle_event(&ch('t'));
    assert_ne!(e.since(), since, "t is the time window");
    let _ = e.handle_event(&key(KeyCode::Enter));
    assert!(e.traces.detail.is_some());
    assert_eq!(e.handle_event(&ch('q')), Outcome::Consumed, "q goes back from the waterfall");
    assert!(e.traces.detail.is_none());
    assert_eq!(e.handle_event(&ch('q')), Outcome::Quit, "q quits on the first screen");

    let _ = e.handle_event(&ch('?'));
    let s = render(&mut e, 140, 50);
    // On its own this tab has no app picker; in a host it does (`mallard tui`'s test).
    for (_, what) in mallard_term::tui::keys::shared(false) {
        assert!(s.contains(what), "missing {what:?} in\n{s}");
    }
    assert!(!s.contains("choose the app"), "{s}");
    assert!(s.contains("All times are UTC."), "{s}");
}

/// A host's error lookup with fixed answers; records what it was asked.
#[derive(Debug, Default)]
struct FakeErrors {
    asked: std::sync::Mutex<Vec<Vec<String>>>,
}

impl crate::ErrorLookup for FakeErrors {
    fn errors(
        &self,
        trace_ids: Vec<String>,
        _from: chrono::DateTime<Utc>,
        _to: chrono::DateTime<Utc>,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<crate::ErrorMap>> {
        self.asked.lock().unwrap().push(trace_ids);
        Box::pin(async { Ok([(TID.to_string(), sentry_errors())].into()) })
    }
}

fn sentry_errors() -> crate::TraceErrors {
    let ev = |id: &str| crate::ErrorEvent {
        event_id: id.into(),
        issue_id: "85fede14250ec6be".into(),
        span_id: Some("0000000000000003".into()),
        title: "PaymentDeclinedError: card expired".into(),
    };
    crate::TraceErrors { count: 2, events: vec![ev("e1"), ev("e2")] }
}

/// Without a lookup (no error tracker) the trace views stay as they were.
#[test]
fn no_error_lookup_no_sentry_markers() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('2'));
    assert!(!render(&mut e, 160, 40).contains("sentry"));
    let _ = e.handle_event(&key(KeyCode::Enter));
    e.traces.set_trace(trace_fixture());
    let _ = e.handle_event(&ch('G'));
    assert_eq!(e.handle_event(&ch('x')), Outcome::Jump(TID.into()));
}

/// The list shows the error count per trace, the waterfall marks the span
/// and `x` on it asks the host for that event.
#[tokio::test]
async fn sentry_errors_in_trace_list_and_waterfall() {
    let mut e = fixture();
    let fake = std::sync::Arc::new(FakeErrors::default());
    e.set_error_lookup(fake.clone());
    let _ = e.handle_event(&ch('2'));
    let list = e.traces.list.clone();
    e.apply(Kind::Traces, Data::Traces(list));
    let mut tries = 0;
    while e.traces.errors.is_empty() && tries < 50 {
        tokio::task::yield_now().await;
        for l in e.loader.drain() {
            e.apply(l.kind, l.result.unwrap());
        }
        tries += 1;
    }
    assert_eq!(fake.asked.lock().unwrap()[0], vec![TID.to_string()], "one lookup for the whole list");
    let s = render(&mut e, 160, 40);
    assert!(s.contains("sentry") && s.contains("⚠ 2"), "{s}");
    all_sizes(&mut e);

    let _ = e.handle_event(&key(KeyCode::Enter));
    e.traces.set_trace(trace_fixture());
    e.traces.set_errors(Kind::SpanErrors, [(TID.to_string(), sentry_errors())].into());
    all_sizes(&mut e);
    let s = render(&mut e, 160, 40);
    assert!(s.contains("⚠ 2 sentry"), "{s}");
    let marked: Vec<&str> = s.lines().filter(|l| l.contains("⚠ Charge")).collect();
    assert_eq!(marked.len(), 1, "only the span with the event: {s}");
    // The root span has no event: x is the whole trace.
    assert_eq!(e.handle_event(&ch('x')), Outcome::Jump(TID.into()));
    let _ = e.handle_event(&ch('G'));
    let s = render(&mut e, 160, 40);
    assert!(s.contains("Sentry errors (2)") && s.contains("PaymentDeclinedError: card expired"), "{s}");
    assert_eq!(
        e.handle_event(&ch('x')),
        Outcome::JumpEvent { trace_id: TID.into(), event_id: "e1".into() },
        "the newest event of the span"
    );
}

/// A server that accepts connections and never answers.
async fn silent_server() -> (Client, tokio::net::TcpListener) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let c = Client::new(&format!("http://{}", l.local_addr().unwrap()), None).unwrap();
    (c, l)
}

/// A server that answers every request with `[]` or `{}` (by path), and keeps the
/// request lines.
async fn recording_server() -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let c = Client::new(&format!("http://{}", l.local_addr().unwrap()), None).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = l.accept().await.unwrap();
            let s = s.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = conn.read(&mut buf).await else { return };
                let req = String::from_utf8_lossy(&buf[..n]);
                let line = req.lines().next().unwrap_or("").to_string();
                let body = if line.contains("/attributes") {
                    r#"{"total_rows":1,"sampled_rows":1,"counted_from":"2026-01-01T00:00:00Z","items":[{"key":"http.route","count":3}]}"#.to_string()
                } else if line.contains("/series") {
                    r#"{"step_secs":60,"points":[]}"#.to_string()
                } else {
                    "[]".to_string()
                };
                s.lock().unwrap().push(line);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(resp.as_bytes()).await;
            });
        }
    });
    (c, seen)
}

fn app_scope(services: &[&str]) -> Scope {
    Scope::App { slug: "checkout".into(), project: None, services: services.iter().map(|s| s.to_string()).collect() }
}

fn typed(e: &mut Embedded, text: &str) {
    for c in text.chars() {
        assert_eq!(e.handle_event(&ch(c)), Outcome::Consumed, "{c:?} is typed");
    }
}

fn prompt_text(e: &Embedded) -> String {
    e.prompt.as_ref().unwrap().input.value.clone()
}

fn draw_to_string(e: &mut Embedded, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| e.draw(f, f.area())).unwrap();
    t.backend().buffer().content().iter().map(|c| c.symbol()).collect()
}

/// The server never answers: keys are handled at once and no text is lost.
#[tokio::test]
async fn typing_never_waits_on_a_server_that_never_answers() {
    let (client, _listener) = silent_server().await;
    let mut e = Embedded::new(client);
    e.view = View::Traces;
    e.tick();
    let started = Instant::now();
    assert_eq!(e.handle_event(&ch('/')), Outcome::Consumed);
    typed(&mut e, "serv");
    assert!(draw_to_string(&mut e, 100, 30).contains("service="), "built-in keys show at once");
    // Tab accepts; the fetch for its values starts on a tick and never comes back.
    assert_eq!(e.handle_event(&key(KeyCode::Tab)), Outcome::Consumed);
    assert_eq!(prompt_text(&e), "service=");
    tokio::time::sleep(Duration::from_millis(250)).await;
    e.tick();
    assert!(e.loader.is_pending(Kind::Suggest), "fetching");
    assert!(!e.loader.any_pending() || e.loader.is_pending(Kind::Traces), "no spinner of its own");
    typed(&mut e, "chec");
    tokio::time::sleep(Duration::from_millis(250)).await;
    e.tick();
    typed(&mut e, "kout");
    assert_eq!(prompt_text(&e), "service=checkout");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(e.handle_event(&key(KeyCode::Enter)), Outcome::Consumed);
    assert_eq!(e.traces.filter.service.as_deref(), Some("checkout"), "applied without an answer");
    assert!(!e.loader.is_pending(Kind::Suggest), "closing the bar drops the fetch");
}

/// A failed fetch costs a small note and nothing else.
#[tokio::test]
async fn a_failed_fetch_keeps_the_text_and_stays_quiet() {
    // Nothing listens here.
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    e.view = View::Logs;
    assert_eq!(e.handle_event(&ch('/')), Outcome::Consumed);
    typed(&mut e, "service=");
    tokio::time::sleep(Duration::from_millis(250)).await;
    e.tick();
    for _ in 0..100 {
        e.tick();
        if e.log_suggest.note().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(e.log_suggest.note(), Some("suggestions unavailable"));
    assert_eq!(prompt_text(&e), "service=");
    assert!(draw_to_string(&mut e, 100, 30).contains("suggestions unavailable"));
    typed(&mut e, "x");
    assert_eq!(prompt_text(&e), "service=x");
    assert!(!e.flash.as_ref().is_some_and(|f| f.0.contains("suggest")), "no flash about it");
}

#[tokio::test]
async fn suggestions_come_from_the_attributes_and_the_scope() {
    let (client, seen) = recording_server().await;
    let mut e = Embedded::new(client);
    e.set_scope(&app_scope(&["Checkout", "checkout"]));
    e.view = View::Logs;
    let _ = e.handle_event(&ch('/'));
    tokio::time::sleep(Duration::from_millis(250)).await;
    e.tick();
    for _ in 0..200 {
        e.tick();
        if e.log_suggest.items().iter().any(|i| i.label == "http.route=") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let labels: Vec<_> = e.log_suggest.items().iter().map(|i| i.label.clone()).collect();
    assert_eq!(labels, ["service=", "level=", "trace=", "http.route="]);
    let line = seen.lock().unwrap().iter().find(|l| l.contains("/attributes")).cloned().unwrap();
    assert!(line.contains("signal=logs"), "{line}");
    assert!(line.contains("service=Checkout&service=checkout"), "both names: {line}");
    // The values of service= inside an app need no server, and are that app's.
    typed(&mut e, "service=");
    tokio::time::sleep(Duration::from_millis(250)).await;
    e.tick();
    let labels: Vec<_> = e.log_suggest.items().iter().map(|i| i.label.clone()).collect();
    assert_eq!(labels, ["Checkout", "checkout"]);
    // Other window, other scope: the list asks again.
    let before = e.log_suggest.scope().to_string();
    let _ = e.handle_event(&key(KeyCode::Esc));
    e.set_since("6h").unwrap();
    let _ = e.handle_event(&ch('/'));
    e.tick();
    assert_ne!(e.log_suggest.scope(), before);
}

/// `op=` reads at most 24h, whatever the window.
#[tokio::test]
async fn operation_suggestions_read_at_most_a_day() {
    let (client, seen) = recording_server().await;
    let mut e = Embedded::new(client);
    e.set_since("7d").unwrap();
    e.view = View::Status;
    e.start_prompt(PromptTarget::TraceFilter, "filter traces", "", "op=");
    tokio::time::sleep(Duration::from_millis(250)).await;
    e.tick();
    for _ in 0..200 {
        if seen.lock().unwrap().iter().any(|l| l.contains("/operations")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let lines = seen.lock().unwrap().clone();
    let line = lines.iter().find(|l| l.contains("/operations")).unwrap_or_else(|| panic!("{lines:?}"));
    assert!(line.contains("since=24h"), "{line}");
}

/// The picker works on every screen, also over the log detail popup.
#[test]
fn p_is_the_hosts_on_the_log_detail_too() {
    let mut e = fixture();
    e.view = View::Logs;
    e.logs.detail = Some(0);
    assert_eq!(e.handle_event(&ch('p')), Outcome::Consumed, "on its own: the popup's");
    e.set_hosted(true);
    assert_eq!(e.handle_event(&ch('p')), Outcome::Ignored);
    assert_eq!(e.logs.detail, Some(0), "the popup stays until the choice changes something");
}

#[tokio::test]
async fn an_app_scopes_every_query() {
    let (client, seen) = recording_server().await;
    let mut e = Embedded::new(client);
    e.set_hosted(true);
    e.set_scope(&app_scope(&["a", "b"]));
    for view in [View::Overview, View::Traces, View::Logs, View::Metrics] {
        e.view = view;
        e.tick();
    }
    for _ in 0..200 {
        if seen.lock().unwrap().iter().any(|l| l.contains("/metrics")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let lines = seen.lock().unwrap().clone();
    for path in ["/spans/series", "/logs/series", "/traces", "/logs?", "/metrics"] {
        let line = lines.iter().find(|l| l.contains(path)).unwrap_or_else(|| panic!("{path}: {lines:?}"));
        assert!(line.contains("service=a&service=b"), "{line}");
    }
}

/// All apps: one service value for a filter, none otherwise; as it always was.
#[tokio::test]
async fn without_an_app_queries_are_as_before() {
    let (client, seen) = recording_server().await;
    let mut e = Embedded::new(client);
    e.set_trace_filter("service=checkout");
    e.view = View::Traces;
    e.tick();
    e.view = View::Overview;
    e.tick();
    for _ in 0..200 {
        if seen.lock().unwrap().iter().any(|l| l.contains("/spans/series")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let lines = seen.lock().unwrap().clone();
    let trace = lines.iter().find(|l| l.contains("/traces")).unwrap();
    assert!(trace.contains("service=checkout") && !trace.contains("service=checkout&service"), "{trace}");
    let series = lines.iter().find(|l| l.contains("/spans/series")).unwrap();
    assert!(!series.contains("service="), "{series}");
}

#[tokio::test]
async fn an_app_without_telemetry_says_so_instead_of_showing_everything() {
    let (client, seen) = recording_server().await;
    let mut e = Embedded::new(client);
    e.set_hosted(true);
    e.set_scope(&app_scope(&[]));
    e.tick();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s = draw_to_string(&mut e, 100, 30);
    assert!(s.contains("App checkout has no telemetry in the last 1h"), "{s}");
    assert!(s.contains("OTEL_SERVICE_NAME=checkout"), "{s}");
    let lines = seen.lock().unwrap().clone();
    assert!(lines.iter().all(|l| l.contains("/stats")), "only the status of the server: {lines:?}");
    // A trace opened by id is not scoped.
    assert!(e.open_trace(TID));
    assert!(!draw_to_string(&mut e, 100, 30).contains("has no telemetry"));
    e.tick();
    // Back to all apps.
    e.traces.detail = None;
    e.set_scope(&Scope::All);
    assert!(!draw_to_string(&mut e, 100, 30).contains("has no telemetry"));
}

/// In a host, `t`, `T` and `p` are the host's; on its own the tab keeps them.
#[tokio::test]
async fn hosted_window_and_picker_keys_go_to_the_host() {
    for view in [View::Overview, View::Traces, View::Logs, View::Metrics, View::Status] {
        let mut e = fixture();
        e.view = view;
        let since = e.since().to_string();
        assert_eq!(e.handle_event(&ch('p')), Outcome::Ignored, "{view:?}");
        e.set_hosted(true);
        for c in ['t', 'T', 'p'] {
            assert_eq!(e.handle_event(&ch(c)), Outcome::Ignored, "{view:?} {c}");
        }
        assert_eq!(e.since(), since, "the host sets it");
        e.set_hosted(false);
        assert_eq!(e.handle_event(&ch('t')), Outcome::Consumed, "{view:?}: standalone keeps t");
        assert_ne!(e.since(), since);
    }
    // In a filter these are text.
    let mut e = fixture();
    e.set_hosted(true);
    e.view = View::Traces;
    let _ = e.handle_event(&ch('/'));
    typed(&mut e, "tTp");
    assert_eq!(prompt_text(&e), "tTp");
    assert!(e.set_since("14d").is_ok());
    assert_eq!(e.since(), "14d", "the host's list is wider than this tab's");
}

#[test]
fn a_filter_cannot_disagree_with_the_picker() {
    let mut e = fixture();
    e.set_scope(&app_scope(&["checkout"]));
    e.set_trace_filter("service=other");
    assert_eq!(e.traces.filter.service, None, "refused");
    assert!(e.flash.as_ref().unwrap().0.contains("not in this app"));
    e.set_trace_filter("service=checkout errors");
    assert_eq!(e.traces.filter.service.as_deref(), Some("checkout"));
    e.set_log_filter("service=other");
    assert_eq!(e.logs.filter.service, None);
    e.set_log_filter("service=checkout level=warn \"two words\"");
    // Another app: the filter's service goes, the rest stays.
    e.set_scope(&app_scope(&["web"]));
    assert_eq!(e.traces.filter.service, None);
    assert!(e.traces.filter.errors_only);
    assert_eq!(e.logs.filter.service, None);
    assert_eq!(e.logs.query, "level=warn \"two words\"");
}

/// Services can't be filtered by the endpoint: the tab filters them.
#[test]
fn the_services_list_is_limited_to_the_app() {
    let mut e = fixture();
    e.set_scope(&app_scope(&["checkout"]));
    let summary = |n: &str| ServiceSummary { name: n.into(), red: red(1, 0) };
    e.apply(Kind::Services, Data::Services(loader::named(vec![summary("checkout"), summary("frontend")])));
    let names: Vec<_> = e.overview.services.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["checkout"]);
}

/// Help shows the picker only where it works.
#[test]
fn help_names_the_app_key_when_hosted() {
    let mut e = fixture();
    let _ = e.handle_event(&ch('?'));
    assert!(!draw_to_string(&mut e, 120, 40).contains("choose the app"));
    e.set_hosted(true);
    assert!(draw_to_string(&mut e, 120, 40).contains("choose the app"));
}

/// A suggestion's hostile text never reaches the frame.
#[test]
fn suggestion_text_is_cleaned_on_screen() {
    let mut e = fixture();
    e.view = View::Traces;
    let _ = e.handle_event(&ch('/'));
    let scope = e.trace_suggest.scope().to_string();
    let want = mallard_term::tui::suggest::Want { key: None, prefix: String::new() };
    e.trace_suggest.answer(&scope, &want, Some(vec![("k\u{202e}\x1b[2Jx".to_string(), 2)]), Instant::now());
    let s = draw_to_string(&mut e, 100, 30);
    assert!(!s.contains('\u{202e}') && !s.contains('\x1b'), "{s}");
    assert!(s.contains("k\\u{202e}"), "{s}");
}

/// Cleaning a response goes through JSON: it must give back the same values, or a whole
/// view would fail or lie (ADR 0006, issue 0146).
#[test]
fn every_kind_of_response_survives_cleaning() {
    use mallard_term::text::sanitize;
    fn same<T>(v: T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug + Clone,
    {
        assert_eq!(sanitize(v.clone(), true).unwrap(), v);
    }
    // One-line values come through the same way; a stack trace has line breaks to lose.
    let one_line = stats();
    assert_eq!(sanitize(one_line.clone(), false).unwrap(), one_line);
    let e = fixture();
    same(one_line);
    same(ServiceSummary { name: "s".into(), red: red(3, 1) });
    same(OperationSummary { service: "s".into(), name: "o".into(), kind: SpanKind::Client, red: red(3, 1) });
    same(e.overview.span_series.clone().unwrap());
    same(e.overview.log_series.clone().unwrap());
    same(e.traces.list.clone());
    same(trace_fixture());
    same(log(1, 9, "body", Some(TID)));
    for temporality in [Temporality::Unspecified, Temporality::Delta, Temporality::Cumulative] {
        same(MetricInfo {
            name: "m".into(),
            metric_type: MetricType::Summary,
            unit: "1".into(),
            description: "d".into(),
            temporality,
            series_count: u64::MAX,
            point_count: 1,
            services: vec!["s".into()],
            last_seen: Utc::now(),
        });
    }
    same(MetricQueryResult {
        name: "m".into(),
        metric_type: MetricType::Gauge,
        unit: "ms".into(),
        agg: "avg".into(),
        step_secs: 60,
        series: vec![MetricSeries {
            labels: [("k".to_string(), "v".to_string())].into(),
            points: vec![MetricPoint { time: Utc::now(), value: 0.1 + 0.2 }],
        }],
        exemplars: vec![],
    });
}

#[test]
fn an_error_from_the_server_is_cleaned() {
    let e = mallard_otel_client::Error::Api { status: 500, code: "x".into(), message: "bo\u{202e}om\x1b[2J".into() };
    let s = loader::short_error(&e);
    assert!(!s.contains('\u{202e}') && !s.contains('\x1b'), "{s}");
    assert!(s.contains("bo\\u{202e}om"), "{s}");
}
