//! Shared HTTP API types for otel-ducklake.
//!
//! The server derives its OpenAPI document from these types, and the `odl`
//! CLI/TUI deserializes responses into them. Changing a type here changes the
//! public API: regenerate `docs/openapi.json` (`otel-ducklake openapi`).
//!
//! Conventions:
//! - Times are RFC 3339 strings in UTC.
//! - Trace ids are 32 lowercase hex chars, span ids 16 lowercase hex chars.
//! - Durations are reported in milliseconds (`*_ms`, float) unless the field
//!   name says otherwise (`duration_ns`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Attribute map. Values are stringified OTLP `AnyValue`s (see docs/data-model.md).
pub type Attributes = BTreeMap<String, String>;

// ---------------------------------------------------------------------------
// Errors & health
// ---------------------------------------------------------------------------

/// Error body returned with every non-2xx response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiError {
    /// Machine readable error code, e.g. `bad_request`, `not_found`, `timeout`.
    pub error: String,
    /// Human readable message.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// `ok` when the server is up.
    pub status: String,
    pub version: String,
}

// ---------------------------------------------------------------------------
// Common query parameters
// ---------------------------------------------------------------------------

/// Time window shared by all query endpoints.
///
/// Give either `since` (relative, e.g. `15m`, `6h`, `2d`) or `from`/`to`.
/// Default window: the last hour.
#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct TimeWindow {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending now, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Stats {
    pub version: String,
    pub uptime_secs: u64,
    pub ingest: IngestStats,
    pub storage: StorageStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IngestStats {
    /// Per signal counters since process start.
    pub signals: Vec<SignalIngestStats>,
    /// Rows waiting in memory for the next flush.
    pub buffered_rows: u64,
    /// Approximate bytes waiting in memory for the next flush.
    pub buffered_bytes: u64,
    /// Requests are rejected (HTTP 429 / gRPC RESOURCE_EXHAUSTED) above this.
    pub max_buffer_bytes: u64,
    pub flushes: u64,
    pub flush_errors: u64,
    pub last_flush: Option<FlushInfo>,
    pub last_flush_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SignalIngestStats {
    /// `traces`, `logs` or `metrics`.
    pub signal: String,
    pub requests: u64,
    /// Spans, log records or metric data points accepted.
    pub accepted: u64,
    /// Items refused because of back-pressure or invalid payloads.
    pub rejected: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FlushInfo {
    pub at: DateTime<Utc>,
    pub rows: u64,
    pub duration_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StorageStats {
    /// `duckdb`, `sqlite` or `postgres`.
    pub catalog_type: String,
    pub data_path: String,
    pub snapshot_id: i64,
    pub tables: Vec<TableStats>,
    pub last_maintenance: Option<MaintenanceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TableStats {
    pub name: String,
    /// Rows in live Parquet files (excludes rows still inlined in the catalog).
    pub rows: i64,
    pub files: i64,
    pub bytes: i64,
    /// Rows currently inlined in the catalog database, waiting to be flushed to Parquet.
    pub inlined_rows: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MaintenanceInfo {
    pub task: String,
    pub at: DateTime<Utc>,
    pub duration_ms: f64,
    pub ok: bool,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Services & operations (RED metrics derived from spans)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct ServiceQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    /// Only count entry spans (kind SERVER/CONSUMER or root spans). Default true.
    pub entry_only: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct OperationQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    /// Only operations of this service.
    pub service: Option<String>,
    /// Only entry spans (kind SERVER/CONSUMER or root spans). Default true.
    pub entry_only: Option<bool>,
}

/// Rate, errors and duration for a service or operation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RedStats {
    pub span_count: u64,
    pub error_count: u64,
    /// error_count / span_count, 0..1.
    pub error_rate: f64,
    /// Spans per second over the window.
    pub rate_per_sec: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceSummary {
    pub name: String,
    #[serde(flatten)]
    pub red: RedStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OperationSummary {
    pub service: String,
    pub name: String,
    pub kind: String,
    #[serde(flatten)]
    pub red: RedStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct SpanSeriesQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    pub service: Option<String>,
    pub operation: Option<String>,
    /// Bucket width, e.g. `30s`, `5m`. Default picks ~60 buckets.
    pub step: Option<String>,
    /// Only entry spans. Default true.
    pub entry_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SpanSeries {
    pub step_secs: u64,
    pub points: Vec<SpanSeriesPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SpanSeriesPoint {
    pub time: DateTime<Utc>,
    pub count: u64,
    pub errors: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

// ---------------------------------------------------------------------------
// Traces
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct TraceQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    /// Match traces with a span from this service.
    pub service: Option<String>,
    /// Match traces with a span with this name.
    pub operation: Option<String>,
    /// Only traces that contain an error span.
    pub errors_only: Option<bool>,
    /// Matching span must take at least this long (ms).
    pub min_duration_ms: Option<f64>,
    /// Matching span must take at most this long (ms).
    pub max_duration_ms: Option<f64>,
    /// Attribute filter `key=value` on span or resource attributes. Repeat with commas: `a=1,b=2`.
    pub attributes: Option<String>,
    /// Max traces returned (default 50, max 1000).
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraceSummary {
    pub trace_id: String,
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    /// Service of the root span, if the root span was received.
    pub root_service: Option<String>,
    pub root_name: Option<String>,
    pub span_count: u64,
    pub error_count: u64,
    pub services: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct TraceGetQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Trace {
    pub trace_id: String,
    /// Spans ordered by start time.
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    /// UNSPECIFIED, INTERNAL, SERVER, CLIENT, PRODUCER or CONSUMER.
    pub kind: String,
    pub service_name: String,
    pub start_time: DateTime<Utc>,
    pub duration_ns: i64,
    /// UNSET, OK or ERROR.
    pub status_code: String,
    pub status_message: String,
    pub trace_state: String,
    pub scope_name: String,
    pub scope_version: String,
    pub attributes: Attributes,
    pub resource_attributes: Attributes,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SpanEvent {
    pub time: DateTime<Utc>,
    pub name: String,
    pub attributes: Attributes,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
    pub trace_state: String,
    pub attributes: Attributes,
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct LogQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    pub service: Option<String>,
    /// Minimum severity number (1-24). 9=INFO, 13=WARN, 17=ERROR, 21=FATAL.
    pub min_severity: Option<u8>,
    /// Case-insensitive substring match on the body.
    pub q: Option<String>,
    /// Only logs of this trace (32 hex chars).
    pub trace_id: Option<String>,
    /// Attribute filter `key=value` on log or resource attributes. Repeat with commas.
    pub attributes: Option<String>,
    /// Max records (default 100, max 5000).
    pub limit: Option<u32>,
    /// `desc` (newest first, default) or `asc`.
    pub order: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LogRecord {
    pub time: DateTime<Utc>,
    pub observed_time: DateTime<Utc>,
    pub service_name: String,
    pub severity_number: u8,
    pub severity_text: String,
    pub body: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub event_name: String,
    pub scope_name: String,
    pub attributes: Attributes,
    pub resource_attributes: Attributes,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct LogSeriesQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    pub service: Option<String>,
    pub q: Option<String>,
    pub step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LogSeries {
    pub step_secs: u64,
    pub points: Vec<LogSeriesPoint>,
}

/// Log counts per bucket, split by severity band.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LogSeriesPoint {
    pub time: DateTime<Utc>,
    pub total: u64,
    /// severity 1-8 (TRACE/DEBUG) or unset
    pub debug: u64,
    /// severity 9-12
    pub info: u64,
    /// severity 13-16
    pub warn: u64,
    /// severity 17-24 (ERROR/FATAL)
    pub error: u64,
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct MetricListQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    pub service: Option<String>,
    /// Substring filter on the metric name.
    pub q: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricInfo {
    pub name: String,
    /// gauge, sum, histogram, exponential_histogram or summary.
    pub metric_type: String,
    pub unit: String,
    pub description: String,
    /// `cumulative`, `delta` or empty for gauges.
    pub temporality: String,
    pub series_count: u64,
    pub point_count: u64,
    pub services: Vec<String>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct MetricQuery {
    /// Metric name.
    pub name: String,
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    pub service: Option<String>,
    /// Aggregation: `avg` (default for gauges), `min`, `max`, `sum`,
    /// `rate` (per-second increase, default for monotonic sums), `increase`,
    /// `p50`/`p90`/`p95`/`p99` (histograms and summaries; default `p95`), `count`.
    pub agg: Option<String>,
    /// Comma separated attribute keys to group by. `service.name` is allowed.
    pub group_by: Option<String>,
    /// Attribute filter `key=value`, comma separated.
    pub attributes: Option<String>,
    /// Bucket width, e.g. `1m`. Default picks ~60 buckets.
    pub step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricQueryResult {
    pub name: String,
    pub metric_type: String,
    pub unit: String,
    pub agg: String,
    pub step_secs: u64,
    pub series: Vec<MetricSeries>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricSeries {
    pub labels: Attributes,
    pub points: Vec<MetricPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricPoint {
    pub time: DateTime<Utc>,
    pub value: f64,
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SqlRequest {
    /// One read-only `SELECT`/`WITH` statement. Tables live in schema `lake.otel`
    /// (the default search path, so `FROM spans` works).
    pub query: String,
    /// Max rows returned (default 1000, max 100000).
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SqlResponse {
    pub columns: Vec<SqlColumn>,
    /// Row values as JSON. Non-JSON types (timestamps, UUIDs, maps) are rendered as strings.
    #[schema(value_type = Vec<Vec<Object>>)]
    pub rows: Vec<Vec<serde_json::Value>>,
    /// True when more rows existed than `limit`.
    pub truncated: bool,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SqlColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

/// Query types that carry a time window (`from`, `to`, `since`).
pub trait HasWindow {
    fn window(&self) -> TimeWindow;
}

macro_rules! has_window {
    ($($t:ty),*) => {$(
        impl HasWindow for $t {
            fn window(&self) -> TimeWindow {
                TimeWindow { from: self.from, to: self.to, since: self.since.clone() }
            }
        }
    )*};
}

has_window!(
    ServiceQuery,
    OperationQuery,
    SpanSeriesQuery,
    TraceQuery,
    TraceGetQuery,
    LogQuery,
    LogSeriesQuery,
    MetricListQuery,
    MetricQuery
);

/// Parse `k=v,k2=v2` attribute filters. Values may not contain commas.
pub fn parse_attribute_filter(s: &str) -> Result<Vec<(String, String)>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p
                .split_once('=')
                .ok_or_else(|| format!("attribute filter `{p}` must look like key=value"))?;
            let k = k.trim();
            if k.is_empty() {
                return Err(format!("attribute filter `{p}` has an empty key"));
            }
            Ok((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_filter_parses() {
        let f = parse_attribute_filter("http.route=/a, k = v ,").unwrap();
        assert_eq!(
            f,
            vec![("http.route".into(), "/a".into()), ("k".into(), "v".into())]
        );
        assert!(parse_attribute_filter("novalue").is_err());
        assert!(parse_attribute_filter("=x").is_err());
    }

    #[test]
    fn window_query_roundtrip() {
        let q: TraceQuery = serde_json::from_str(r#"{"since":"15m","service":"api"}"#).unwrap();
        assert_eq!(q.window().since.as_deref(), Some("15m"));
        assert_eq!(q.service.as_deref(), Some("api"));
    }
}
