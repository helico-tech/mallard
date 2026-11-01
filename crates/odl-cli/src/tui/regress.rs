//! Regression tests for review findings: slow servers, retries, modifier
//! keys, the log tail window and dedupe, wide characters, overflow.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::{DateTime, TimeDelta, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use odl_client::{Client, LogTail, api::*};
use ratatui::{Terminal, backend::TestBackend};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::tui::waterfall::tests::span;

const TID: &str = "0af7651916cd43dd8448eb211c80319c";

fn ev(c: KeyCode, m: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(c, m))
}

/// Requests seen by the fake server: full path with query string.
type Hits = Arc<Mutex<Vec<String>>>;
/// (path without query, how many times this path was requested) -> (status, body, delay ms)
type Handler = Arc<dyn Fn(&str, usize) -> (u16, String, u64) + Send + Sync>;

async fn fake(h: Handler) -> (String, Hits) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let hits: Hits = Arc::new(Mutex::new(Vec::new()));
    let hc = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (h, hits) = (h.clone(), hc.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut n = 0;
                loop {
                    let k = s.read(&mut buf[n..]).await.unwrap_or(0);
                    if k == 0 {
                        return;
                    }
                    n += k;
                    if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let full = req.split_whitespace().nth(1).unwrap_or("").to_string();
                let path = full.split('?').next().unwrap().to_string();
                let nth = {
                    let mut v = hits.lock().unwrap();
                    v.push(full);
                    v.iter().filter(|p| p.split('?').next() == Some(path.as_str())).count()
                };
                let (status, body, delay) = h(&path, nth);
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    (url, hits)
}

fn count(hits: &Hits, path: &str) -> usize {
    hits.lock().unwrap().iter().filter(|p| p.split('?').next() == Some(path)).count()
}

fn rec(t: DateTime<Utc>, body: &str) -> LogRecord {
    LogRecord {
        time: t,
        observed_time: t,
        service_name: "api".into(),
        severity_number: 9,
        severity_text: "INFO".into(),
        body: body.into(),
        trace_id: Some(TID.into()),
        span_id: None,
        event_name: String::new(),
        scope_name: String::new(),
        attributes: Default::default(),
        resource_attributes: Default::default(),
    }
}

fn services_json() -> String {
    serde_json::to_string(&vec![ServiceSummary {
        name: "checkout".into(),
        red: RedStats {
            span_count: 1,
            error_count: 0,
            error_rate: 0.0,
            rate_per_sec: 1.0,
            p50_ms: 1.0,
            p95_ms: 2.0,
            p99_ms: 3.0,
            max_ms: 4.0,
        },
    }])
    .unwrap()
}

async fn settle(e: &mut Embedded, until: impl Fn(&Embedded) -> bool, max: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < max {
        e.tick();
        if until(e) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

fn render(e: &mut Embedded, w: u16, h: u16) -> Vec<Vec<String>> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| e.draw(f, f.area())).unwrap();
    let buf = t.backend().buffer().clone();
    (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect()
}

// 1: a server slower than the refresh interval still delivers, and refreshes
// don't pile up while a request is pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_server_delivers_without_piling_up() {
    let (url, hits) = fake(Arc::new(|path, _| match path {
        "/api/v1/services" => (200, services_json(), 400),
        "/api/v1/stats" => (503, "{}".into(), 0),
        _ => (200, "{\"step_secs\":60,\"points\":[]}".into(), 400),
    }))
    .await;
    let mut e = Embedded::new(Client::new(&url, None).unwrap());
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(900) {
        // Every tick looks "due", as with a refresh interval shorter than the
        // server's response time.
        e.overview.refreshed = None;
        e.tick();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(
        settle(&mut e, |e| !e.overview.services.is_empty(), Duration::from_secs(3)).await,
        "services never applied"
    );
    let n = count(&hits, "/api/v1/services");
    assert!(n <= 3, "{n} services requests: refreshes piled up");
}

// 2: the initial logs load is retried after an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_initial_failure_is_retried() {
    let now = Utc::now();
    let ok = serde_json::to_string(&vec![rec(now, "hello")]).unwrap();
    let (url, hits) = fake(Arc::new(move |path, nth| match path {
        "/api/v1/logs" if nth == 1 => (503, "{\"error\":\"busy\",\"message\":\"busy\"}".into(), 0),
        "/api/v1/logs" => (200, ok.clone(), 0),
        "/api/v1/logs/series" => (200, "{\"step_secs\":60,\"points\":[]}".into(), 0),
        _ => (503, "{}".into(), 0),
    }))
    .await;
    let mut e = Embedded::new(Client::new(&url, None).unwrap());
    e.handle_event(&ev(KeyCode::Char('3'), KeyModifiers::NONE));
    assert!(settle(&mut e, |e| e.logs.error.is_some(), Duration::from_secs(3)).await);
    assert!(e.logs.retry_at.is_some(), "a retry is scheduled");
    e.logs.retry_at = Some(Instant::now()); // don't wait the back-off in the test
    assert!(settle(&mut e, |e| e.logs.loaded, Duration::from_secs(3)).await, "never retried");
    assert_eq!(e.logs.records.len(), 1);
    assert!(count(&hits, "/api/v1/logs") >= 2);
}

// 3: a failed trace lookup is retried, and `r` refreshes an open trace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trace_detail_retry_and_refresh() {
    let trace = Trace { trace_id: TID.into(), spans: vec![span("0000000000000001", None, 0, 10)] };
    let ok = serde_json::to_string(&trace).unwrap();
    let path = format!("/api/v1/traces/{TID}");
    let p2 = path.clone();
    let (url, hits) = fake(Arc::new(move |p, nth| {
        if p == p2 && nth == 1 {
            (404, "{\"error\":\"not_found\",\"message\":\"trace not found\"}".into(), 0)
        } else if p == p2 {
            (200, ok.clone(), 0)
        } else if p == "/api/v1/logs" {
            (200, "[]".into(), 0)
        } else {
            (503, "{}".into(), 0)
        }
    }))
    .await;
    let mut e = Embedded::new(Client::new(&url, None).unwrap());
    e.open_trace(TID);
    let has_err = |e: &Embedded| e.traces.detail.as_ref().is_some_and(|d| d.error.is_some());
    assert!(settle(&mut e, has_err, Duration::from_secs(3)).await);
    e.traces.detail.as_mut().unwrap().retry_at = Some(Instant::now());
    let loaded = |e: &Embedded| e.traces.detail.as_ref().is_some_and(|d| d.trace.is_some());
    assert!(settle(&mut e, loaded, Duration::from_secs(3)).await, "never retried");
    let before = count(&hits, &path);
    e.handle_event(&ev(KeyCode::Char('r'), KeyModifiers::NONE));
    assert!(
        settle(&mut e, |_| count(&hits, &path) > before, Duration::from_secs(3)).await,
        "r did not refresh the open trace"
    );
}

// 4: Ctrl/Alt keys are left to the host; Ctrl-C quits even with help open.
#[test]
fn modifier_keys_are_ignored() {
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    e.handle_event(&ev(KeyCode::Char('3'), KeyModifiers::NONE));
    let level = e.logs.query.clone();
    assert_eq!(e.handle_event(&ev(KeyCode::Char('s'), KeyModifiers::CONTROL)), Outcome::Ignored);
    assert_eq!(e.logs.query, level, "Ctrl-S must not change the severity");
    assert_eq!(e.handle_event(&ev(KeyCode::Char('f'), KeyModifiers::CONTROL)), Outcome::Ignored);
    assert!(e.prompt.is_none());
    assert_eq!(e.handle_event(&ev(KeyCode::Char('k'), KeyModifiers::ALT)), Outcome::Ignored);
    // Shifted letters still work (E = errors only in traces).
    e.handle_event(&ev(KeyCode::Char('2'), KeyModifiers::NONE));
    assert_eq!(e.handle_event(&ev(KeyCode::Char('E'), KeyModifiers::SHIFT)), Outcome::Consumed);
    e.handle_event(&ev(KeyCode::Char('?'), KeyModifiers::NONE));
    assert!(e.help);
    assert_eq!(e.handle_event(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)), Outcome::Quit);
}

// 8: the tail window stays bounded (server-relative `since`) when nothing matches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logtail_window_stays_bounded() {
    let (url, hits) = fake(Arc::new(|_, _| (200, "[]".into(), 0))).await;
    let c = Client::new(&url, None).unwrap();
    let mut tail = LogTail::new(c, LogQuery::default(), Utc::now() - TimeDelta::hours(2));
    for _ in 0..3 {
        tail.poll().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let h = hits.lock().unwrap().clone();
    assert!(h[0].contains("from="), "first poll starts at `start`: {}", h[0]);
    for q in &h[1..] {
        assert!(!q.contains("from="), "{q}");
        let ms: u64 = q
            .split("since=")
            .nth(1)
            .and_then(|s| s.split(['&', 'm']).next())
            .and_then(|s| s.parse().ok())
            .expect("since=<n>ms");
        assert!(ms < 40_000, "window must stay ~lookback, got {ms}ms");
    }
}

// 9: identical records (same µs, same content) are not merged; repeats are not re-emitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logtail_keeps_identical_records() {
    let t = Utc::now();
    let two = serde_json::to_string(&vec![rec(t, "same"), rec(t, "same")]).unwrap();
    let three = serde_json::to_string(&vec![rec(t, "same"), rec(t, "same"), rec(t, "same")]).unwrap();
    let (url, _) = fake(Arc::new(move |_, nth| match nth {
        1 | 2 => (200, two.clone(), 0),
        _ => (200, three.clone(), 0),
    }))
    .await;
    let mut tail = LogTail::new(Client::new(&url, None).unwrap(), LogQuery::default(), t);
    assert_eq!(tail.poll().await.unwrap().len(), 2, "both identical records");
    assert_eq!(tail.poll().await.unwrap().len(), 0, "nothing new");
    assert_eq!(tail.poll().await.unwrap().len(), 1, "one more identical record");
    // Records the caller already shows are not returned again.
    let mut tail = LogTail::new(Client::new(&url, None).unwrap(), LogQuery::default(), t);
    tail.mark_seen(&[rec(t, "same"), rec(t, "same")]);
    // (the fake now answers with three identical records)
    assert_eq!(tail.poll().await.unwrap().len(), 1);
}

// 11: wide (CJK) names keep the waterfall's duration column aligned.
#[test]
fn waterfall_wide_names_align() {
    let mut a = span("0000000000000001", None, 0, 100);
    a.service_name = "frontend".into();
    let mut b = span("0000000000000002", Some("0000000000000001"), 10, 50);
    b.service_name = "支付服务".into();
    b.name = "处理付款请求并更新订单".into();
    let t = Trace { trace_id: TID.into(), spans: vec![a, b] };
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    e.view = View::Traces;
    e.traces.detail = Some(crate::tui::views::traces::Detail::new(TID.into(), None));
    e.traces.detail.as_mut().unwrap().requested = true;
    e.traces.set_trace(t);
    let grid = render(&mut e, 160, 20);
    // Column (cell x) where the duration ("…ms") ends on a span's row.
    let end_of = |row_marker: &str, dur: &str| -> Option<usize> {
        grid.iter().find_map(|row| {
            let line: String = row.concat();
            // Span rows are the ones with bars.
            (line.contains(row_marker) && line.contains(dur) && line.contains('█')).then(|| {
                // First cell run spelling `dur`; returns the cell x of its end.
                let n = dur.chars().count();
                (0..row.len().saturating_sub(n)).find(|&x| row[x..x + n].concat() == dur).map(|x| x + n - 1).unwrap()
            })
        })
    };
    let x1 = end_of("op-0000000000000001", "100.0ms").expect("root row");
    let x2 = end_of("支", "50.0ms").expect("CJK row must show its duration");
    assert_eq!(x1, x2, "duration columns misaligned");
}

// 13: `odl trace` shows every span, also with parent loops; empty traces don't panic.
#[test]
fn cli_trace_shows_cycles() {
    let a = span("0000000000000001", Some("0000000000000002"), 0, 10);
    let b = span("0000000000000002", Some("0000000000000001"), 1, 5);
    let c = span("0000000000000003", Some("0000000000000003"), 2, 1);
    let t = Trace { trace_id: TID.into(), spans: vec![a, b, c] };
    let lines = crate::fmt::trace_lines(&t, false);
    assert_eq!(lines.len(), 4, "header + 3 spans: {lines:#?}");
    let empty = Trace { trace_id: TID.into(), spans: vec![] };
    assert_eq!(crate::fmt::trace_lines(&empty, true).len(), 1);
}

// 14: huge durations don't overflow.
#[test]
fn huge_durations_saturate() {
    let mut a = span("0000000000000001", None, 0, 10);
    let mut b = span("0000000000000002", Some("0000000000000001"), 5, 1);
    b.duration_ns = i64::MAX;
    a.duration_ns = i64::MAX;
    let (rows, total) = crate::tui::waterfall::layout(&[a.clone(), b.clone()]);
    assert_eq!(rows.len(), 2);
    assert_eq!(total, i64::MAX);
    let t = Trace { trace_id: TID.into(), spans: vec![a, b] };
    assert_eq!(crate::fmt::trace_lines(&t, false).len(), 3);
}

// 7: ANSI stripping for non-terminal output.
#[test]
fn strips_ansi() {
    assert_eq!(crate::fmt::strip_ansi("\u{1b}[2mdim\u{1b}[0m x \u{1b}[31;1mred\u{1b}[39m"), "dim x red");
}

// 12: the errors hint is hidden unless the host enables it.
#[test]
fn errors_hint_only_when_enabled() {
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    e.view = View::Traces;
    let n = |e: &mut Embedded| {
        let footer = render(e, 160, 20).last().unwrap().concat();
        footer.matches("errors").count()
    };
    let without = n(&mut e);
    e.set_errors_enabled(true);
    assert_eq!(n(&mut e), without + 1, "the e/errors hint appears only when enabled");
}

fn metric_result_json(agg: &str) -> String {
    serde_json::to_string(&MetricQueryResult {
        name: "m".into(),
        metric_type: "sum".into(),
        unit: String::new(),
        agg: agg.into(),
        step_secs: 60,
        series: vec![],
    })
    .unwrap()
}

// Review finding 2: changing the aggregation while a query is pending sends
// the new query at once, and the old aggregation's result is never shown
// under the new label.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregation_change_supersedes_pending_query() {
    let (url, hits) = fake(Arc::new(|path, _| match path {
        "/api/v1/metrics/query" => (200, metric_result_json("rate"), 600),
        _ => (503, "{}".into(), 0),
    }))
    .await;
    let mut e = Embedded::new(Client::new(&url, None).unwrap());
    e.view = View::Metrics;
    let info = MetricInfo {
        name: "m".into(),
        metric_type: "sum".into(),
        unit: String::new(),
        description: String::new(),
        temporality: "cumulative".into(),
        series_count: 1,
        point_count: 1,
        services: vec![],
        last_seen: Utc::now(),
    };
    let aggs = crate::tui::views::metrics::aggs_for(&info);
    assert_eq!(aggs[..2], ["rate", "increase"]);
    e.metrics.chart = Some(crate::tui::views::metrics::ChartState {
        info,
        aggs,
        agg: 0,
        group_by: String::new(),
        result: None,
        error: None,
        refreshed: None,
    });
    e.tick();
    assert!(settle(&mut e, |_| count(&hits, "/api/v1/metrics/query") == 1, Duration::from_secs(1)).await);
    e.handle_event(&ev(KeyCode::Char('a'), KeyModifiers::NONE));
    let sent = |_: &Embedded| hits.lock().unwrap().iter().any(|q| q.contains("agg=increase"));
    assert!(settle(&mut e, sent, Duration::from_millis(400)).await, "new aggregation not sent while the old one ran");
    // Both answers say "rate": neither may be shown as "increase".
    tokio::time::sleep(Duration::from_millis(900)).await;
    e.tick();
    assert!(e.metrics.chart.as_ref().unwrap().result.is_none(), "stale aggregation applied");
}

// Review finding 2: drilling from an operation into traces replaces a
// pending trace list for the old filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drill_down_supersedes_pending_traces() {
    let (url, hits) = fake(Arc::new(|path, _| match path {
        "/api/v1/traces" => (200, "[]".into(), 600),
        _ => (503, "{}".into(), 0),
    }))
    .await;
    let mut e = Embedded::new(Client::new(&url, None).unwrap());
    e.view = View::Traces;
    e.tick();
    assert!(settle(&mut e, |_| count(&hits, "/api/v1/traces") == 1, Duration::from_secs(1)).await);
    e.view = View::Overview;
    let red = RedStats {
        span_count: 1,
        error_count: 0,
        error_rate: 0.0,
        rate_per_sec: 1.0,
        p50_ms: 1.0,
        p95_ms: 1.0,
        p99_ms: 1.0,
        max_ms: 1.0,
    };
    let op = OperationSummary { service: "checkout".into(), name: "GET /cart".into(), kind: "SERVER".into(), red };
    e.overview.ops = Some(("checkout".into(), vec![op]));
    e.overview.ops_sel.select(Some(0));
    e.handle_event(&ev(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(e.view, View::Traces);
    let sent = |_: &Embedded| hits.lock().unwrap().iter().any(|q| q.contains("service=checkout"));
    assert!(settle(&mut e, sent, Duration::from_millis(400)).await, "filtered list not requested");
}

// Review finding 17: a failing kind flashes once, not on every retry, and a
// successful tail poll clears the old logs error.
#[test]
fn errors_flash_once_and_clear() {
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    let err = odl_client::Error::Api { status: 503, error: "busy".into(), message: "busy".into() };
    e.apply_error(Kind::LogTail, &err);
    assert!(e.flash.take().is_some());
    e.apply_error(Kind::LogTail, &err);
    assert!(e.flash.is_none(), "a retry that fails again must not flash");
    assert!(e.logs.error.is_some());
    e.apply(Kind::LogTail, Data::Tail((vec![], false)));
    assert!(e.logs.error.is_none(), "recovered tail still shows the old error");
    e.apply_error(Kind::LogTail, &err);
    assert!(e.flash.is_some(), "failing again after a success flashes");
}

// Review finding 19: a trace that is not found is retried with a growing pause.
#[test]
fn trace_lookup_backs_off() {
    let mut d = crate::tui::views::traces::Detail::new(TID.into(), None);
    let err = odl_client::Error::Api { status: 404, error: "not_found".into(), message: "trace not found".into() };
    let mut last = Duration::ZERO;
    for _ in 0..6 {
        d.on_error(&err);
        let wait = d.retry_at.unwrap() - Instant::now();
        assert!(wait + Duration::from_millis(50) >= last, "{wait:?} < {last:?}");
        last = wait;
    }
    assert!(last > Duration::from_secs(40) && last <= Duration::from_secs(60), "{last:?}");
    // A window the server refuses is retried at once with 24h.
    let mut d = crate::tui::views::traces::Detail::new(TID.into(), None);
    let too_large = odl_client::Error::Api {
        status: 400,
        error: "bad_request".into(),
        message: "larger than query.max_window".into(),
    };
    d.on_error(&too_large);
    assert!(d.short_window);
}

// Review finding 6: in a prompt, Ctrl/Alt keys go to the host instead of
// being typed, and Ctrl-C cancels the prompt instead of quitting.
#[test]
fn prompt_leaves_modifier_keys_to_the_host() {
    let mut e = Embedded::new(Client::new("http://127.0.0.1:9", None).unwrap());
    e.handle_event(&ev(KeyCode::Char('3'), KeyModifiers::NONE));
    e.handle_event(&ev(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(e.prompt.is_some());
    assert_eq!(e.handle_event(&ev(KeyCode::Char('s'), KeyModifiers::CONTROL)), Outcome::Ignored);
    assert_eq!(e.handle_event(&ev(KeyCode::Char('x'), KeyModifiers::ALT)), Outcome::Ignored);
    assert_eq!(e.prompt.as_ref().unwrap().input.value, "", "modifier keys were typed");
    assert_eq!(e.handle_event(&ev(KeyCode::Char('a'), KeyModifiers::NONE)), Outcome::Consumed);
    assert_eq!(e.handle_event(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)), Outcome::Consumed);
    assert!(e.prompt.is_none(), "Ctrl-C cancels the prompt");
    assert_eq!(e.handle_event(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)), Outcome::Quit);
}
