//! Text view of a trace: the span tree, with each Sentry error and log
//! record shown under the span it happened in.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

use obs_client::TraceOverview;

pub fn render(t: &TraceOverview) -> String {
    let mut out = String::new();
    let start = t.spans.iter().map(|s| s.start_time).min();
    let end = t.spans.iter().map(|s| s.start_time + chrono::Duration::nanoseconds(s.duration_ns)).max();
    let _ = writeln!(out, "trace {}", t.trace_id);
    match (start, end) {
        (Some(s), Some(e)) => {
            let _ = writeln!(
                out,
                "  {}  {:.1} ms  {} spans  {} errors  {} logs\n",
                s.format("%Y-%m-%d %H:%M:%S%.3f UTC"),
                (e - s).num_microseconds().unwrap_or(0) as f64 / 1000.0,
                t.spans.len(),
                t.errors.len(),
                t.logs.len()
            );
        }
        _ => {
            let _ = writeln!(out, "  no spans found  {} errors  {} logs\n", t.errors.len(), t.logs.len());
        }
    }

    let ids: HashSet<&str> = t.spans.iter().map(|s| s.span_id.as_str()).collect();
    let mut children: BTreeMap<Option<&str>, Vec<usize>> = BTreeMap::new();
    for (i, s) in t.spans.iter().enumerate() {
        // Spans whose parent is missing are shown as roots.
        let parent = s.parent_span_id.as_deref().filter(|p| ids.contains(p));
        children.entry(parent).or_default().push(i);
    }
    for v in children.values_mut() {
        v.sort_by_key(|&i| t.spans[i].start_time);
    }

    let mut shown_errors = HashSet::new();
    let mut shown_logs = HashSet::new();
    let mut stack: Vec<(usize, usize)> = children.get(&None).into_iter().flatten().rev().map(|&i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        let s = &t.spans[i];
        let pad = "  ".repeat(depth);
        let offset = start.map(|st| (s.start_time - st).num_microseconds().unwrap_or(0) as f64 / 1000.0).unwrap_or(0.0);
        let mark = if s.status_code == "ERROR" { "✖" } else { "•" };
        let _ = writeln!(
            out,
            "{pad}{mark} {}  {}  {:.1} ms  (+{offset:.1} ms)  {}",
            s.service_name,
            s.name,
            s.duration_ns as f64 / 1e6,
            s.span_id
        );
        for (k, e) in t.errors.iter().enumerate() {
            if e.span_id.as_deref() == Some(s.span_id.as_str()) {
                shown_errors.insert(k);
                let _ = writeln!(out, "{pad}    ⚠ error  {}  (issue {}, event {})", e.title, e.issue_id, e.event_id);
            }
        }
        for (k, l) in t.logs.iter().enumerate() {
            if l.span_id.as_deref() == Some(s.span_id.as_str()) {
                shown_logs.insert(k);
                let _ = writeln!(out, "{pad}    ▏{} {}  {}", l.time.format("%H:%M:%S%.3f"), l.severity_text, l.body);
            }
        }
        if let Some(kids) = children.get(&Some(s.span_id.as_str())) {
            stack.extend(kids.iter().rev().map(|&k| (k, depth + 1)));
        }
    }

    let rest_errors: Vec<_> = t.errors.iter().enumerate().filter(|(k, _)| !shown_errors.contains(k)).collect();
    if !rest_errors.is_empty() {
        let _ = writeln!(out, "\nerrors not linked to a known span:");
        for (_, e) in rest_errors {
            let _ = writeln!(
                out,
                "  ⚠ {}  {}  (project {}, issue {}, event {})",
                e.timestamp.format("%H:%M:%S%.3f"),
                e.title,
                e.project_id,
                e.issue_id,
                e.event_id
            );
        }
    }
    let rest_logs: Vec<_> = t.logs.iter().enumerate().filter(|(k, _)| !shown_logs.contains(k)).collect();
    if !rest_logs.is_empty() {
        let _ = writeln!(out, "\nlogs not linked to a known span:");
        for (_, l) in rest_logs {
            let _ = writeln!(out, "  ▏{} {} {}  {}", l.time.format("%H:%M:%S%.3f"), l.service_name, l.severity_text, l.body);
        }
    }
    out
}
