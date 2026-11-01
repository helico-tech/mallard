//! Human-friendly output for the CLI.
//!
//! The `*_text` functions build the text of one command's output; `Out::line`
//! writes it. Every value that comes from the server goes through [`clean`]
//! before it is styled: OTLP data is written by any app that can reach the
//! collector, and must not be able to send escape sequences to the
//! operator's terminal.

use std::borrow::Cow;

use comfy_table::{Cell, CellAlignment, Color};
use mallard_term::color::{alert, bold, cyan, dim, green, red, yellow};
pub(crate) use mallard_term::fmt::{bytes, compact, ms};
pub(crate) use mallard_term::table::table;
pub(crate) use mallard_term::text::{clean, json_safe};
use odl_client::api::*;

/// Append one formatted line to a `String`.
macro_rules! put {
    ($out:ident, $($arg:tt)*) => {{
        $out.push_str(&format!($($arg)*));
        $out.push('\n');
    }};
}

fn num(v: impl Into<f64>) -> Cell {
    Cell::new(compact(v.into())).set_alignment(CellAlignment::Right)
}

pub(crate) fn err_cell(rate: f64) -> Cell {
    let c = Cell::new(format!("{:.2}%", rate * 100.0)).set_alignment(CellAlignment::Right);
    if rate >= 0.05 {
        c.fg(Color::Red)
    } else if rate > 0.0 {
        c.fg(Color::Yellow)
    } else {
        c.fg(Color::Green)
    }
}

fn red_cells(r: &RedStats) -> Vec<Cell> {
    vec![
        num(r.span_count as f64),
        Cell::new(format!("{}/s", compact(r.rate_per_sec))).set_alignment(CellAlignment::Right),
        err_cell(r.error_rate),
        Cell::new(ms(r.p50_ms)).set_alignment(CellAlignment::Right),
        Cell::new(ms(r.p95_ms)).set_alignment(CellAlignment::Right),
        Cell::new(ms(r.p99_ms)).set_alignment(CellAlignment::Right),
    ]
}

pub(crate) fn stats_text(s: &Stats, url: &str) -> String {
    let mut out = String::new();
    put!(out, "{} {} {}", bold("otel-ducklake"), clean(&s.version), dim(&format!("({url})")));
    put!(
        out,
        "uptime {}   catalog {}   data {}   snapshot {}",
        uptime(s.uptime_secs),
        cyan(&s.storage.catalog_type.to_string()),
        cyan(&clean(&s.storage.data_path)),
        s.storage.snapshot_id
    );
    let i = &s.ingest;
    let mut t = table(&["signal", "requests", "accepted", "throttled", "invalid"]);
    for sig in &i.signals {
        t.add_row(vec![
            Cell::new(sig.signal),
            num(sig.requests as f64),
            num(sig.accepted as f64),
            num(sig.throttled as f64),
            num(sig.invalid as f64),
        ]);
    }
    put!(out, "\n{}\n{t}", bold("Ingest"));
    let fill = i.buffered_bytes as f64 / i.max_buffer_bytes.max(1) as f64;
    put!(
        out,
        "buffer {} rows / {} of {} ({:.0}%)   flushes {}   flush errors {}",
        i.buffered_rows,
        bytes(i.buffered_bytes),
        bytes(i.max_buffer_bytes),
        fill * 100.0,
        i.flushes,
        if i.flush_errors > 0 { red(&i.flush_errors.to_string()) } else { green("0") }
    );
    if let Some(f) = &i.last_flush {
        put!(out, "last flush {} rows in {} at {}", f.rows, ms(f.duration_ms), f.at.format("%H:%M:%S"));
    }
    if let Some(e) = &i.last_flush_error {
        put!(out, "{} {}", red("last flush error:"), clean(e));
    }
    let mut t = table(&["table", "rows", "inlined", "files", "size"]);
    for tb in &s.storage.tables {
        t.add_row(vec![
            Cell::new(clean(&tb.name)),
            num(tb.rows as f64),
            num(tb.inlined_rows as f64),
            num(tb.files as f64),
            Cell::new(bytes(tb.bytes as u64)).set_alignment(CellAlignment::Right),
        ]);
    }
    put!(out, "\n{}\n{t}", bold("Storage"));
    if let Some(m) = &s.storage.last_maintenance {
        let status = if m.ok { green("ok") } else { red("failed") };
        put!(
            out,
            "last maintenance: {} {status} at {} — {}",
            clean(&m.task),
            m.at.format("%H:%M:%S"),
            clean(&m.detail)
        );
    }
    out
}

/// Server uptime, e.g. `5m 3s`, `2h 10m`, `3d 4h`.
pub(crate) fn uptime(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        3600..86400 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

/// Smallest and largest value (`(MAX, MIN)` when empty).
pub(crate) fn min_max(v: &[f64]) -> (f64, f64) {
    v.iter().fold((f64::MAX, f64::MIN), |(l, h), x| (l.min(*x), h.max(*x)))
}

pub(crate) fn services_text(v: &[ServiceSummary]) -> String {
    if v.is_empty() {
        return "no spans in this window".into();
    }
    let mut t = table(&["service", "spans", "rate", "errors", "p50", "p95", "p99"]);
    for s in v {
        let mut row = vec![Cell::new(clean(&s.name)).fg(Color::White)];
        row.extend(red_cells(&s.red));
        t.add_row(row);
    }
    t.to_string()
}

pub(crate) fn operations_text(v: &[OperationSummary]) -> String {
    if v.is_empty() {
        return "no spans in this window".into();
    }
    let mut t = table(&["service", "operation", "kind", "spans", "rate", "errors", "p50", "p95", "p99"]);
    for s in v {
        let mut row = vec![
            Cell::new(clean(&s.service)),
            Cell::new(clean(&s.name)),
            Cell::new(s.kind.as_str().to_lowercase()).fg(Color::DarkGrey),
        ];
        row.extend(red_cells(&s.red));
        t.add_row(row);
    }
    t.to_string()
}

/// `sentry`: the host's error count per trace (`None`: unknown), shown as a
/// `sentry` column; no column without a host lookup.
pub(crate) fn traces_text(v: &[TraceSummary], sentry: Option<&[Option<u64>]>) -> String {
    if v.is_empty() {
        return "no matching traces".into();
    }
    let mut head = vec!["start", "trace id", "root", "duration", "spans", "errors"];
    if sentry.is_some() {
        head.push("sentry");
    }
    head.push("services");
    let mut t = table(&head);
    for (i, tr) in v.iter().enumerate() {
        let root = match (&tr.root_service, &tr.root_name) {
            (Some(s), Some(n)) => format!("{}: {}", clean(s), clean(n)),
            _ => "(root not received)".into(),
        };
        let errs = if tr.error_count > 0 {
            Cell::new(tr.error_count).fg(Color::Red)
        } else {
            Cell::new(0).fg(Color::DarkGrey)
        };
        let mut row = vec![
            Cell::new(tr.start_time.format("%m-%d %H:%M:%S%.3f")),
            Cell::new(clean(&tr.trace_id)).fg(Color::DarkGrey),
            Cell::new(root),
            Cell::new(ms(tr.duration_ms)).set_alignment(CellAlignment::Right),
            num(tr.span_count as f64),
            errs.set_alignment(CellAlignment::Right),
        ];
        if let Some(s) = sentry {
            let cell = match s.get(i).copied().flatten() {
                Some(0) => Cell::new(0).fg(Color::DarkGrey),
                Some(n) => Cell::new(n).fg(Color::Red),
                None => Cell::new("?").fg(Color::DarkGrey),
            };
            row.push(cell.set_alignment(CellAlignment::Right));
        }
        row.push(Cell::new(clean(&tr.services.join(", "))));
        t.add_row(row);
    }
    t.to_string()
}

/// Waterfall: indented span tree with a timeline bar, one line per entry.
/// Uses the TUI's layout, so spans with missing parents or parent loops are
/// shown too.
pub(crate) fn trace_lines(t: &Trace, show_attrs: bool) -> Vec<String> {
    let mut out = Vec::new();
    let Some(start) = t.spans.iter().map(|s| s.start_time).min() else {
        out.push(format!("{} {}  (no spans)", bold("trace"), clean(&t.trace_id)));
        return out;
    };
    let (rows, end_ns) = crate::waterfall::layout(&t.spans);
    out.push(format!(
        "{} {}  {}  {} spans  {}",
        bold("trace"),
        clean(&t.trace_id),
        start.format("%Y-%m-%d %H:%M:%S%.3f UTC"),
        t.spans.len(),
        ms(end_ns as f64 / 1e6)
    ));
    let width = 40usize;
    for row in &rows {
        let s = &t.spans[row.span];
        let (a, len) = crate::waterfall::bar(row.offset_ns, row.duration_ns, end_ns, width as u16);
        let (a, len) = (a as usize, len as usize);
        let bar = format!("{}{}{}", " ".repeat(a), "█".repeat(len), " ".repeat(width.saturating_sub(a + len)));
        let bar = if s.status_code == SpanStatus::Error { red(&bar) } else { cyan(&bar) };
        let label = format!("{}{} {}", row.prefix, bold(&clean(&s.service_name)), clean(&s.name));
        let err = if s.status_code == SpanStatus::Error { format!(" {}", red("✗")) } else { String::new() };
        out.push(format!("{bar} {:>9}  {label}{err}", ms(s.duration_ns as f64 / 1e6)));
        if show_attrs {
            let indent = " ".repeat(row.depth * 2 + 2);
            for (k, v) in &s.attributes {
                out.push(format!("{}  {indent}{} = {}", " ".repeat(width + 10), dim(&clean(k)), clean(v)));
            }
            for e in &s.events {
                out.push(format!(
                    "{}  {indent}• {} {}",
                    " ".repeat(width + 10),
                    yellow(&clean(&e.name)),
                    dim(&fmt_attrs(&e.attributes))
                ));
            }
        }
    }
    out
}

/// `k=v k=v`, cleaned.
fn fmt_attrs(a: &Attributes) -> String {
    a.iter().map(|(k, v)| format!("{}={}", clean(k), clean(v))).collect::<Vec<_>>().join(" ")
}

fn severity_label(n: u8, text: &str) -> String {
    let name = crate::severity::name(n, text);
    let padded = format!("{name:<5}");
    match name {
        "ERROR" | "FATAL" => alert(&padded),
        "WARN" => yellow(&padded),
        "INFO" => green(&padded),
        _ => dim(&padded),
    }
}

pub(crate) fn log_line(r: &LogRecord) -> String {
    let trace = r.trace_id.as_deref().map(|t| format!(" trace={}", clean(&t[..8.min(t.len())]))).unwrap_or_default();
    format!(
        "{} {} {} {}{}",
        dim(&r.time.format("%m-%d %H:%M:%S%.3f").to_string()),
        severity_label(r.severity_number, &r.severity_text),
        cyan(&clean(&r.service_name)),
        clean(&r.body),
        dim(&trace)
    )
}

pub(crate) fn metrics_text(v: &[MetricInfo]) -> String {
    if v.is_empty() {
        return "no metrics in this window".into();
    }
    let mut t = table(&["metric", "type", "unit", "series", "points", "services", "last seen"]);
    for m in v {
        let temporality =
            if m.temporality == Temporality::Unspecified { String::new() } else { format!(" ({})", m.temporality) };
        t.add_row(vec![
            Cell::new(clean(&m.name)).fg(Color::White),
            Cell::new(format!("{}{temporality}", m.metric_type)),
            Cell::new(clean(&m.unit)),
            num(m.series_count as f64),
            num(m.point_count as f64),
            Cell::new(clean(&m.services.join(", "))),
            Cell::new(m.last_seen.format("%H:%M:%S")),
        ]);
    }
    t.to_string()
}

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub fn sparkline(vals: &[f64]) -> String {
    let (lo, hi) = min_max(vals);
    vals.iter()
        .map(|v| if hi > lo { SPARK[(((v - lo) / (hi - lo)) * 7.0).round() as usize] } else { SPARK[3] })
        .collect()
}

pub(crate) fn metric_text(r: &MetricQueryResult) -> String {
    let mut out = String::new();
    put!(
        out,
        "{} {}  agg={}  step={}s  unit={}",
        bold(&clean(&r.name)),
        dim(&r.metric_type.to_string()),
        cyan(&clean(&r.agg)),
        r.step_secs,
        if r.unit.is_empty() { Cow::Borrowed("-") } else { clean(&r.unit) }
    );
    if r.series.is_empty() {
        put!(out, "no data");
        return out;
    }
    // Piped: no sparkline column.
    let fancy = mallard_term::table::fancy();
    let headers: &[&str] = if fancy {
        &["series", "last", "min", "avg", "max", "trend"]
    } else {
        &["series", "last", "min", "avg", "max"]
    };
    let mut t = table(headers);
    for s in &r.series {
        let vals: Vec<f64> = s.points.iter().map(|p| p.value).collect();
        let Some(&last) = vals.last() else {
            continue;
        };
        let label = if s.labels.is_empty() { "(all)".to_string() } else { fmt_attrs(&s.labels) };
        let (min, max) = min_max(&vals);
        let avg = vals.iter().sum::<f64>() / vals.len() as f64;
        let mut row = vec![Cell::new(label), num(last), num(min), num(avg), num(max)];
        if fancy {
            row.push(Cell::new(sparkline(&vals[vals.len().saturating_sub(60)..])).fg(Color::Cyan));
        }
        t.add_row(row);
    }
    put!(out, "{t}");
    out
}

pub(crate) fn sql_text(r: &SqlResponse) -> String {
    let headers: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
    let mut t = table(&headers);
    for row in &r.rows {
        t.add_row(row.iter().map(|v| match v {
            serde_json::Value::String(s) => Cell::new(clean(s)),
            serde_json::Value::Null => Cell::new("NULL").fg(Color::DarkGrey),
            serde_json::Value::Number(n) => Cell::new(n).set_alignment(CellAlignment::Right),
            // Nested values print as JSON: strings in it are escaped.
            other => Cell::new(json_safe(&other.to_string())),
        }));
    }
    let summary =
        format!("{} rows{} in {}", r.rows.len(), if r.truncated { " (truncated)" } else { "" }, ms(r.elapsed_ms));
    format!("{t}\n{}", dim(&summary))
}

/// Keys (or values of one key) with how many sampled rows have them.
pub(crate) fn attributes_text(s: &AttributeStats, key: Option<&str>) -> String {
    if s.items.is_empty() {
        return "no attributes in this window".into();
    }
    let mut t = table(&[if key.is_some() { "value" } else { "key" }, "rows"]);
    for a in &s.items {
        let name = if key.is_some() { a.value.as_deref().unwrap_or_default() } else { &a.key };
        t.add_row(vec![Cell::new(clean(name)).fg(Color::White), num(a.count as f64)]);
    }
    let sampled = if s.sampled_rows < s.total_rows {
        format!("counted in a sample of {} of {} rows", compact(s.sampled_rows as f64), compact(s.total_rows as f64))
    } else {
        format!("counted in {} rows", compact(s.total_rows as f64))
    };
    format!("{t}\n{}", dim(&sampled))
}

/// One row per column: table, column, type; the time column of each table
/// is marked.
pub(crate) fn schema_text(tables: &[SchemaTable]) -> String {
    let mut t = table(&["table", "column", "type", ""]);
    for tb in tables {
        for c in &tb.columns {
            let time = if c.name == tb.time_column { "time" } else { "" };
            t.add_row(vec![
                Cell::new(clean(&tb.name)).fg(Color::White),
                Cell::new(clean(&c.name)),
                Cell::new(clean(&c.data_type)),
                Cell::new(time).fg(Color::Cyan),
            ]);
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The number, size and duration cases moved to mallard-term's tests.
    #[test]
    fn formats() {
        assert_eq!(sparkline(&[0.0, 1.0]).chars().count(), 2);
    }

    /// Payloads an app could put in OTLP data: OSC 52 (clipboard), a CSI
    /// that clears the screen, an OSC 8 link, a C1 CSI, a carriage return
    /// and a bidi override.
    const EVIL: &str = "a\u{1b}]52;c;aGk=\u{7}b\u{1b}[2J\u{1b}]8;;http://x\u{1b}\\c\u{9b}31md\re\u{202e}f";

    /// Every escape sequence in `s` is our own styling: `ESC [ digits ; m`.
    fn only_sgr(s: &str) -> bool {
        let mut rest = s;
        while let Some(i) = rest.find('\u{1b}') {
            let seq = &rest[i + 1..];
            let Some(end) = seq.find('m') else { return false };
            if !seq.starts_with('[') || !seq[1..end].chars().all(|c| c.is_ascii_digit() || c == ';') {
                return false;
            }
            rest = &seq[end + 1..];
        }
        !s.chars().any(|c| c.is_control() && !matches!(c, '\u{1b}' | '\n' | '\t')) && !s.contains('\u{202e}')
    }

    // `clean` itself is tested in mallard-term (both former test sets).
    #[test]
    fn one_line_values_stay_on_one_line() {
        assert_eq!(log_line(&record("a\nb")).lines().count(), 1, "a log body cannot fake a second record");
    }

    fn record(body: &str) -> LogRecord {
        LogRecord {
            time: chrono::Utc::now(),
            observed_time: chrono::Utc::now(),
            service_name: format!("svc{EVIL}"),
            severity_number: 0,
            severity_text: EVIL.into(),
            body: body.into(),
            trace_id: Some(EVIL.into()),
            span_id: None,
            event_name: String::new(),
            scope_name: String::new(),
            attributes: Default::default(),
            resource_attributes: Default::default(),
        }
    }

    // Finding 3: data from the server cannot put escape sequences on the
    // terminal, also when colors are on.
    #[test]
    fn output_is_cleaned() {
        assert!(only_sgr(&log_line(&record(EVIL))), "{:?}", log_line(&record(EVIL)));
        let mut span = crate::waterfall::tests::span("1", None, 0, 1);
        span.name = EVIL.into();
        span.service_name = EVIL.into();
        span.attributes.insert(EVIL.into(), EVIL.into());
        let t = Trace { trace_id: EVIL.into(), spans: vec![span] };
        for l in trace_lines(&t, true) {
            assert!(only_sgr(&l), "{l:?}");
        }
        let sql = SqlResponse {
            columns: vec![SqlColumn { name: EVIL.into(), data_type: "VARCHAR".into() }],
            rows: vec![vec![serde_json::Value::String(EVIL.into())], vec![serde_json::json!([EVIL])]],
            truncated: false,
            elapsed_ms: 1.0,
        };
        assert!(only_sgr(&sql_text(&sql)), "{}", sql_text(&sql));
        let svc = ServiceSummary {
            name: EVIL.into(),
            red: RedStats {
                span_count: 1,
                error_count: 0,
                error_rate: 0.0,
                rate_per_sec: 1.0,
                p50_ms: 1.0,
                p95_ms: 1.0,
                p99_ms: 1.0,
                max_ms: 1.0,
            },
        };
        assert!(only_sgr(&services_text(&[svc])));
    }

    #[test]
    fn sentry_column_only_with_a_lookup() {
        let t = |id: &str| TraceSummary {
            trace_id: id.into(),
            start_time: chrono::Utc::now(),
            duration_ms: 1.0,
            root_service: Some("checkout".into()),
            root_name: Some("POST /pay".into()),
            span_count: 1,
            error_count: 0,
            services: vec!["checkout".into()],
        };
        let v = [t("a"), t("b"), t("c")];
        assert!(!traces_text(&v, None).contains("sentry"));
        let s = traces_text(&v, Some(&[Some(3), Some(0), None]));
        assert!(s.contains("sentry"), "{s}");
        let row = |id: &str| s.lines().find(|l| l.contains(&format!(" {id} "))).unwrap().to_string();
        assert!(row("a").contains(" 3 "), "{s}");
        assert!(row("c").contains(" ? "), "unknown when the lookup failed: {s}");
    }
}
