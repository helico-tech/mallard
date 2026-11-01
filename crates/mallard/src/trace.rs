//! Text view of a trace: the span tree, with each Sentry error and log
//! record shown under the span it happened in. Every string from the server
//! goes through `mallard_term::text::clean` (review 1.M1); times are UTC.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::time::Duration;

use anyhow::Result;
use mallard_client::TraceOverview;
use mallard_term::fmt::ms;
use mallard_term::text::clean;
use mallard_term::wait::{WaitArgs, until};
use mallard_term::window::WindowArgs;
use serde_json::{Map, Value};

use crate::Run;

/// Default window of `mallard trace`.
const WINDOW: Duration = Duration::from_secs(7 * 86_400);

/// `mallard trace <id>` / `traces show <id>`. Not found anywhere: exit 4
/// (with `--wait`: exit 8 when nothing arrived in time).
pub(crate) async fn run(r: &Run, trace_id: &str, window: &WindowArgs, wait: &WaitArgs) -> Result<()> {
    // The window is resolved on every try: with --wait it moves along.
    let fetch = || async {
        let w = window.resolve(WINDOW)?;
        let t = r
            .client
            .trace_overview_in(trace_id, w.from, w.to)
            .await
            .map_err(|e| crate::described(e, r.client.base_url()))?;
        anyhow::Ok((w, t))
    };
    let ((w, t), waited) = until(&r.output, wait, fetch, |(_, t)| !is_empty(t)).await?.split();
    let empty = is_empty(&t);
    if r.output.text() {
        r.output.out.line(&render(&t))?;
    } else {
        let mut v = concise(&t, r.output.raw)?;
        v["window"] = serde_json::to_value(w)?;
        r.output.object(&v)?;
    }
    waited?;
    if empty {
        return Err(mallard_term::error::not_found(format!("trace {} not found", t.trace_id)));
    }
    Ok(())
}

fn is_empty(t: &TraceOverview) -> bool {
    t.spans.is_empty() && t.errors.is_empty() && t.logs.is_empty()
}

/// The trace as JSON. Without `raw`, resource attributes (the same for
/// every span and log of a service) are listed once per service under
/// `resources` instead of on every span, and dropped from logs.
pub(crate) fn concise(t: &TraceOverview, raw: bool) -> Result<Value> {
    let mut v = serde_json::to_value(t)?;
    // Each span: how many of the trace's Sentry errors were captured in it.
    if let Some(spans) = v["spans"].as_array_mut() {
        for s in spans {
            let n = sentry_errors(t, s["span_id"].as_str().unwrap_or_default());
            s["sentry_errors"] = n.into();
        }
    }
    if raw {
        return Ok(v);
    }
    let mut resources = Map::new();
    if let Some(spans) = v["spans"].as_array_mut() {
        for s in spans {
            let service = s["service_name"].as_str().unwrap_or_default().to_string();
            if let Some(attrs) = s.as_object_mut().and_then(|o| o.remove("resource_attributes")) {
                resources.entry(service).or_insert(attrs);
            }
        }
    }
    if let Some(logs) = v["logs"].as_array_mut() {
        for l in logs {
            if let Some(o) = l.as_object_mut() {
                o.remove("resource_attributes");
            }
        }
    }
    v["resources"] = Value::Object(resources);
    Ok(v)
}

/// The Sentry errors captured in this span.
fn sentry_errors(t: &TraceOverview, span_id: &str) -> usize {
    t.errors.iter().filter(|e| e.span_id.as_deref() == Some(span_id)).count()
}

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
        let sentry = match sentry_errors(t, &s.span_id) {
            0 => String::new(),
            n => format!("  ⚠ {n}"),
        };
        let _ = writeln!(
            out,
            "{pad}{mark} {}  {}  {}  (+{})  {}{sentry}",
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
    fn concise_json_lists_resources_once() {
        let mut a = span("1", None);
        a.service_name = "checkout".into();
        a.resource_attributes.insert("host.name".into(), "h1".into());
        let mut b = span("2", Some("1"));
        b.service_name = "checkout".into();
        b.resource_attributes = a.resource_attributes.clone();
        let mut l = log(Some("1"));
        l.resource_attributes = a.resource_attributes.clone();
        let t = TraceOverview {
            trace_id: mallard_client::TraceId::parse("0af7651916cd43dd8448eb211c80319c").unwrap(),
            spans: vec![a, b],
            errors: vec![],
            logs: vec![l],
            logs_truncated: false,
            warnings: vec![],
        };
        let v = concise(&t, false).unwrap();
        assert!(v["spans"].as_array().unwrap().iter().all(|s| s.get("resource_attributes").is_none()));
        assert!(v["logs"][0].get("resource_attributes").is_none());
        assert_eq!(v["resources"]["checkout"]["host.name"], "h1");
        let raw = concise(&t, true).unwrap();
        assert!(raw["spans"][0].get("resource_attributes").is_some() && raw.get("resources").is_none());
    }

    fn error_in(span_id: &str) -> mallard_client::sdl_client::types::EventSummary {
        mallard_client::sdl_client::types::EventSummary {
            event_id: "e1".into(),
            project_id: 1,
            issue_id: "85fede14250ec6be".into(),
            timestamp: chrono::Utc::now(),
            received_at: chrono::Utc::now(),
            title: "TypeError: boom".into(),
            level: "error".into(),
            platform: "node".into(),
            environment: None,
            release: None,
            user: None,
            message: None,
            trace_id: Some("0af7651916cd43dd8448eb211c80319c".into()),
            span_id: Some(span_id.into()),
        }
    }

    #[test]
    fn spans_with_sentry_errors_are_marked() {
        let mut a = span("1", None);
        a.name = "GET /".into();
        let mut b = span("2", Some("1"));
        b.name = "charge".into();
        let t = TraceOverview {
            trace_id: mallard_client::TraceId::parse("0af7651916cd43dd8448eb211c80319c").unwrap(),
            spans: vec![a, b],
            errors: vec![error_in("2"), error_in("2")],
            logs: vec![],
            logs_truncated: false,
            warnings: vec![],
        };
        for raw in [false, true] {
            let v = concise(&t, raw).unwrap();
            assert_eq!(
                (v["spans"][0]["sentry_errors"].as_u64(), v["spans"][1]["sentry_errors"].as_u64()),
                (Some(0), Some(2))
            );
        }
        let out = render(&t);
        let line = |name: &str| out.lines().find(|l| l.contains(name)).unwrap().to_string();
        assert!(line("charge").ends_with("⚠ 2"), "{out}");
        assert!(!line("GET /").contains('⚠'), "{out}");
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
