//! `ops --compare <dur>`: each operation's p95 and error rate now and in
//! the same window `<dur>` earlier, joined in the CLI (no server API).
//! Operations that only ran in the earlier window are not listed.

use std::collections::HashMap;

use comfy_table::{Cell, CellAlignment, Color};
use odl_client::api::{OperationSummary, SpanKind};
use serde::Serialize;

use crate::fmt::{clean, compact, err_cell, ms, table};

/// One operation now, with the earlier window's numbers and the change.
/// `*_before` and the changes are null when the operation had no spans
/// then.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OpCompare {
    #[serde(flatten)]
    pub now: OperationSummary,
    pub span_count_before: Option<u64>,
    pub p95_ms_before: Option<f64>,
    /// (p95 now − p95 before) / p95 before, in percent (null when p95 before is 0).
    pub p95_change_pct: Option<f64>,
    pub error_rate_before: Option<f64>,
    /// error rate now − error rate before (both 0..1).
    pub error_rate_change: Option<f64>,
}

/// Join on service, name and kind.
pub(crate) fn join(now: Vec<OperationSummary>, before: Vec<OperationSummary>) -> Vec<OpCompare> {
    let key = |o: &OperationSummary| (o.service.clone(), o.name.clone(), o.kind);
    let before: HashMap<(String, String, SpanKind), OperationSummary> =
        before.into_iter().map(|o| (key(&o), o)).collect();
    now.into_iter()
        .map(|o| {
            let b = before.get(&key(&o)).map(|b| &b.red);
            let p95_change_pct =
                b.filter(|b| b.p95_ms > 0.0).map(|b| round((o.red.p95_ms - b.p95_ms) / b.p95_ms * 100.0, 1));
            OpCompare {
                span_count_before: b.map(|b| b.span_count),
                p95_ms_before: b.map(|b| b.p95_ms),
                p95_change_pct,
                error_rate_before: b.map(|b| b.error_rate),
                error_rate_change: b.map(|b| round(o.red.error_rate - b.error_rate, 4)),
                now: o,
            }
        })
        .collect()
}

fn round(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (v * f).round() / f
}

fn right(s: String) -> Cell {
    Cell::new(s).set_alignment(CellAlignment::Right)
}

/// A change in percent: red when it got worse.
fn change_cell(pct: Option<f64>) -> Cell {
    match pct {
        None => right("-".into()),
        Some(p) => {
            let c = right(format!("{p:+.1}%"));
            if p >= 10.0 {
                c.fg(Color::Red)
            } else if p <= -10.0 {
                c.fg(Color::Green)
            } else {
                c
            }
        }
    }
}

pub(crate) fn compare_text(v: &[OpCompare], shift: &str) -> String {
    if v.is_empty() {
        return "no spans in this window".into();
    }
    let before = format!("{shift} ago");
    let mut t = table(&[
        "service",
        "operation",
        "spans",
        "errors",
        "p95",
        &format!("p95 {before}"),
        "p95 change",
        &format!("errors {before}"),
    ]);
    for o in v {
        t.add_row(vec![
            Cell::new(clean(&o.now.service)),
            Cell::new(clean(&o.now.name)),
            right(compact(o.now.red.span_count as f64)),
            err_cell(o.now.red.error_rate),
            right(ms(o.now.red.p95_ms)),
            right(o.p95_ms_before.map_or("-".into(), ms)),
            change_cell(o.p95_change_pct),
            o.error_rate_before.map_or(right("-".into()), err_cell),
        ]);
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use odl_client::api::RedStats;

    use super::*;

    fn op(name: &str, spans: u64, error_rate: f64, p95_ms: f64) -> OperationSummary {
        OperationSummary {
            service: "checkout".into(),
            name: name.into(),
            kind: SpanKind::Server,
            red: RedStats {
                span_count: spans,
                error_count: (spans as f64 * error_rate) as u64,
                error_rate,
                rate_per_sec: 1.0,
                p50_ms: p95_ms / 2.0,
                p95_ms,
                p99_ms: p95_ms * 2.0,
                max_ms: p95_ms * 3.0,
            },
        }
    }

    #[test]
    fn joins_on_the_operation_and_computes_the_change() {
        let now = vec![op("POST /pay", 100, 0.10, 150.0), op("GET /new", 5, 0.0, 10.0)];
        let before = vec![op("POST /pay", 80, 0.05, 100.0), op("GET /gone", 9, 0.0, 1.0)];
        let v = join(now, before);
        assert_eq!(v.len(), 2, "only operations of the current window");
        let pay = &v[0];
        assert_eq!((pay.span_count_before, pay.p95_ms_before, pay.p95_change_pct), (Some(80), Some(100.0), Some(50.0)));
        assert_eq!((pay.error_rate_before, pay.error_rate_change), (Some(0.05), Some(0.05)));
        let new = &v[1];
        assert_eq!((new.p95_ms_before, new.p95_change_pct, new.error_rate_change), (None, None, None));
    }

    #[test]
    fn json_is_the_operation_plus_the_before_columns() {
        let v = serde_json::to_value(&join(vec![op("a", 1, 0.0, 10.0)], vec![op("a", 1, 0.0, 0.0)])[0]).unwrap();
        assert_eq!(v["name"], "a");
        assert_eq!(v["p95_ms"], 10.0);
        assert_eq!(v["p95_ms_before"], 0.0);
        assert!(v["p95_change_pct"].is_null(), "no percentage of zero: {v}");
        assert_eq!(v["error_rate_change"], 0.0);
    }

    #[test]
    fn text_has_the_before_columns() {
        let v = join(vec![op("POST /pay", 100, 0.1, 150.0)], vec![op("POST /pay", 80, 0.05, 100.0)]);
        let t = compare_text(&v, "1d");
        assert!(t.contains("p95 1d ago") && t.contains("+50.0%") && t.contains("POST /pay"), "{t}");
    }
}
