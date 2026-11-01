//! Trace waterfall layout: span tree order, tree glyphs and bar geometry.

use std::collections::{HashMap, HashSet};

use odl_client::api::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Index into the trace's span list.
    pub span: usize,
    pub depth: usize,
    /// Tree glyphs to print before the name, e.g. "│ ├─ ".
    pub prefix: String,
    /// Nanoseconds from trace start.
    pub offset_ns: i64,
    pub duration_ns: i64,
}

/// Depth-first span order (children by start time). Spans whose parent is
/// missing from the trace are treated as roots. Returns rows and the total
/// trace duration in ns (>= 1).
pub fn layout(spans: &[Span]) -> (Vec<Row>, i64) {
    if spans.is_empty() {
        return (Vec::new(), 1);
    }
    let start = spans.iter().map(|s| s.start_time).min().unwrap();
    let off = |s: &Span| (s.start_time - start).num_nanoseconds().unwrap_or(0);
    let total = spans.iter().map(|s| off(s).saturating_add(s.duration_ns.max(0))).max().unwrap_or(1).max(1);
    let ids: HashSet<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();
    let mut children: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
    for (i, s) in spans.iter().enumerate() {
        let parent = s.parent_span_id.as_deref().filter(|p| ids.contains(p) && *p != s.span_id);
        children.entry(parent).or_default().push(i);
    }
    for v in children.values_mut() {
        v.sort_by(|a, b| {
            let (sa, sb) = (&spans[*a], &spans[*b]);
            (sa.start_time, &sa.span_id).cmp(&(sb.start_time, &sb.span_id))
        });
    }
    let mut rows = Vec::with_capacity(spans.len());
    let mut visited = vec![false; spans.len()];
    // Stack of (span, depth, ancestors' "has more siblings" flags, is_last)
    let roots = children.get(&None).cloned().unwrap_or_default();
    let mut stack: Vec<(usize, usize, Vec<bool>, bool)> = Vec::new();
    for (i, r) in roots.iter().enumerate().rev() {
        stack.push((*r, 0, vec![], i == roots.len() - 1));
    }
    while let Some((i, depth, guides, last)) = stack.pop() {
        if visited[i] {
            continue;
        }
        visited[i] = true;
        let mut prefix = String::new();
        if depth > 0 {
            for more in &guides[1..] {
                prefix.push_str(if *more { "│ " } else { "  " });
            }
            prefix.push_str(if last { "└─" } else { "├─" });
        }
        let s = &spans[i];
        rows.push(Row { span: i, depth, prefix, offset_ns: off(s), duration_ns: s.duration_ns.max(0) });
        if let Some(kids) = children.get(&Some(s.span_id.as_str())) {
            let mut g = guides.clone();
            g.push(!last);
            for (k, c) in kids.iter().enumerate().rev() {
                stack.push((*c, depth + 1, g.clone(), k == kids.len() - 1));
            }
        }
    }
    // Cycles (parent loops) leave spans unvisited: append them as roots.
    for (i, v) in visited.iter().enumerate() {
        if !v {
            let s = &spans[i];
            rows.push(Row {
                span: i,
                depth: 0,
                prefix: String::new(),
                offset_ns: off(s),
                duration_ns: s.duration_ns.max(0),
            });
        }
    }
    (rows, total)
}

/// Bar start column and length (>= 1) within `width` cells.
pub fn bar(offset_ns: i64, duration_ns: i64, total_ns: i64, width: u16) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }
    let w = width as f64;
    let total = total_ns.max(1) as f64;
    let start = ((offset_ns.max(0) as f64 / total) * w).floor().min(w - 1.0) as u16;
    let len = ((duration_ns.max(0) as f64 / total) * w).round().max(1.0) as u16;
    (start, len.min(width - start))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use odl_client::api::{Span, SpanKind, SpanStatus};

    pub fn span(id: &str, parent: Option<&str>, start_ms: i64, dur_ms: i64) -> Span {
        Span {
            trace_id: "t".into(),
            span_id: id.into(),
            parent_span_id: parent.map(str::to_string),
            name: format!("op-{id}"),
            kind: SpanKind::Server,
            service_name: "svc".into(),
            start_time: Utc.timestamp_millis_opt(1_700_000_000_000 + start_ms).unwrap(),
            duration_ns: dur_ms * 1_000_000,
            status_code: SpanStatus::Unset,
            status_message: String::new(),
            trace_state: String::new(),
            scope_name: String::new(),
            scope_version: String::new(),
            attributes: Default::default(),
            resource_attributes: Default::default(),
            events: vec![],
            links: vec![],
        }
    }

    #[test]
    fn orders_depth_first_with_glyphs() {
        let spans = vec![
            span("c2", Some("a"), 50, 10),
            span("a", None, 0, 100),
            span("c1", Some("a"), 10, 20),
            span("g", Some("c1"), 12, 5),
            span("orphan", Some("missing"), 60, 1),
        ];
        let (rows, total) = layout(&spans);
        let order: Vec<&str> = rows.iter().map(|r| spans[r.span].span_id.as_str()).collect();
        assert_eq!(order, vec!["a", "c1", "g", "c2", "orphan"]);
        assert_eq!(total, 100_000_000);
        let prefixes: Vec<&str> = rows.iter().map(|r| r.prefix.as_str()).collect();
        assert_eq!(prefixes, vec!["", "├─", "│ └─", "└─", ""]);
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn survives_cycles() {
        let spans = vec![span("a", Some("b"), 0, 1), span("b", Some("a"), 0, 1)];
        let (rows, _) = layout(&spans);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn bars() {
        assert_eq!(bar(0, 100, 100, 10), (0, 10));
        assert_eq!(bar(50, 1, 100, 10), (5, 1));
        assert_eq!(bar(99, 100, 100, 10), (9, 1));
        assert_eq!(bar(0, 0, 0, 0), (0, 0));
    }
}
