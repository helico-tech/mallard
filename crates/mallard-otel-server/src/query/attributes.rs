//! Attribute keys and values (`/api/v1/attributes`) and the table schema
//! (`/api/v1/schema`): what an agent can filter on and query.

use mallard_duck::duckdb::{Connection, params, params_from_iter, types::Value};
use mallard_otel_api::{AttributeCount, AttributeSignal, AttributeStats, Schema, SchemaTable, SqlColumn};

use super::{Filter, QResult, Window, bad};
use crate::store::{
    CATALOG,
    schema::{
        LOGS, METRIC_EXP_HISTOGRAMS, METRIC_HISTOGRAMS, METRIC_POINTS, METRIC_SUMMARIES, SPANS, TABLES, TableDef,
    },
};

/// At most this many matching rows are counted (a random sample).
pub const SAMPLE_ROWS: u64 = 10_000;

pub struct AttributeSearch<'a> {
    pub signal: AttributeSignal,
    pub service: Option<&'a str>,
    pub metric: Option<&'a str>,
    pub key: Option<&'a str>,
    pub limit: u32,
}

/// `SELECT attributes, resource_attributes` of the matching rows of one table.
fn rows_of(t: &TableDef, w: &Window, q: &AttributeSearch) -> (String, Vec<Value>) {
    let mut f = Filter::default();
    f.window(t.time_column, w);
    if let Some(s) = q.service {
        f.add("service_name = ?", [Value::Text(s.into())]);
    }
    if let Some(m) = q.metric {
        f.add("metric_name = ?", [Value::Text(m.into())]);
    }
    if let Some(k) = q.key {
        f.add(
            "(attributes[?] IS NOT NULL OR resource_attributes[?] IS NOT NULL)",
            [Value::Text(k.into()), Value::Text(k.into())],
        );
    }
    (format!("SELECT attributes, resource_attributes FROM {} WHERE {}", t.name, f.sql()), f.params)
}

/// Keys with the number of sampled rows that have them, or, with `key`, the
/// values of that key. Span/log/point attributes and resource attributes
/// count together (filters match either).
pub fn stats(conn: &Connection, w: &Window, q: &AttributeSearch) -> QResult<AttributeStats> {
    let tables: &[&TableDef] = match q.signal {
        AttributeSignal::Spans => &[&SPANS],
        AttributeSignal::Logs => &[&LOGS],
        AttributeSignal::Metrics => &[&METRIC_POINTS, &METRIC_HISTOGRAMS, &METRIC_EXP_HISTOGRAMS, &METRIC_SUMMARIES],
    };
    if q.metric.is_some() && q.signal != AttributeSignal::Metrics {
        return Err(bad("metric applies to signal=metrics only"));
    }
    if q.key.is_some_and(|k| k.trim().is_empty()) {
        return Err(bad("key must not be empty"));
    }
    let (mut parts, mut params) = (Vec::new(), Vec::new());
    for t in tables {
        let (sql, p) = rows_of(t, w, q);
        parts.push(sql);
        params.extend(p);
    }
    let src = parts.join(" UNION ALL ");
    let total: i64 =
        conn.query_row(&format!("SELECT count(*) FROM ({src})"), params_from_iter(params.clone()), |r| r.get(0))?;
    let sample = format!("WITH s AS (SELECT * FROM ({src}) USING SAMPLE reservoir({SAMPLE_ROWS} ROWS))");
    let items = match q.key {
        None => {
            let sql = format!(
                "{sample} SELECT k, count(*) FROM (SELECT unnest(list_distinct(list_concat( \
                    coalesce(map_keys(attributes), []), coalesce(map_keys(resource_attributes), [])))) AS k FROM s) \
                 GROUP BY k ORDER BY 2 DESC, 1 LIMIT {}",
                q.limit
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |r| {
                Ok(AttributeCount { key: r.get(0)?, value: None, count: r.get::<_, i64>(1)? as u64 })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        }
        Some(key) => {
            let sql = format!(
                "{sample} SELECT v, count(*) FROM (SELECT coalesce(attributes[?], resource_attributes[?]) AS v FROM s) \
                 WHERE v IS NOT NULL GROUP BY v ORDER BY 2 DESC, 1 LIMIT {}",
                q.limit
            );
            params.extend([Value::Text(key.into()), Value::Text(key.into())]);
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |r| {
                Ok(AttributeCount { key: key.to_string(), value: r.get(0)?, count: r.get::<_, i64>(1)? as u64 })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        }
    };
    let total = total.max(0) as u64;
    Ok(AttributeStats { total_rows: total, sampled_rows: total.min(SAMPLE_ROWS), items })
}

/// The lake tables the SQL endpoint allows, with their columns as they are
/// in the catalog (a newer version may have added some).
pub fn schema(conn: &Connection, schema: &str) -> QResult<Schema> {
    let mut stmt = conn.prepare(
        "SELECT table_name, column_name, data_type FROM duckdb_columns() \
         WHERE database_name = ? AND schema_name = ? ORDER BY table_name, column_index",
    )?;
    let cols: Vec<(String, String, String)> = stmt
        .query_map(params![CATALOG, schema], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let tables = TABLES
        .iter()
        .map(|t| SchemaTable {
            name: t.name.to_string(),
            time_column: t.time_column.to_string(),
            columns: cols
                .iter()
                .filter(|c| c.0 == t.name)
                .map(|c| SqlColumn { name: c.1.clone(), data_type: c.2.clone() })
                .collect(),
        })
        .filter(|t| !t.columns.is_empty())
        .collect();
    Ok(Schema { tables })
}
