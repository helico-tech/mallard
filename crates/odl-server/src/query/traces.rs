//! Services, operations, span time series, trace search and trace lookup.

use std::collections::HashMap;

use duckdb::{Connection, params_from_iter, types::Value};
use odl_api::{
    OperationSummary, RedStats, ServiceSummary, Span, SpanEvent, SpanLink, SpanSeries, SpanSeriesPoint, Trace,
    TraceSummary,
};
use serde::Deserialize;

use super::{Filter, QResult, QueryError, Window, bad, from_micros, json_col, parse_trace_id, span_hex, trace_hex};

/// Entry spans: where requests enter a service.
const ENTRY: &str = "(kind IN ('SERVER', 'CONSUMER') OR parent_span_id IS NULL)";

/// Aggregates producing RED stats over `duration_ns` / `status_code`.
/// Columns: n, errors, p50, p95, p99, max (durations in ns).
const RED_AGGS: &str = "count(*) AS n, count(*) FILTER (WHERE status_code = 'ERROR') AS errors, \
    approx_quantile(duration_ns, 0.5)::DOUBLE AS p50, approx_quantile(duration_ns, 0.95)::DOUBLE AS p95, \
    approx_quantile(duration_ns, 0.99)::DOUBLE AS p99, max(duration_ns)::DOUBLE AS mx";

fn red(row: &duckdb::Row, first: usize, secs: f64) -> duckdb::Result<RedStats> {
    let n: i64 = row.get(first)?;
    let errors: i64 = row.get(first + 1)?;
    let ms = |i: usize| -> duckdb::Result<f64> { Ok(row.get::<_, Option<f64>>(first + i)?.unwrap_or(0.0) / 1e6) };
    Ok(RedStats {
        span_count: n as u64,
        error_count: errors as u64,
        error_rate: if n > 0 { errors as f64 / n as f64 } else { 0.0 },
        rate_per_sec: n as f64 / secs.max(1.0),
        p50_ms: ms(2)?,
        p95_ms: ms(3)?,
        p99_ms: ms(4)?,
        max_ms: ms(5)?,
    })
}

/// RED per service. Services without entry spans fall back to all their spans.
pub fn services(conn: &Connection, w: &Window, entry_only: bool) -> QResult<Vec<ServiceSummary>> {
    let mut f = Filter::default();
    f.window("start_time", w);
    let sql = format!(
        "WITH s AS (SELECT service_name, kind, parent_span_id, status_code, duration_ns FROM spans WHERE {where_}), \
         svc AS (SELECT service_name, bool_or({ENTRY}) AS has_entry FROM s GROUP BY 1) \
         SELECT s.service_name, {RED_AGGS} FROM s JOIN svc USING (service_name) \
         WHERE NOT ? OR NOT svc.has_entry OR {ENTRY} \
         GROUP BY 1 ORDER BY n DESC, 1",
        where_ = f.sql()
    );
    let mut params = f.params;
    params.push(Value::Boolean(entry_only));
    let mut stmt = conn.prepare(&sql)?;
    let secs = w.secs();
    let rows =
        stmt.query_map(params_from_iter(params), |r| Ok(ServiceSummary { name: r.get(0)?, red: red(r, 1, secs)? }))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn operations(
    conn: &Connection,
    w: &Window,
    service: Option<&str>,
    entry_only: bool,
) -> QResult<Vec<OperationSummary>> {
    let mut f = Filter::default();
    f.window("start_time", w);
    if let Some(s) = service {
        f.add("service_name = ?", [Value::Text(s.into())]);
    }
    if entry_only {
        f.add(ENTRY, []);
    }
    let sql = format!(
        "SELECT service_name, name, kind, {RED_AGGS} FROM spans WHERE {} GROUP BY 1, 2, 3 ORDER BY n DESC LIMIT 1000",
        f.sql()
    );
    let mut stmt = conn.prepare(&sql)?;
    let secs = w.secs();
    let rows = stmt.query_map(params_from_iter(f.params), |r| {
        Ok(OperationSummary { service: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, red: red(r, 3, secs)? })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn span_series(
    conn: &Connection,
    w: &Window,
    step: u64,
    service: Option<&str>,
    operation: Option<&str>,
    entry_only: bool,
) -> QResult<SpanSeries> {
    let mut f = Filter::default();
    f.window("start_time", w);
    if let Some(s) = service {
        f.add("service_name = ?", [Value::Text(s.into())]);
    }
    if let Some(o) = operation {
        f.add("name = ?", [Value::Text(o.into())]);
    }
    if entry_only {
        f.add(ENTRY, []);
    }
    let step_us = step as i64 * 1_000_000;
    let sql = format!(
        "SELECT (epoch_us(start_time) // {step_us}) * {step} AS b, {RED_AGGS} FROM spans WHERE {} GROUP BY 1",
        f.sql()
    );
    let mut stmt = conn.prepare(&sql)?;
    let secs = step as f64;
    let mut by_bucket: HashMap<i64, RedStats> = HashMap::new();
    for row in stmt.query_map(params_from_iter(f.params), |r| Ok((r.get::<_, i64>(0)?, red(r, 1, secs)?)))? {
        let (b, s) = row?;
        by_bucket.insert(b, s);
    }
    let points = w
        .buckets(step)
        .into_iter()
        .map(|b| {
            let s = by_bucket.get(&b);
            SpanSeriesPoint {
                time: from_micros(b * 1_000_000),
                count: s.map(|s| s.span_count).unwrap_or(0),
                errors: s.map(|s| s.error_count).unwrap_or(0),
                p50_ms: s.map(|s| s.p50_ms).unwrap_or(0.0),
                p95_ms: s.map(|s| s.p95_ms).unwrap_or(0.0),
                p99_ms: s.map(|s| s.p99_ms).unwrap_or(0.0),
            }
        })
        .collect();
    Ok(SpanSeries { step_secs: step, points })
}

pub struct TraceSearch<'a> {
    pub service: Option<&'a str>,
    pub operation: Option<&'a str>,
    pub errors_only: bool,
    pub min_duration_ms: Option<f64>,
    pub max_duration_ms: Option<f64>,
    pub attributes: Option<&'a str>,
    pub limit: u32,
}

/// Find traces with at least one span matching all filters, newest first,
/// and summarize each trace from its spans inside the window.
pub fn search(conn: &Connection, w: &Window, q: &TraceSearch) -> QResult<Vec<TraceSummary>> {
    let mut f = Filter::default();
    f.window("start_time", w);
    if let Some(s) = q.service {
        f.add("service_name = ?", [Value::Text(s.into())]);
    }
    if let Some(o) = q.operation {
        f.add("name = ?", [Value::Text(o.into())]);
    }
    if q.errors_only {
        f.add("status_code = 'ERROR'", []);
    }
    if let Some(ms) = q.min_duration_ms {
        f.add("duration_ns >= ?", [Value::BigInt((ms * 1e6) as i64)]);
    }
    if let Some(ms) = q.max_duration_ms {
        f.add("duration_ns <= ?", [Value::BigInt((ms * 1e6) as i64)]);
    }
    f.attributes(q.attributes)?;
    let sql = format!(
        "WITH m AS ( \
            SELECT trace_id, max(start_time) AS last FROM spans WHERE {where_} \
            GROUP BY trace_id ORDER BY last DESC LIMIT {limit} \
         ) \
         SELECT {tid}, epoch_us(min(s.start_time)), \
            (max(epoch_us(s.start_time) * 1000 + s.duration_ns) - min(epoch_us(s.start_time)) * 1000)::DOUBLE / 1e6, \
            any_value(s.service_name) FILTER (WHERE s.parent_span_id IS NULL), \
            any_value(s.name) FILTER (WHERE s.parent_span_id IS NULL), \
            count(*), count(*) FILTER (WHERE s.status_code = 'ERROR'), \
            to_json(list(DISTINCT s.service_name ORDER BY s.service_name)) \
         FROM spans s JOIN m USING (trace_id) \
         WHERE s.start_time >= ? AND s.start_time < ? \
         GROUP BY s.trace_id ORDER BY min(s.start_time) DESC",
        where_ = f.sql(),
        limit = q.limit,
        tid = trace_hex("s.trace_id"),
    );
    let mut params = f.params;
    params.push(w.start_param());
    params.push(w.end_param());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |r| {
        Ok(TraceSummary {
            trace_id: r.get(0)?,
            start_time: from_micros(r.get(1)?),
            duration_ms: r.get(2)?,
            root_service: r.get(3)?,
            root_name: r.get(4)?,
            span_count: r.get::<_, i64>(5)? as u64,
            error_count: r.get::<_, i64>(6)? as u64,
            services: json_col(r.get(7)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[derive(Deserialize, Default)]
struct EventJson {
    time: i64,
    name: String,
    #[serde(default)]
    attributes: odl_api::Attributes,
}

#[derive(Deserialize, Default)]
struct LinkJson {
    trace_id: Option<String>,
    span_id: Option<String>,
    trace_state: Option<String>,
    #[serde(default)]
    attributes: odl_api::Attributes,
}

/// Columns selected for a full span, in the order `span_from_row` reads them.
pub fn span_select(alias: &str) -> String {
    let a = |c: &str| format!("{alias}{c}");
    format!(
        "{tid}, {sid}, CASE WHEN {p} IS NULL THEN NULL ELSE {pid} END, {name}, {kind}, {svc}, epoch_us({st}), {dur}, \
         {sc}, {sm}, {ts}, {scn}, {scv}, to_json({attrs}), to_json({rattrs}), \
         to_json(list_transform({ev}, lambda e: {{'time': epoch_us(e.time), 'name': e.name, 'attributes': e.attributes}})), \
         to_json(list_transform({ln}, lambda l: {{'trace_id': {ltid}, 'span_id': {lsid}, 'trace_state': l.trace_state, 'attributes': l.attributes}}))",
        tid = trace_hex(&a("trace_id")),
        sid = span_hex(&a("span_id")),
        p = a("parent_span_id"),
        pid = span_hex(&a("parent_span_id")),
        name = a("name"),
        kind = a("kind"),
        svc = a("service_name"),
        st = a("start_time"),
        dur = a("duration_ns"),
        sc = a("status_code"),
        sm = a("status_message"),
        ts = a("trace_state"),
        scn = a("scope_name"),
        scv = a("scope_version"),
        attrs = a("attributes"),
        rattrs = a("resource_attributes"),
        ev = a("events"),
        ln = a("links"),
        ltid = trace_hex("l.trace_id"),
        lsid = span_hex("l.span_id"),
    )
}

pub fn span_from_row(r: &duckdb::Row) -> duckdb::Result<Span> {
    let events: Vec<EventJson> = json_col(r.get(15)?);
    let links: Vec<LinkJson> = json_col(r.get(16)?);
    Ok(Span {
        trace_id: r.get(0)?,
        span_id: r.get(1)?,
        parent_span_id: r.get(2)?,
        name: r.get(3)?,
        kind: r.get(4)?,
        service_name: r.get(5)?,
        start_time: from_micros(r.get(6)?),
        duration_ns: r.get(7)?,
        status_code: r.get(8)?,
        status_message: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
        trace_state: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
        scope_name: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        scope_version: r.get::<_, Option<String>>(12)?.unwrap_or_default(),
        attributes: json_col(r.get(13)?),
        resource_attributes: json_col(r.get(14)?),
        events: events
            .into_iter()
            .map(|e| SpanEvent { time: from_micros(e.time), name: e.name, attributes: e.attributes })
            .collect(),
        links: links
            .into_iter()
            .map(|l| SpanLink {
                trace_id: l.trace_id.unwrap_or_default(),
                span_id: l.span_id.unwrap_or_default(),
                trace_state: l.trace_state.unwrap_or_default(),
                attributes: l.attributes,
            })
            .collect(),
    })
}

/// All spans of one trace inside the window.
pub fn get(conn: &Connection, w: &Window, trace_id: &str) -> QResult<Trace> {
    let tid = parse_trace_id(trace_id)?;
    let sql = format!(
        "SELECT {} FROM spans WHERE trace_id = ?::UUID AND start_time >= ? AND start_time < ? \
         ORDER BY start_time, span_id LIMIT 100000",
        span_select("")
    );
    let mut stmt = conn.prepare(&sql)?;
    let spans: Vec<Span> = stmt
        .query_map(params_from_iter([Value::Text(tid.clone()), w.start_param(), w.end_param()]), span_from_row)?
        .collect::<Result<_, _>>()?;
    if spans.is_empty() {
        return Err(QueryError::NotFound(format!("trace {tid} not found in the time window")));
    }
    Ok(Trace { trace_id: tid, spans })
}

pub fn clamp_limit(limit: Option<u32>, default: u32, max: u32) -> QResult<u32> {
    match limit {
        None => Ok(default),
        Some(0) => Err(bad("limit must be at least 1")),
        Some(n) => Ok(n.min(max)),
    }
}
