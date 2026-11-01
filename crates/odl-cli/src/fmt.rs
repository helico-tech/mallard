//! Human-friendly output for the CLI.

use std::collections::HashMap;

use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use odl_client::api::*;
use owo_colors::OwoColorize;

fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_style(UTF8_FULL_CONDENSED.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers.iter().map(|h| Cell::new(h).fg(Color::Cyan)));
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

pub fn print_stats(s: &Stats, url: &str) {
    println!("{} {} {}", "otel-ducklake".bold(), s.version, format!("({url})").dimmed());
    println!(
        "uptime {}   catalog {}   data {}   snapshot {}",
        humanize_secs(s.uptime_secs),
        s.storage.catalog_type.cyan(),
        s.storage.data_path.cyan(),
        s.storage.snapshot_id
    );
    let i = &s.ingest;
    let mut t = table(&["signal", "requests", "accepted", "rejected"]);
    for sig in &i.signals {
        t.add_row(vec![
            Cell::new(&sig.signal),
            num(sig.requests as f64),
            num(sig.accepted as f64),
            num(sig.rejected as f64),
        ]);
    }
    println!("\n{}\n{t}", "Ingest".bold());
    let fill = i.buffered_bytes as f64 / i.max_buffer_bytes.max(1) as f64;
    println!(
        "buffer {} rows / {} of {} ({:.0}%)   flushes {}   flush errors {}",
        i.buffered_rows,
        bytes(i.buffered_bytes as f64),
        bytes(i.max_buffer_bytes as f64),
        fill * 100.0,
        i.flushes,
        if i.flush_errors > 0 { i.flush_errors.red().to_string() } else { "0".green().to_string() }
    );
    if let Some(f) = &i.last_flush {
        println!("last flush {} rows in {} at {}", f.rows, ms(f.duration_ms), f.at.format("%H:%M:%S"));
    }
    if let Some(e) = &i.last_flush_error {
        println!("{} {e}", "last flush error:".red());
    }
    let mut t = table(&["table", "rows", "inlined", "files", "size"]);
    for tb in &s.storage.tables {
        t.add_row(vec![
            Cell::new(&tb.name),
            num(tb.rows as f64),
            num(tb.inlined_rows as f64),
            num(tb.files as f64),
            Cell::new(bytes(tb.bytes as f64)).set_alignment(CellAlignment::Right),
        ]);
    }
    println!("\n{}\n{t}", "Storage".bold());
    if let Some(m) = &s.storage.last_maintenance {
        let status = if m.ok { "ok".green().to_string() } else { "failed".red().to_string() };
        println!("last maintenance: {} {status} at {} — {}", m.task, m.at.format("%H:%M:%S"), m.detail);
    }
}

fn humanize_secs(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h{}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{}h", s / 86400, (s % 86400) / 3600),
    }
}

pub fn print_services(v: &[ServiceSummary]) {
    if v.is_empty() {
        println!("no spans in this window");
        return;
    }
    let mut t = table(&["service", "spans", "rate", "errors", "p50", "p95", "p99"]);
    for s in v {
        let mut row = vec![Cell::new(&s.name).fg(Color::White)];
        row.extend(red_cells(&s.red));
        t.add_row(row);
    }
    println!("{t}");
}

pub fn print_operations(v: &[OperationSummary]) {
    if v.is_empty() {
        println!("no spans in this window");
        return;
    }
    let mut t = table(&["service", "operation", "kind", "spans", "rate", "errors", "p50", "p95", "p99"]);
    for s in v {
        let mut row =
            vec![Cell::new(&s.service), Cell::new(&s.name), Cell::new(s.kind.to_lowercase()).fg(Color::DarkGrey)];
        row.extend(red_cells(&s.red));
        t.add_row(row);
    }
    println!("{t}");
}

pub fn print_traces(v: &[TraceSummary]) {
    if v.is_empty() {
        println!("no matching traces");
        return;
    }
    let mut t = table(&["start", "trace id", "root", "duration", "spans", "errors", "services"]);
    for tr in v {
        let root = match (&tr.root_service, &tr.root_name) {
            (Some(s), Some(n)) => format!("{s}: {n}"),
            _ => "(root not received)".into(),
        };
        let errs = if tr.error_count > 0 {
            Cell::new(tr.error_count).fg(Color::Red)
        } else {
            Cell::new(0).fg(Color::DarkGrey)
        };
        t.add_row(vec![
            Cell::new(tr.start_time.format("%m-%d %H:%M:%S%.3f")),
            Cell::new(&tr.trace_id).fg(Color::DarkGrey),
            Cell::new(root),
            Cell::new(ms(tr.duration_ms)).set_alignment(CellAlignment::Right),
            num(tr.span_count as f64),
            errs.set_alignment(CellAlignment::Right),
            Cell::new(tr.services.join(", ")),
        ]);
    }
    println!("{t}");
}

/// Waterfall: indented span tree with a timeline bar.
pub fn print_trace(t: &Trace, show_attrs: bool) {
    let start = t.spans.iter().map(|s| s.start_time).min().unwrap();
    let end_ns = t
        .spans
        .iter()
        .map(|s| (s.start_time - start).num_nanoseconds().unwrap_or(0) + s.duration_ns)
        .max()
        .unwrap_or(1)
        .max(1);
    println!(
        "{} {}  {}  {} spans  {}",
        "trace".bold(),
        t.trace_id,
        start.format("%Y-%m-%d %H:%M:%S%.3f UTC"),
        t.spans.len(),
        ms(end_ns as f64 / 1e6)
    );
    let ids: std::collections::HashSet<&str> = t.spans.iter().map(|s| s.span_id.as_str()).collect();
    let mut children: HashMap<Option<&str>, Vec<&Span>> = HashMap::new();
    for s in &t.spans {
        // Spans whose parent is missing are shown as roots.
        let parent = s.parent_span_id.as_deref().filter(|p| ids.contains(p));
        children.entry(parent).or_default().push(s);
    }
    let width = 40usize;
    fn walk(
        s: &Span,
        depth: usize,
        children: &HashMap<Option<&str>, Vec<&Span>>,
        start: chrono::DateTime<chrono::Utc>,
        end_ns: i64,
        width: usize,
        show_attrs: bool,
    ) {
        let off = (s.start_time - start).num_nanoseconds().unwrap_or(0);
        let a = (off as f64 / end_ns as f64 * width as f64) as usize;
        let len =
            ((s.duration_ns as f64 / end_ns as f64 * width as f64).ceil() as usize).clamp(1, width - a.min(width - 1));
        let bar =
            format!("{}{}{}", " ".repeat(a.min(width - 1)), "█".repeat(len), " ".repeat(width.saturating_sub(a + len)));
        let bar = if s.status_code == "ERROR" { bar.red().to_string() } else { bar.cyan().to_string() };
        let label = format!("{}{} {}", "  ".repeat(depth), s.service_name.bold(), s.name);
        let err = if s.status_code == "ERROR" { format!(" {}", "✗".red()) } else { String::new() };
        println!("{bar} {:>9}  {label}{err}", ms(s.duration_ns as f64 / 1e6));
        if show_attrs {
            for (k, v) in &s.attributes {
                println!("{}  {}{} = {}", " ".repeat(width + 10), "  ".repeat(depth + 1), k.dimmed(), v);
            }
            for e in &s.events {
                println!(
                    "{}  {}• {} {}",
                    " ".repeat(width + 10),
                    "  ".repeat(depth + 1),
                    e.name.yellow(),
                    fmt_attrs(&e.attributes).dimmed()
                );
            }
        }
        if let Some(kids) = children.get(&Some(s.span_id.as_str())) {
            for k in kids {
                walk(k, depth + 1, children, start, end_ns, width, show_attrs);
            }
        }
    }
    for root in children.get(&None).into_iter().flatten() {
        walk(root, 0, &children, start, end_ns, width, show_attrs);
    }
}

fn fmt_attrs(a: &Attributes) -> String {
    a.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

pub fn severity_label(n: u8, text: &str) -> String {
    let name = match n {
        0 => {
            if text.is_empty() {
                "-"
            } else {
                text
            }
        }
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        _ => "FATAL",
    };
    let padded = format!("{name:<5}");
    match n {
        17.. => padded.red().bold().to_string(),
        13..=16 => padded.yellow().to_string(),
        9..=12 => padded.green().to_string(),
        _ => padded.dimmed().to_string(),
    }
}

pub fn print_log(r: &LogRecord) {
    let trace = r.trace_id.as_deref().map(|t| format!(" trace={}", &t[..8.min(t.len())])).unwrap_or_default();
    println!(
        "{} {} {} {}{}",
        r.time.format("%m-%d %H:%M:%S%.3f").dimmed(),
        severity_label(r.severity_number, &r.severity_text),
        r.service_name.cyan(),
        r.body,
        trace.dimmed()
    );
}

pub fn print_metrics(v: &[MetricInfo]) {
    if v.is_empty() {
        println!("no metrics in this window");
        return;
    }
    let mut t = table(&["metric", "type", "unit", "series", "points", "services", "last seen"]);
    for m in v {
        t.add_row(vec![
            Cell::new(&m.name).fg(Color::White),
            Cell::new(format!(
                "{}{}",
                m.metric_type,
                if m.temporality.is_empty() { String::new() } else { format!(" ({})", m.temporality) }
            )),
            Cell::new(&m.unit),
            num(m.series_count as f64),
            num(m.point_count as f64),
            Cell::new(m.services.join(", ")),
            Cell::new(m.last_seen.format("%H:%M:%S")),
        ]);
    }
    println!("{t}");
}

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub fn sparkline(vals: &[f64]) -> String {
    let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    vals.iter()
        .map(|v| if hi > lo { SPARK[(((v - lo) / (hi - lo)) * 7.0).round() as usize] } else { SPARK[3] })
        .collect()
}

pub fn print_metric(r: &MetricQueryResult) {
    println!(
        "{} {}  agg={}  step={}s  unit={}",
        r.name.bold(),
        r.metric_type.dimmed(),
        r.agg.cyan(),
        r.step_secs,
        if r.unit.is_empty() { "-" } else { &r.unit }
    );
    if r.series.is_empty() {
        println!("no data");
        return;
    }
    let mut t = table(&["series", "last", "min", "avg", "max", "trend"]);
    for s in &r.series {
        let vals: Vec<f64> = s.points.iter().map(|p| p.value).collect();
        if vals.is_empty() {
            continue;
        }
        let label = if s.labels.is_empty() { "(all)".to_string() } else { fmt_attrs(&s.labels) };
        let (min, max) = vals.iter().fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
        let avg = vals.iter().sum::<f64>() / vals.len() as f64;
        t.add_row(vec![
            Cell::new(label),
            num(*vals.last().unwrap()),
            num(min),
            num(avg),
            num(max),
            Cell::new(sparkline(&vals[vals.len().saturating_sub(60)..])).fg(Color::Cyan),
        ]);
    }
    println!("{t}");
}

pub fn print_sql(r: &SqlResponse) {
    let headers: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
    let mut t = table(&headers);
    for row in &r.rows {
        t.add_row(row.iter().map(|v| match v {
            serde_json::Value::String(s) => Cell::new(s),
            serde_json::Value::Null => Cell::new("NULL").fg(Color::DarkGrey),
            serde_json::Value::Number(n) => Cell::new(n).set_alignment(CellAlignment::Right),
            other => Cell::new(other.to_string()),
        }));
    }
    println!("{t}");
    println!(
        "{}",
        format!("{} rows{} in {}", r.rows.len(), if r.truncated { " (truncated)" } else { "" }, ms(r.elapsed_ms))
            .dimmed()
    );
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
}
