//! Metric listing and time-series queries.
//!
//! Semantics (see docs/api.md):
//! - Gauges / non-monotonic sums: `avg`, `min`, `max` over all points in a
//!   bucket; `sum` adds the per-series average of each series; `count` counts points.
//! - Monotonic cumulative sums: `increase` = per-series difference between
//!   the last value in a bucket and the last value in the previous bucket
//!   (a drop is treated as a counter reset), summed over series; `rate` =
//!   increase / step. Delta sums: increase = sum of values.
//! - Temporality is read per point, not per metric: a metric with both
//!   delta and cumulative series handles each series by its own temporality
//!   (the OTel stream identity includes temporality).
//! - Histograms (explicit and exponential) and summaries: `pNN` percentiles
//!   computed from the merged bucket increase within each time bucket (linear
//!   interpolation inside a bucket, like Prometheus `histogram_quantile`);
//!   `avg` = sum/count, `count` = count increase, `rate` = count / step.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet, hash_map::Entry},
};

use mallard_duck::duckdb::{Connection, params_from_iter, types::Value};
use odl_api::{MetricInfo, MetricPoint, MetricQueryResult, MetricSeries, MetricType};

use super::{Filter, QResult, QueryError, Window, bad, enum_col, from_micros, json_col, json_str};
use crate::store::schema::TableId;

const MAX_SERIES: usize = 500;
const MAX_POINTS: usize = 200_000;

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// The four metric tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricTable {
    Points,
    Histograms,
    ExpHistograms,
    Summaries,
}

impl MetricTable {
    const ALL: [MetricTable; 4] =
        [MetricTable::Points, MetricTable::Histograms, MetricTable::ExpHistograms, MetricTable::Summaries];

    fn name(self) -> &'static str {
        match self {
            MetricTable::Points => TableId::MetricPoints,
            MetricTable::Histograms => TableId::MetricHistograms,
            MetricTable::ExpHistograms => TableId::MetricExpHistograms,
            MetricTable::Summaries => TableId::MetricSummaries,
        }
        .def()
        .name
    }

    /// SQL expression for the metric type of a row.
    fn type_expr(self) -> String {
        let fixed = |t: MetricType| format!("'{t}'");
        match self {
            MetricTable::Points => "metric_type".into(),
            MetricTable::Histograms => fixed(MetricType::Histogram),
            MetricTable::ExpHistograms => fixed(MetricType::ExponentialHistogram),
            MetricTable::Summaries => fixed(MetricType::Summary),
        }
    }
}

fn base_filter(w: &Window, service: Option<&str>) -> Filter {
    let mut f = Filter::default();
    f.window("time", w);
    if let Some(s) = service {
        f.add("service_name = ?", [Value::Text(s.into())]);
    }
    f
}

pub fn list(conn: &Connection, w: &Window, service: Option<&str>, q: Option<&str>) -> QResult<Vec<MetricInfo>> {
    let mut f = base_filter(w, service);
    if let Some(q) = q.filter(|q| !q.is_empty()) {
        f.add("contains(lower(metric_name), lower(?))", [Value::Text(q.into())]);
    }
    let parts: Vec<String> = MetricTable::ALL
        .iter()
        .map(|t| {
            format!(
                "SELECT metric_name, {} AS t, metric_unit, metric_description, temporality, series_id, \
                 service_name, time FROM {} WHERE {}",
                t.type_expr(),
                t.name(),
                f.sql()
            )
        })
        .collect();
    let sql = format!(
        "SELECT metric_name, any_value(t), any_value(metric_unit), any_value(metric_description), \
            CASE WHEN count(DISTINCT nullif(temporality, '')) > 1 THEN 'mixed' \
                ELSE coalesce(any_value(nullif(temporality, '')), '') END, \
            count(DISTINCT series_id), count(*), \
            to_json(list(DISTINCT service_name ORDER BY service_name)), epoch_us(max(time)) \
         FROM ({}) GROUP BY 1 ORDER BY 1 LIMIT 5000",
        parts.join(" UNION ALL ")
    );
    let params: Vec<Value> = MetricTable::ALL.iter().flat_map(|_| f.params.clone()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |r| {
        Ok(MetricInfo {
            name: r.get(0)?,
            metric_type: enum_col(r, 1)?,
            unit: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            description: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            temporality: enum_col(r, 4)?,
            series_count: r.get::<_, i64>(5)? as u64,
            point_count: r.get::<_, i64>(6)? as u64,
            services: json_col(r.get(7)?),
            last_seen: from_micros(r.get(8)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

pub struct MetricRequest<'a> {
    pub name: &'a str,
    pub service: Option<&'a str>,
    pub agg: Option<&'a str>,
    pub group_by: Option<&'a str>,
    pub attributes: Option<&'a str>,
    pub step: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Agg {
    Avg,
    Min,
    Max,
    Sum,
    Count,
    Rate,
    Increase,
    Quantile(f64),
}

impl Agg {
    fn parse(s: &str) -> QResult<Self> {
        Ok(match s {
            "avg" => Agg::Avg,
            "min" => Agg::Min,
            "max" => Agg::Max,
            "sum" => Agg::Sum,
            "count" => Agg::Count,
            "rate" => Agg::Rate,
            "increase" => Agg::Increase,
            p if p.starts_with('p') => {
                let n: f64 = p[1..].parse().map_err(|_| bad(format!("unknown agg {s:?}")))?;
                if !(0.0..=100.0).contains(&n) {
                    return Err(bad("percentile must be between p0 and p100"));
                }
                Agg::Quantile(n / 100.0)
            }
            _ => return Err(bad(format!("unknown agg {s:?}"))),
        })
    }
    fn name(self) -> String {
        match self {
            Agg::Avg => "avg".into(),
            Agg::Min => "min".into(),
            Agg::Max => "max".into(),
            Agg::Sum => "sum".into(),
            Agg::Count => "count".into(),
            Agg::Rate => "rate".into(),
            Agg::Increase => "increase".into(),
            Agg::Quantile(q) => format!("p{}", (q * 100.0 * 1000.0).round() / 1000.0),
        }
    }
}

struct Kind {
    table: MetricTable,
    metric_type: MetricType,
    monotonic: bool,
    unit: String,
}

fn detect(conn: &Connection, w: &Window, req: &MetricRequest) -> QResult<Kind> {
    let mut f = base_filter(w, req.service);
    f.add("metric_name = ?", [Value::Text(req.name.into())]);
    for table in MetricTable::ALL {
        let mono = if table == MetricTable::Points { "bool_or(is_monotonic)" } else { "false" };
        let sql = format!(
            "SELECT any_value({}), {mono}, any_value(metric_unit), count(*) \
             FROM {} WHERE {}",
            table.type_expr(),
            table.name(),
            f.sql()
        );
        let kind = conn.query_row(&sql, params_from_iter(f.params.clone()), |r| {
            if r.get::<_, i64>(3)? == 0 {
                return Ok(None);
            }
            Ok(Some(Kind {
                table,
                metric_type: enum_col(r, 0)?,
                monotonic: r.get::<_, Option<bool>>(1)?.unwrap_or(false),
                unit: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            }))
        })?;
        if let Some(kind) = kind {
            return Ok(kind);
        }
    }
    Err(QueryError::NotFound(format!("metric {:?} has no data in the time window", req.name)))
}

/// SELECT expression producing a JSON array of group label values, plus its params.
pub(super) fn labels_expr(group_by: &[String]) -> (String, Vec<Value>) {
    if group_by.is_empty() {
        return ("'[]'".into(), vec![]);
    }
    let mut params = vec![];
    let parts: Vec<String> = group_by
        .iter()
        .map(|k| {
            if k == "service.name" {
                "service_name".to_string()
            } else {
                params.push(Value::Text(k.clone()));
                params.push(Value::Text(k.clone()));
                "coalesce(attributes[?], resource_attributes[?], '')".to_string()
            }
        })
        .collect();
    (format!("to_json([{}])", parts.join(", ")), params)
}

type Key = String; // JSON label array

/// Enforces [`MAX_SERIES`] and [`MAX_POINTS`] while rows stream in, so a
/// too-large result fails early instead of being materialized first.
#[derive(Default)]
struct Caps {
    series: HashSet<Key>,
    points: usize,
}

impl Caps {
    /// Count one more point of series `key`.
    fn add(&mut self, key: &Key) -> QResult<()> {
        if !self.series.contains(key) {
            if self.series.len() >= MAX_SERIES {
                return Err(bad(format!(
                    "query produces more than {MAX_SERIES} series; narrow group_by or add filters"
                )));
            }
            self.series.insert(key.clone());
        }
        self.points += 1;
        if self.points > MAX_POINTS {
            return Err(bad(format!(
                "query produces more than {MAX_POINTS} points; use a larger step or a shorter window"
            )));
        }
        Ok(())
    }
}

struct Collector {
    group_by: Vec<String>,
    points: BTreeMap<Key, BTreeMap<i64, f64>>,
    caps: Caps,
}

impl Collector {
    fn new(group_by: Vec<String>) -> Self {
        Self { group_by, points: BTreeMap::new(), caps: Caps::default() }
    }
    fn put(&mut self, key: Key, bucket: i64, v: f64) -> QResult<()> {
        self.caps.add(&key)?;
        if v.is_finite() {
            self.points.entry(key).or_default().insert(bucket, v);
        }
        Ok(())
    }
    fn finish(self) -> QResult<Vec<MetricSeries>> {
        if self.points.len() > MAX_SERIES {
            return Err(bad(format!(
                "query produced {} series (max {MAX_SERIES}); narrow group_by or add filters",
                self.points.len()
            )));
        }
        Ok(self
            .points
            .into_iter()
            .map(|(k, pts)| {
                let vals: Vec<String> = json_col(Some(k));
                MetricSeries {
                    labels: self.group_by.iter().cloned().zip(vals).collect(),
                    points: pts
                        .into_iter()
                        .map(|(b, v)| MetricPoint { time: from_micros(b * 1_000_000), value: v })
                        .collect(),
                }
            })
            .collect())
    }
}

pub fn query(conn: &Connection, w: &Window, req: &MetricRequest) -> QResult<MetricQueryResult> {
    let kind = detect(conn, w, req)?;
    let is_counter = kind.metric_type == MetricType::Sum && kind.monotonic;
    let agg = match req.agg {
        Some(a) => Agg::parse(a)?,
        None => match kind.table {
            MetricTable::Points if is_counter => Agg::Rate,
            MetricTable::Points => Agg::Avg,
            _ => Agg::Quantile(0.95),
        },
    };
    let group_by: Vec<String> = req
        .group_by
        .map(|g| g.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    if group_by.len() > 5 {
        return Err(bad("group_by supports at most 5 keys"));
    }
    let step = req.step as i64;
    // One extra bucket before the window gives cumulative series a baseline.
    let ext = Window { from: w.from - chrono::TimeDelta::seconds(step), to: w.to };
    let mut filter = base_filter(&ext, req.service);
    filter.add("metric_name = ?", [Value::Text(req.name.into())]);
    filter.attributes(req.attributes)?;
    let (labels, label_params) = labels_expr(&group_by);
    let plan = Plan {
        w,
        filter,
        labels,
        label_params,
        bucket: format!("(epoch_us(time) // {}) * {step}", step * 1_000_000),
        first_bucket: w.from.timestamp().div_euclid(step) * step,
        step,
    };
    let mut col = Collector::new(group_by);
    match kind.table {
        MetricTable::Points => query_points(conn, &plan, is_counter, agg, &mut col)?,
        MetricTable::Histograms => query_histograms(conn, &plan, HistKind::Explicit, agg, &mut col)?,
        MetricTable::ExpHistograms => query_histograms(conn, &plan, HistKind::Exponential, agg, &mut col)?,
        MetricTable::Summaries => query_histograms(conn, &plan, HistKind::Summary, agg, &mut col)?,
    }
    Ok(MetricQueryResult {
        name: req.name.to_string(),
        metric_type: kind.metric_type,
        unit: kind.unit,
        agg: agg.name(),
        step_secs: req.step,
        series: col.finish()?,
        exemplars: Vec::new(),
    })
}

/// SQL pieces shared by the point and histogram queries.
struct Plan<'a> {
    w: &'a Window,
    /// Metric, service and attribute filters over the window plus one bucket.
    filter: Filter,
    /// Label expression ([`labels_expr`]) and its parameters.
    labels: String,
    label_params: Vec<Value>,
    /// Bucket start (epoch seconds) of a row.
    bucket: String,
    first_bucket: i64,
    step: i64,
}

impl Plan<'_> {
    /// Parameters: labels, filter, and the window start when `from_window`.
    fn params(&self, from_window: bool) -> Vec<Value> {
        let mut p = self.label_params.clone();
        p.extend(self.filter.params.iter().cloned());
        if from_window {
            p.push(super::ts(self.w.from));
        }
        p
    }
}

/// SQL condition: the row is a cumulative point. Temporality is per point,
/// so one metric can mix delta and cumulative series.
const IS_CUMULATIVE: &str = "temporality = 'cumulative'";

/// Gauges and sums.
fn query_points(conn: &Connection, plan: &Plan, is_counter: bool, agg: Agg, col: &mut Collector) -> QResult<()> {
    let Plan { labels, bucket, first_bucket, .. } = plan;
    let where_ = plan.filter.sql();
    let (sql, from_window) = match agg {
        Agg::Quantile(_) => return Err(bad("percentiles need a histogram or summary metric")),
        Agg::Rate | Agg::Increase if !is_counter => {
            return Err(bad(
                "rate/increase need a monotonic sum (counter); use avg/min/max for gauges and up-down counters",
            ));
        }
        // Counter, per series and bucket. Cumulative: last value, then
        // reset-aware differences (partitioned by temporality too, so delta
        // points never become a cumulative baseline). Delta: sum of values.
        Agg::Rate | Agg::Increase => (
            format!(
                "WITH per AS (SELECT {labels} AS k, series_id, {IS_CUMULATIVE} AS cum, {bucket} AS b, \
                    arg_max(value, time) AS v, sum(value) AS total FROM metric_points WHERE {where_} GROUP BY ALL), \
                 d AS (SELECT k, b, cum, v, total, \
                    v - lag(v) OVER (PARTITION BY series_id, cum ORDER BY b) AS dv FROM per) \
                 SELECT k, b, sum(CASE WHEN cum IS NOT TRUE THEN total WHEN dv < 0 THEN v ELSE dv END) FROM d \
                 WHERE (dv IS NOT NULL OR cum IS NOT TRUE) AND b >= {first_bucket} GROUP BY ALL"
            ),
            false,
        ),
        // Per series: the average for cumulative sums and gauges, the total
        // for delta sums; then summed over series.
        Agg::Sum => (
            format!(
                "SELECT k, b, sum(CASE WHEN cum OR metric_type = 'gauge' THEN a ELSE s END) FROM \
                 (SELECT {labels} AS k, series_id, {IS_CUMULATIVE} AS cum, metric_type, {bucket} AS b, \
                    avg(value) AS a, sum(value) AS s FROM metric_points WHERE {where_} AND time >= ? GROUP BY ALL) \
                 GROUP BY ALL"
            ),
            true,
        ),
        Agg::Count | Agg::Avg | Agg::Min | Agg::Max => {
            let a = match agg {
                Agg::Count => "count(*)::DOUBLE",
                Agg::Avg => "avg(value)",
                Agg::Min => "min(value)",
                _ => "max(value)",
            };
            (
                format!(
                    "SELECT {labels} AS k, {bucket} AS b, {a} FROM metric_points WHERE {where_} \
                     AND time >= ? GROUP BY ALL"
                ),
                true,
            )
        }
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(plan.params(from_window)), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<f64>>(2)?))
    })?;
    for row in rows {
        let (k, b, v) = row?;
        let v = v.unwrap_or(f64::NAN);
        let v = if agg == Agg::Rate { v / plan.step as f64 } else { v };
        col.put(k, b, v)?;
    }
    Ok(())
}

/// Data point kinds stored as buckets or quantiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistKind {
    Explicit,
    Exponential,
    Summary,
}

impl HistKind {
    fn table(self) -> MetricTable {
        match self {
            HistKind::Explicit => MetricTable::Histograms,
            HistKind::Exponential => MetricTable::ExpHistograms,
            HistKind::Summary => MetricTable::Summaries,
        }
    }

    /// Columns: count, sum, a, b, scale, zero_count, offset (see `Row`).
    fn columns(self) -> [&'static str; 7] {
        match self {
            HistKind::Explicit => [
                "count::DOUBLE",
                "sum",
                "to_json(bucket_counts)",
                "to_json(explicit_bounds)",
                "NULL::INTEGER",
                "0::UBIGINT",
                "0::INTEGER",
            ],
            HistKind::Exponential => [
                "count::DOUBLE",
                "sum",
                "to_json(positive_bucket_counts)",
                "to_json(negative_bucket_counts)",
                "scale",
                "zero_count",
                "positive_offset",
            ],
            HistKind::Summary => [
                "count::DOUBLE",
                "sum",
                "to_json(quantiles)",
                "NULL::VARCHAR",
                "NULL::INTEGER",
                "0::UBIGINT",
                "0::INTEGER",
            ],
        }
    }
}

/// Aggregations histograms and summaries support.
#[derive(Debug, Clone, Copy, PartialEq)]
enum HistAgg {
    Quantile(f64),
    Avg,
    Count,
    Rate,
}

/// Histograms, exponential histograms and summaries.
fn query_histograms(conn: &Connection, plan: &Plan, hk: HistKind, agg: Agg, col: &mut Collector) -> QResult<()> {
    let hagg = match agg {
        Agg::Quantile(q) => HistAgg::Quantile(q),
        Agg::Avg => HistAgg::Avg,
        Agg::Count | Agg::Increase => HistAgg::Count,
        Agg::Rate => HistAgg::Rate,
        Agg::Min | Agg::Max | Agg::Sum => {
            return Err(bad("histograms and summaries support pNN, avg, count, rate, increase"));
        }
    };
    let Plan { labels, bucket, first_bucket, .. } = plan;
    let (where_, table, cols) = (plan.filter.sql(), hk.table().name(), hk.columns());
    let summary = hk == HistKind::Summary;
    // Temporality is per point: one pass over the cumulative points, one over
    // the delta points. Summaries are always cumulative.
    let passes: &[(bool, String)] = if summary {
        &[(true, String::new())]
    } else {
        &[(true, format!(" AND {IS_CUMULATIVE}")), (false, format!(" AND ({IS_CUMULATIVE}) IS NOT TRUE"))]
    };
    let mut merged: HashMap<(Key, i64), Merged> = HashMap::new();
    // One merged entry becomes one output point: cap them as they appear.
    let mut caps = Caps::default();
    for (cumulative, only) in passes {
        let cumulative = *cumulative;
        // Cumulative: the last point per series & bucket. Delta: every point.
        // Both count whole buckets from `first_bucket` on, so the two kinds
        // cover the same interval.
        let sql = if cumulative {
            let last: Vec<String> = cols.iter().map(|c| format!("arg_max({c}, time)")).collect();
            format!(
                "SELECT {labels} AS k, series_id, {bucket} AS b, {} FROM {table} WHERE {where_}{only} \
                 GROUP BY ALL ORDER BY series_id, b",
                last.join(", "),
            )
        } else {
            format!(
                "SELECT {labels} AS k, series_id, {bucket} AS b, {} FROM {table} WHERE {where_}{only} \
                 ORDER BY series_id, b",
                cols.join(", "),
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(plan.params(false)), |r| {
            Ok(Row {
                key: r.get(0)?,
                series: r.get::<_, u64>(1)?,
                bucket: r.get(2)?,
                count: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                sum: r.get::<_, Option<f64>>(4)?.unwrap_or(f64::NAN),
                a: r.get(5)?,
                b: r.get(6)?,
                scale: r.get(7)?,
                zero: r.get::<_, Option<u64>>(8)?.unwrap_or(0),
                offset: r.get::<_, Option<i32>>(9)?.unwrap_or(0),
            })
        })?;
        let mut prev: Option<(u64, Hist)> = None;
        for row in rows {
            let row = row?;
            let h = Hist::from_row(hk, &row);
            let inc = if cumulative {
                let delta = match &prev {
                    Some((s, p)) if *s == row.series => Some(h.minus(p)),
                    _ => None,
                };
                prev = Some((row.series, h));
                delta
            } else {
                Some(h)
            };
            let Some(inc) = inc else { continue };
            if row.bucket < *first_bucket {
                continue;
            }
            let m = match merged.entry((row.key, row.bucket)) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => {
                    caps.add(&v.key().0)?;
                    v.insert(Merged::default())
                }
            };
            if summary {
                m.hist.count += inc.count;
                m.hist.sum += inc.sum;
                // Quantiles are not additive; average the reported value across series.
                if let HistAgg::Quantile(q) = hagg
                    && let Some(v) = summary_quantile(row.a.as_deref(), q)
                {
                    m.quantile_sum += v;
                    m.quantile_n += 1;
                }
            } else {
                m.hist.add(&inc);
            }
        }
    }
    for ((k, b), m) in merged {
        let v = match hagg {
            HistAgg::Quantile(_) if summary => {
                if m.quantile_n == 0 {
                    f64::NAN
                } else {
                    m.quantile_sum / m.quantile_n as f64
                }
            }
            HistAgg::Quantile(q) => m.hist.quantile(q).unwrap_or(f64::NAN),
            HistAgg::Avg => {
                if m.hist.count > 0.0 {
                    m.hist.sum / m.hist.count
                } else {
                    f64::NAN
                }
            }
            HistAgg::Count => m.hist.count,
            HistAgg::Rate => m.hist.count / plan.step as f64,
        };
        col.put(k, b, v)?;
    }
    Ok(())
}

struct Row {
    key: String,
    series: u64,
    bucket: i64,
    count: f64,
    sum: f64,
    /// histogram: bucket_counts; exp: positive counts; summary: quantiles
    a: Option<String>,
    /// histogram: explicit_bounds; exp: negative counts
    b: Option<String>,
    scale: Option<i32>,
    zero: u64,
    offset: i32,
}

#[derive(Default)]
struct Merged {
    hist: Hist,
    quantile_sum: f64,
    quantile_n: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Bound(f64);
impl Eq for Bound {}
impl PartialOrd for Bound {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Bound {
    fn cmp(&self, o: &Self) -> Ordering {
        self.0.total_cmp(&o.0)
    }
}

/// A histogram as upper bound -> count (the last bound is +Inf).
#[derive(Debug, Clone, Default)]
struct Hist {
    count: f64,
    sum: f64,
    buckets: BTreeMap<Bound, f64>,
}

impl Hist {
    fn from_row(kind: HistKind, r: &Row) -> Self {
        let mut h = Hist { count: r.count, sum: r.sum, buckets: BTreeMap::new() };
        match kind {
            HistKind::Explicit => {
                let counts: Vec<u64> = json_str(r.a.as_deref());
                let bounds: Vec<f64> = json_str(r.b.as_deref());
                for (i, c) in counts.iter().enumerate() {
                    let ub = bounds.get(i).copied().unwrap_or(f64::INFINITY);
                    *h.buckets.entry(Bound(ub)).or_default() += *c as f64;
                }
            }
            HistKind::Exponential => {
                let pos: Vec<u64> = json_str(r.a.as_deref());
                let neg: Vec<u64> = json_str(r.b.as_deref());
                // OTLP defines scales -10..=20; anything else is bad data and
                // would overflow below.
                let scale = r.scale.unwrap_or(0).clamp(-10, 20);
                let base = 2f64.powf(2f64.powi(-scale));
                // Zero and negative buckets are folded into one bucket with upper bound 0.
                let at_or_below_zero = r.zero as f64 + neg.iter().sum::<u64>() as f64;
                if at_or_below_zero > 0.0 {
                    h.buckets.insert(Bound(0.0), at_or_below_zero);
                }
                // Bucket i covers (base^i, base^(i+1)]. An empty bucket at the
                // lower edge of the first one gives interpolation its real
                // lower bound (not 0), and empty buckets are kept so gaps
                // don't stretch the next bucket's range.
                if !pos.is_empty() {
                    h.buckets.entry(Bound(base.powf(r.offset as f64))).or_insert(0.0);
                }
                for (k, c) in pos.iter().enumerate() {
                    let idx = r.offset as f64 + k as f64;
                    *h.buckets.entry(Bound(base.powf(idx + 1.0))).or_default() += *c as f64;
                }
            }
            HistKind::Summary => {}
        }
        h
    }

    /// self - prev; a decrease means the series restarted, then self is the increase.
    fn minus(&self, prev: &Hist) -> Hist {
        if self.count < prev.count {
            return self.clone();
        }
        let mut out = Hist { count: self.count - prev.count, sum: self.sum - prev.sum, buckets: BTreeMap::new() };
        for (b, c) in &self.buckets {
            let d = c - prev.buckets.get(b).copied().unwrap_or(0.0);
            if d < 0.0 {
                return self.clone();
            }
            // Keep empty buckets: they carry bucket edges for interpolation.
            out.buckets.insert(*b, d);
        }
        out
    }

    fn add(&mut self, o: &Hist) {
        self.count += o.count;
        if o.sum.is_finite() {
            self.sum += o.sum;
        }
        for (b, c) in &o.buckets {
            *self.buckets.entry(*b).or_default() += c;
        }
    }

    fn quantile(&self, q: f64) -> Option<f64> {
        let total: f64 = self.buckets.values().sum();
        if total <= 0.0 {
            return None;
        }
        let rank = q * total;
        let mut cum = 0.0;
        let mut lower = 0.0f64;
        let mut first = true;
        for (Bound(ub), c) in &self.buckets {
            if cum + c >= rank && *c > 0.0 {
                if ub.is_infinite() {
                    return Some(lower);
                }
                // The first bucket has no lower bound; assume 0 for positive values.
                let lo = if first { if *ub > 0.0 { 0.0 } else { *ub } } else { lower };
                return Some(lo + (ub - lo) * ((rank - cum) / c));
            }
            cum += c;
            lower = *ub;
            first = false;
        }
        Some(lower)
    }
}

#[derive(serde::Deserialize)]
struct QV {
    quantile: f64,
    value: f64,
}

fn summary_quantile(json: Option<&str>, q: f64) -> Option<f64> {
    let qs: Vec<QV> = json.and_then(|s| serde_json::from_str(s).ok())?;
    qs.iter().min_by(|a, b| (a.quantile - q).abs().total_cmp(&(b.quantile - q).abs())).map(|x| x.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(bounds: &[f64], counts: &[f64]) -> Hist {
        let mut h = Hist::default();
        for (i, c) in counts.iter().enumerate() {
            h.buckets.insert(Bound(bounds.get(i).copied().unwrap_or(f64::INFINITY)), *c);
            h.count += c;
        }
        h
    }

    #[test]
    fn quantile_interpolates() {
        // 100 values uniformly in (0,10], (10,20]
        let h = hist(&[10.0, 20.0], &[50.0, 50.0, 0.0]);
        assert!((h.quantile(0.5).unwrap() - 10.0).abs() < 1e-9);
        assert!((h.quantile(0.75).unwrap() - 15.0).abs() < 1e-9);
        assert!((h.quantile(0.25).unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn quantile_in_inf_bucket_returns_last_bound() {
        let h = hist(&[10.0], &[0.0, 5.0]);
        assert_eq!(h.quantile(0.9), Some(10.0));
    }

    #[test]
    fn minus_detects_reset() {
        let a = hist(&[10.0], &[5.0, 1.0]);
        let b = hist(&[10.0], &[8.0, 3.0]);
        let d = b.minus(&a);
        assert_eq!(d.count, 5.0);
        assert_eq!(d.buckets[&Bound(10.0)], 3.0);
        let reset = a.minus(&b);
        assert_eq!(reset.count, a.count);
    }

    #[test]
    fn exp_histogram_percentile_uses_bucket_lower_bound() {
        // scale 20: tiny buckets; every value is 100.0
        let scale = 20;
        let base = 2f64.powf(2f64.powi(-scale));
        let idx = (100f64.ln() / base.ln()).ceil() as i32 - 1; // bucket containing 100
        let r = Row {
            key: String::new(),
            series: 1,
            bucket: 0,
            count: 10.0,
            sum: 1000.0,
            a: Some("[10]".into()),
            b: Some("[]".into()),
            scale: Some(scale),
            zero: 0,
            offset: idx,
        };
        let h = Hist::from_row(HistKind::Exponential, &r);
        for q in [0.5, 0.99] {
            let v = h.quantile(q).unwrap();
            assert!((v - 100.0).abs() < 0.01, "p{q} = {v}");
        }
        // Still right after a cumulative difference.
        let mut later = h.clone();
        for c in later.buckets.values_mut() {
            *c *= 2.0;
        }
        later.count *= 2.0;
        let d = later.minus(&h);
        assert!((d.quantile(0.5).unwrap() - 100.0).abs() < 0.01);
    }

    #[test]
    fn caps_stop_at_max_series_and_points() {
        let mut caps = Caps::default();
        for i in 0..MAX_SERIES {
            caps.add(&i.to_string()).unwrap();
        }
        caps.add(&"0".to_string()).expect("known series");
        assert!(caps.add(&"new".to_string()).is_err(), "series cap");
        let mut caps = Caps::default();
        for _ in 0..MAX_POINTS {
            caps.add(&"k".to_string()).unwrap();
        }
        assert!(caps.add(&"k".to_string()).is_err(), "point cap");
    }

    #[test]
    fn out_of_range_exp_histogram_scale_does_not_panic() {
        for scale in [i32::MIN, i32::MAX] {
            let r = Row {
                key: String::new(),
                series: 1,
                bucket: 0,
                count: 1.0,
                sum: 1.0,
                a: Some("[1]".into()),
                b: None,
                scale: Some(scale),
                zero: 0,
                offset: 0,
            };
            let h = Hist::from_row(HistKind::Exponential, &r);
            assert!(h.quantile(0.5).unwrap().is_finite());
        }
    }

    #[test]
    fn agg_parse() {
        assert_eq!(Agg::parse("p95").unwrap(), Agg::Quantile(0.95));
        assert_eq!(Agg::parse("p99.9").unwrap().name(), "p99.9");
        assert!(Agg::parse("p200").is_err());
        assert!(Agg::parse("median").is_err());
    }

    #[test]
    fn summary_q() {
        let j = r#"[{"quantile":0.5,"value":1.0},{"quantile":0.99,"value":9.0}]"#;
        assert_eq!(summary_quantile(Some(j), 0.95), Some(9.0));
    }
}
