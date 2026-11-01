//! Text view of a trace: the span tree, with each Sentry error and log
//! record shown under the span it happened in. Every string from the server
//! goes through `mallard_term::text::clean` (review 1.M1); times are UTC.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

use mallard_client::TraceOverview;
use mallard_term::fmt::ms;
use mallard_term::text::clean;

pub fn render(t: &TraceOverview) -> String {
    let mut out = String::new();
    let start = t.spans.iter().map(|s| s.start_time).min();
    let end = t.spans.iter().map(|s| s.start_time + chrono::Duration::nanoseconds(s.duration_ns)).max();
    let _ = writeln!(out, "trace {}", t.trace_id);
    match (start, end) {
        (Some(s), Some(e)) => {
            let _ = writeln!(
                out,
                "  {}  {}  {} spans  {} errors  {} logs\n",
                s.format("%Y-%m-%d %H:%M:%S%.3f UTC"),
                ms((e - s).num_microseconds().unwrap_or(0) as f64 / 1000.0),
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
        let mark = if s.status_code == mallard_client::odl_client::api::SpanStatus::Error { "✖" } else { "•" };
        let _ = writeln!(
            out,
            "{pad}{mark} {}  {}  {}  (+{})  {}",
            clean(&s.service_name),
            clean(&s.name),
            ms(s.duration_ns as f64 / 1e6),
            ms(offset),
            clean(&s.span_id)
        );
        for (k, e) in t.errors.iter().enumerate() {
            if e.span_id.as_deref() == Some(s.span_id.as_str()) {
                shown_errors.insert(k);
                let _ = writeln!(
                    out,
                    "{pad}    ⚠ error  {}  (issue {}, event {})",
                    clean(&e.title),
                    clean(&e.issue_id),
                    clean(&e.event_id)
                );
            }
        }
        for (k, l) in t.logs.iter().enumerate() {
            if l.span_id.as_deref() == Some(s.span_id.as_str()) {
                shown_logs.insert(k);
                let _ = writeln!(
                    out,
                    "{pad}    ▏{} {}  {}",
                    l.time.format("%H:%M:%S%.3f"),
                    clean(&l.severity_text),
                    clean(&l.body)
                );
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
                clean(&e.title),
                e.project_id,
                clean(&e.issue_id),
                clean(&e.event_id)
            );
        }
    }
    let rest_logs: Vec<_> = t.logs.iter().enumerate().filter(|(k, _)| !shown_logs.contains(k)).collect();
    if !rest_logs.is_empty() {
        let _ = writeln!(out, "\nlogs not linked to a known span:");
        for (_, l) in rest_logs {
            let _ = writeln!(
                out,
                "  ▏{} {} {}  {}",
                l.time.format("%H:%M:%S%.3f"),
                clean(&l.service_name),
                clean(&l.severity_text),
                clean(&l.body)
            );
        }
    }
    if t.logs_truncated {
        let _ = writeln!(out, "\n(only the first {} log records are shown)", t.logs.len());
    }
    for w in &t.warnings {
        let _ = writeln!(out, "\nwarning: {}", clean(w));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mallard_client::odl_client::api::{LogRecord, Span, SpanKind, SpanStatus};

    /// OSC 52, a screen clear, a C1 CSI, a newline and a bidi override.
    const EVIL: &str = "a\x1b]52;c;ZXZpbA==\x07b\x1b[2Jc\u{9b}31m\nd\u{202e}e";

    fn span(id: &str, parent: Option<&str>) -> Span {
        Span {
            trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
            span_id: id.into(),
            parent_span_id: parent.map(str::to_string),
            name: EVIL.into(),
            service_name: EVIL.into(),
            kind: SpanKind::Server,
            start_time: chrono::Utc::now(),
            duration_ns: 1_500_000,
            status_code: SpanStatus::Error,
            status_message: EVIL.into(),
            attributes: Default::default(),
            resource_attributes: Default::default(),
            events: vec![],
            links: vec![],
            trace_state: String::new(),
            scope_name: String::new(),
            scope_version: String::new(),
        }
    }

    fn log(span_id: Option<&str>) -> LogRecord {
        LogRecord {
            time: chrono::Utc::now(),
            observed_time: chrono::Utc::now(),
            service_name: EVIL.into(),
            severity_number: 9,
            severity_text: EVIL.into(),
            body: EVIL.into(),
            trace_id: None,
            span_id: span_id.map(str::to_string),
            event_name: String::new(),
            scope_name: String::new(),
            attributes: Default::default(),
            resource_attributes: Default::default(),
        }
    }

    #[test]
    fn server_text_cannot_reach_the_terminal() {
        let t = TraceOverview {
            trace_id: mallard_client::TraceId::parse("0af7651916cd43dd8448eb211c80319c").unwrap(),
            spans: vec![span("1", None), span(EVIL, Some("1"))],
            errors: vec![],
            logs: vec![log(Some("1")), log(None)],
            logs_truncated: false,
            warnings: vec![EVIL.into()],
        };
        let out = render(&t);
        let bad: Vec<char> = out.chars().filter(|c| (c.is_control() && *c != '\n') || *c == '\u{202e}').collect();
        assert!(bad.is_empty(), "{bad:?} in\n{out}");
        assert!(out.contains("1.5ms"), "durations use the shared format: {out}");
        assert_eq!(out.lines().filter(|l| l.contains("✖")).count(), 2, "one line per span: {out}");
    }
}
