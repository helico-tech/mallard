//! Metric exemplars for `metrics/query?exemplars=true`: per series (the
//! `group_by` labels) and time bucket, the exemplar with the highest value
//! that links to a trace. Exemplars without a trace id, or with a value that
//! is not finite, are skipped.

use mallard_duck::duckdb::{Connection, params_from_iter, types::Value};
use odl_api::MetricExemplar;

use super::{Filter, QResult, Window, from_micros, json_col, span_hex, trace_hex, ts};
use crate::store::schema::TableId;

/// At most this many exemplars per query (oldest first).
pub const MAX_EXEMPLARS: usize = 1000;

/// Tables with an `exemplars` column (OTLP summaries have no exemplars).
const TABLES: [TableId; 3] = [TableId::MetricPoints, TableId::MetricHistograms, TableId::MetricExpHistograms];

pub struct ExemplarRequest<'a> {
    pub name: &'a str,
    pub service: Option<&'a str>,
    pub group_by: Option<&'a str>,
    pub attributes: Option<&'a str>,
    /// Bucket width in seconds, as in the metric query.
    pub step: u64,
}

pub fn query(conn: &Connection, w: &Window, req: &ExemplarRequest) -> QResult<Vec<MetricExemplar>> {
    let group_by: Vec<String> = req
        .group_by
        .map(|g| g.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let mut f = Filter::default();
    f.window("time", w);
    if let Some(s) = req.service {
        f.add("service_name = ?", [Value::Text(s.into())]);
    }
    f.add("metric_name = ?", [Value::Text(req.name.into())]);
    f.attributes(req.attributes)?;
    let (labels, label_params) = super::metrics::labels_expr(&group_by);
    let parts: Vec<String> = TABLES
        .iter()
        .map(|t| format!("SELECT {labels} AS k, unnest(exemplars) AS e FROM {} WHERE {}", t.def().name, f.sql()))
        .collect();
    let step_us = req.step.max(1) * 1_000_000;
    let sql = format!(
        "SELECT k, epoch_us(e.time), e.value, {tid}, {sid}, to_json(e.attributes) FROM ( \
            SELECT k, e, row_number() OVER (PARTITION BY k, epoch_us(e.time) // {step_us} \
                                            ORDER BY e.value DESC, e.time) AS rn \
            FROM ({parts}) \
            WHERE e.trace_id IS NOT NULL AND isfinite(e.value) AND e.time >= ? AND e.time < ?) \
         WHERE rn = 1 ORDER BY e.time, k LIMIT {MAX_EXEMPLARS}",
        tid = trace_hex("e.trace_id"),
        sid = span_hex("e.span_id"),
        parts = parts.join(" UNION ALL "),
    );
    let mut params: Vec<Value> = Vec::new();
    for _ in TABLES {
        params.extend(label_params.iter().cloned());
        params.extend(f.params.iter().cloned());
    }
    params.extend([ts(w.from), ts(w.to)]);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |r| {
        let vals: Vec<String> = json_col(r.get(0)?);
        Ok(MetricExemplar {
            labels: group_by.iter().cloned().zip(vals).collect(),
            time: from_micros(r.get(1)?),
            value: r.get(2)?,
            trace_id: r.get(3)?,
            span_id: r.get(4)?,
            attributes: json_col(r.get(5)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}
