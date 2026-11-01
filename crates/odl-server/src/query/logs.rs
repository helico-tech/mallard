//! Log search and log volume series.

use std::collections::HashMap;

use duckdb::{Connection, params_from_iter, types::Value};
use odl_api::{LogRecord, LogSeries, LogSeriesPoint};

use super::{
    Filter, QResult, Window, bad, from_micros, json_col, parse_trace_id, span_hex, trace_hex,
};

pub struct LogSearch<'a> {
    pub service: Option<&'a str>,
    pub min_severity: Option<u8>,
    pub q: Option<&'a str>,
    pub trace_id: Option<&'a str>,
    pub attributes: Option<&'a str>,
    pub limit: u32,
    pub ascending: bool,
}

fn filter(w: &Window, service: Option<&str>, q: Option<&str>) -> Filter {
    let mut f = Filter::default();
    f.window("time", w);
    if let Some(s) = service {
        f.add("service_name = ?", [Value::Text(s.into())]);
    }
    if let Some(q) = q.filter(|q| !q.is_empty()) {
        // contains() on lower-cased text: no LIKE wildcards to escape.
        f.add("contains(lower(body), lower(?))", [Value::Text(q.into())]);
    }
    f
}

pub fn search(conn: &Connection, w: &Window, s: &LogSearch) -> QResult<Vec<LogRecord>> {
    let mut f = filter(w, s.service, s.q);
    if let Some(sev) = s.min_severity {
        if sev > 24 {
            return Err(bad("min_severity must be between 0 and 24"));
        }
        f.add("severity_number >= ?", [Value::UTinyInt(sev)]);
    }
    if let Some(t) = s.trace_id {
        f.add("trace_id = ?::UUID", [Value::Text(parse_trace_id(t)?)]);
    }
    f.attributes(s.attributes)?;
    let sql = format!(
        "SELECT epoch_us(time), epoch_us(observed_time), service_name, severity_number, severity_text, body, \
            CASE WHEN trace_id IS NULL THEN NULL ELSE {tid} END, CASE WHEN span_id IS NULL THEN NULL ELSE {sid} END, \
            event_name, scope_name, to_json(attributes), to_json(resource_attributes) \
         FROM logs WHERE {where_} ORDER BY time {dir} LIMIT {limit}",
        tid = trace_hex("trace_id"),
        sid = span_hex("span_id"),
        where_ = f.sql(),
        dir = if s.ascending { "ASC" } else { "DESC" },
        limit = s.limit,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(f.params), |r| {
        Ok(LogRecord {
            time: from_micros(r.get(0)?),
            observed_time: from_micros(r.get(1)?),
            service_name: r.get(2)?,
            severity_number: r.get(3)?,
            severity_text: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            body: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            trace_id: r.get(6)?,
            span_id: r.get(7)?,
            event_name: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
            scope_name: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
            attributes: json_col(r.get(10)?),
            resource_attributes: json_col(r.get(11)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn series(
    conn: &Connection,
    w: &Window,
    step: u64,
    service: Option<&str>,
    q: Option<&str>,
) -> QResult<LogSeries> {
    let f = filter(w, service, q);
    let step_us = step as i64 * 1_000_000;
    let sql = format!(
        "SELECT (epoch_us(time) // {step_us}) * {step} AS b, count(*), \
            count(*) FILTER (WHERE severity_number BETWEEN 1 AND 8 OR severity_number = 0), \
            count(*) FILTER (WHERE severity_number BETWEEN 9 AND 12), \
            count(*) FILTER (WHERE severity_number BETWEEN 13 AND 16), \
            count(*) FILTER (WHERE severity_number >= 17) \
         FROM logs WHERE {} GROUP BY 1",
        f.sql()
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut by: HashMap<i64, [i64; 5]> = HashMap::new();
    for row in stmt.query_map(params_from_iter(f.params), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
        ))
    })? {
        let (b, c) = row?;
        by.insert(b, c);
    }
    let points = w
        .buckets(step)
        .into_iter()
        .map(|b| {
            let c = by.get(&b).copied().unwrap_or_default();
            LogSeriesPoint {
                time: from_micros(b * 1_000_000),
                total: c[0] as u64,
                debug: c[1] as u64,
                info: c[2] as u64,
                warn: c[3] as u64,
                error: c[4] as u64,
            }
        })
        .collect();
    Ok(LogSeries {
        step_secs: step,
        points,
    })
}
