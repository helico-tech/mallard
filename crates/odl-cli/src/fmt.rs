//! Human-friendly output for the CLI.
//!
//! The `*_text` functions build the text of one command's output; `emit`
//! writes it. Every value that comes from the server goes through [`clean`]
//! before it is styled: OTLP data is written by any app that can reach the
//! collector, and must not be able to send escape sequences to the
//! operator's terminal.

use std::{
    borrow::Cow,
    io::{IsTerminal, Write},
    sync::OnceLock,
};

use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use odl_client::api::*;
use owo_colors::OwoColorize;

/// Append one formatted line to a `String`.
macro_rules! put {
    ($out:ident, $($arg:tt)*) => {{
        $out.push_str(&format!($($arg)*));
        $out.push('\n');
    }};
}

/// Whether a stream may get ANSI colors: it is a terminal, `NO_COLOR` is
/// unset or empty, and `TERM` is not `dumb`.
fn colors_enabled(is_tty: bool) -> bool {
    is_tty
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

/// Whether stdout gets ANSI colors.
fn color() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| colors_enabled(std::io::stdout().is_terminal()))
}

/// Characters that must not reach a terminal from untrusted text: control
/// characters (they start escape sequences or move the cursor) except
/// newline and tab, and the bidi controls that reorder what is shown.
fn is_unsafe(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Make untrusted text safe to print: unsafe characters become visible
/// escapes (`\u{1b}`).
pub fn clean(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_unsafe) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if is_unsafe(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Escape what serde_json leaves raw in strings (DEL, C1 controls, bidi
/// controls) as `\uXXXX`. Pretty-printed JSON has no such characters outside
/// strings, so the result is the same JSON value.
pub(crate) fn json_safe(json: &str) -> Cow<'_, str> {
    if !json.chars().any(is_unsafe) {
        return Cow::Borrowed(json);
    }
    let mut out = String::with_capacity(json.len() + 8);
    for c in json.chars() {
        if is_unsafe(c) {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Remove ANSI escape sequences (CSI `ESC [ ... final`). Only our own
/// styling contains them: data is cleaned first.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' && it.peek() == Some(&'[') {
            it.next();
            for d in it.by_ref() {
                if ('@'..='~').contains(&d) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Write text to stdout, adding a final newline if missing. Colors are
/// stripped when stdout is not a terminal or `NO_COLOR` is set.
pub(crate) fn emit(text: &str) -> std::io::Result<()> {
    emit_to(&mut std::io::stdout().lock(), text, color())
}

fn emit_to(w: &mut impl Write, text: &str, color: bool) -> std::io::Result<()> {
    let text = if color { Cow::Borrowed(text) } else { Cow::Owned(strip_ansi(text)) };
    w.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        w.write_all(b"\n")?;
    }
    w.flush()
}

/// A red, bold label for stderr messages (plain when stderr gets no colors).
pub fn paint_err(s: &str) -> String {
    if colors_enabled(std::io::stderr().is_terminal()) { s.red().bold().to_string() } else { s.to_string() }
}

fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_style(UTF8_FULL_CONDENSED.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers.iter().map(|h| Cell::new(clean(h)).fg(Color::Cyan)));
    t
}

fn num(v: impl Into<f64>) -> Cell {
    Cell::new(compact(v.into())).set_alignment(CellAlignment::Right)
}

pub fn compact(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.1}G", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if a >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else if a == a.trunc() || a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

pub fn ms(v: f64) -> String {
    if v >= 60_000.0 {
        format!("{:.1}m", v / 60_000.0)
    } else if v >= 1000.0 {
        format!("{:.2}s", v / 1000.0)
    } else if v >= 1.0 {
        format!("{v:.1}ms")
    } else {
        format!("{:.0}µs", v * 1000.0)
    }
}

pub fn bytes(b: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", units[i]) } else { format!("{v:.1} {}", units[i]) }
}

fn err_cell(rate: f64) -> Cell {
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
    put!(out, "{} {} {}", "otel-ducklake".bold(), clean(&s.version), format!("({url})").dimmed());
    put!(
        out,
        "uptime {}   catalog {}   data {}   snapshot {}",
        uptime(s.uptime_secs),
        clean(&s.storage.catalog_type).cyan(),
        clean(&s.storage.data_path).cyan(),
        s.storage.snapshot_id
    );
    let i = &s.ingest;
    let mut t = table(&["signal", "requests", "accepted", "rejected"]);
    for sig in &i.signals {
        t.add_row(vec![
            Cell::new(clean(&sig.signal)),
            num(sig.requests as f64),
            num(sig.accepted as f64),
            num(sig.rejected as f64),
        ]);
    }
    put!(out, "\n{}\n{t}", "Ingest".bold());
    let fill = i.buffered_bytes as f64 / i.max_buffer_bytes.max(1) as f64;
    put!(
        out,
        "buffer {} rows / {} of {} ({:.0}%)   flushes {}   flush errors {}",
        i.buffered_rows,
        bytes(i.buffered_bytes as f64),
        bytes(i.max_buffer_bytes as f64),
        fill * 100.0,
        i.flushes,
        if i.flush_errors > 0 { i.flush_errors.red().to_string() } else { "0".green().to_string() }
    );
    if let Some(f) = &i.last_flush {
        put!(out, "last flush {} rows in {} at {}", f.rows, ms(f.duration_ms), f.at.format("%H:%M:%S"));
    }
    if let Some(e) = &i.last_flush_error {
        put!(out, "{} {}", "last flush error:".red(), clean(e));
    }
    let mut t = table(&["table", "rows", "inlined", "files", "size"]);
    for tb in &s.storage.tables {
        t.add_row(vec![
            Cell::new(clean(&tb.name)),
            num(tb.rows as f64),
            num(tb.inlined_rows as f64),
            num(tb.files as f64),
            Cell::new(bytes(tb.bytes as f64)).set_alignment(CellAlignment::Right),
        ]);
    }
    put!(out, "\n{}\n{t}", "Storage".bold());
    if let Some(m) = &s.storage.last_maintenance {
        let status = if m.ok { "ok".green().to_string() } else { "failed".red().to_string() };
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
            Cell::new(clean(&s.kind.to_lowercase())).fg(Color::DarkGrey),
        ];
        row.extend(red_cells(&s.red));
        t.add_row(row);
    }
    t.to_string()
}

pub(crate) fn traces_text(v: &[TraceSummary]) -> String {
    if v.is_empty() {
        return "no matching traces".into();
    }
    let mut t = table(&["start", "trace id", "root", "duration", "spans", "errors", "services"]);
    for tr in v {
        let root = match (&tr.root_service, &tr.root_name) {
            (Some(s), Some(n)) => format!("{}: {}", clean(s), clean(n)),
            _ => "(root not received)".into(),
        };
        let errs = if tr.error_count > 0 {
            Cell::new(tr.error_count).fg(Color::Red)
        } else {
            Cell::new(0).fg(Color::DarkGrey)
        };
        t.add_row(vec![
            Cell::new(tr.start_time.format("%m-%d %H:%M:%S%.3f")),
            Cell::new(clean(&tr.trace_id)).fg(Color::DarkGrey),
            Cell::new(root),
            Cell::new(ms(tr.duration_ms)).set_alignment(CellAlignment::Right),
            num(tr.span_count as f64),
            errs.set_alignment(CellAlignment::Right),
            Cell::new(clean(&tr.services.join(", "))),
        ]);
    }
    t.to_string()
}

/// Waterfall: indented span tree with a timeline bar, one line per entry.
/// Uses the TUI's layout, so spans with missing parents or parent loops are
/// shown too.
pub(crate) fn trace_lines(t: &Trace, show_attrs: bool) -> Vec<String> {
    let mut out = Vec::new();
    let Some(start) = t.spans.iter().map(|s| s.start_time).min() else {
        out.push(format!("{} {}  (no spans)", "trace".bold(), clean(&t.trace_id)));
        return out;
    };
    let (rows, end_ns) = crate::tui::waterfall::layout(&t.spans);
    out.push(format!(
        "{} {}  {}  {} spans  {}",
        "trace".bold(),
        clean(&t.trace_id),
        start.format("%Y-%m-%d %H:%M:%S%.3f UTC"),
        t.spans.len(),
        ms(end_ns as f64 / 1e6)
    ));
    let width = 40usize;
    for row in &rows {
        let s = &t.spans[row.span];
        let (a, len) = crate::tui::waterfall::bar(row.offset_ns, row.duration_ns, end_ns, width as u16);
        let (a, len) = (a as usize, len as usize);
        let bar = format!("{}{}{}", " ".repeat(a), "█".repeat(len), " ".repeat(width.saturating_sub(a + len)));
        let bar = if s.status_code == "ERROR" { bar.red().to_string() } else { bar.cyan().to_string() };
        let label = format!("{}{} {}", row.prefix, clean(&s.service_name).bold(), clean(&s.name));
        let err = if s.status_code == "ERROR" { format!(" {}", "✗".red()) } else { String::new() };
        out.push(format!("{bar} {:>9}  {label}{err}", ms(s.duration_ns as f64 / 1e6)));
        if show_attrs {
            let indent = " ".repeat(row.depth * 2 + 2);
            for (k, v) in &s.attributes {
                out.push(format!("{}  {indent}{} = {}", " ".repeat(width + 10), clean(k).dimmed(), clean(v)));
            }
            for e in &s.events {
                out.push(format!(
                    "{}  {indent}• {} {}",
                    " ".repeat(width + 10),
                    clean(&e.name).yellow(),
                    fmt_attrs(&e.attributes).dimmed()
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
        "ERROR" | "FATAL" => padded.red().bold().to_string(),
        "WARN" => padded.yellow().to_string(),
        "INFO" => padded.green().to_string(),
        _ => padded.dimmed().to_string(),
    }
}

pub(crate) fn log_line(r: &LogRecord) -> String {
    let trace = r.trace_id.as_deref().map(|t| format!(" trace={}", clean(&t[..8.min(t.len())]))).unwrap_or_default();
    format!(
        "{} {} {} {}{}",
        r.time.format("%m-%d %H:%M:%S%.3f").dimmed(),
        severity_label(r.severity_number, &r.severity_text),
        clean(&r.service_name).cyan(),
        clean(&r.body),
        trace.dimmed()
    )
}

pub(crate) fn metrics_text(v: &[MetricInfo]) -> String {
    if v.is_empty() {
        return "no metrics in this window".into();
    }
    let mut t = table(&["metric", "type", "unit", "series", "points", "services", "last seen"]);
    for m in v {
        let temporality = if m.temporality.is_empty() { String::new() } else { format!(" ({})", m.temporality) };
        t.add_row(vec![
            Cell::new(clean(&m.name)).fg(Color::White),
            Cell::new(clean(&format!("{}{temporality}", m.metric_type))),
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
        clean(&r.name).bold(),
        clean(&r.metric_type).dimmed(),
        clean(&r.agg).cyan(),
        r.step_secs,
        if r.unit.is_empty() { Cow::Borrowed("-") } else { clean(&r.unit) }
    );
    if r.series.is_empty() {
        put!(out, "no data");
        return out;
    }
    let mut t = table(&["series", "last", "min", "avg", "max", "trend"]);
    for s in &r.series {
        let vals: Vec<f64> = s.points.iter().map(|p| p.value).collect();
        let Some(&last) = vals.last() else {
            continue;
        };
        let label = if s.labels.is_empty() { "(all)".to_string() } else { fmt_attrs(&s.labels) };
        let (min, max) = min_max(&vals);
        let avg = vals.iter().sum::<f64>() / vals.len() as f64;
        t.add_row(vec![
            Cell::new(label),
            num(last),
            num(min),
            num(avg),
            num(max),
            Cell::new(sparkline(&vals[vals.len().saturating_sub(60)..])).fg(Color::Cyan),
        ]);
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
    format!("{t}\n{}", summary.dimmed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(ms(0.5), "500µs");
        assert_eq!(ms(12.34), "12.3ms");
        assert_eq!(ms(2500.0), "2.50s");
        assert_eq!(compact(12_345.0), "12.3k");
        assert_eq!(compact(3.0), "3");
        assert_eq!(bytes(1_500_000.0), "1.5 MB");
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

    #[test]
    fn clean_escapes_control_characters() {
        assert_eq!(clean("plain\ttext\nok"), "plain\ttext\nok");
        assert!(matches!(clean("plain"), Cow::Borrowed(_)));
        let c = clean(EVIL);
        assert!(!c.chars().any(is_unsafe), "{c:?}");
        assert!(c.contains("\\u{1b}]52"), "escapes stay visible: {c:?}");
        let j = json_safe("[\"\u{9b}x\u{202e}\"]");
        assert_eq!(j, "[\"\\u009bx\\u202e\"]");
        assert_eq!(serde_json::from_str::<Vec<String>>(&j).unwrap(), vec!["\u{9b}x\u{202e}"]);
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
        let mut span = crate::tui::waterfall::tests::span("1", None, 0, 1);
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

    struct BrokenPipe;
    impl Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // Finding 7: a closed pipe is an error for the caller, not an exit.
    #[test]
    fn emit_returns_write_errors() {
        let e = emit_to(&mut BrokenPipe, "x", false).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe);
        let mut buf = Vec::new();
        emit_to(&mut buf, "\u{1b}[31mred\u{1b}[0m", false).unwrap();
        assert_eq!(buf, b"red\n");
    }
}
