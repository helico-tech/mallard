//! Shared HTTP API types for otel-ducklake.
//!
//! The server derives its OpenAPI document from these types, and the `odl`
//! CLI/TUI deserializes responses into them. Changing a type here changes the
//! public API: regenerate `docs/otel/openapi.json`
//! (`UPDATE_OPENAPI=1 cargo test -p odl-server --test openapi`).
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

/// A string that is not one of the values of a closed set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} {value:?}")]
pub struct UnknownValue {
    pub kind: &'static str,
    pub value: String,
}

/// A closed set of values sent as strings: serde names, `as_str`,
/// `Display` and `FromStr` from one list.
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident: $kind:literal { $($(#[$vm:meta])* $v:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        pub enum $name {
            $($(#[$vm])* #[serde(rename = $s)] $v),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$v => $s),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = UnknownValue;

            fn from_str(s: &str) -> Result<Self, UnknownValue> {
                match s {
                    $($s => Ok($name::$v),)+
                    _ => Err(UnknownValue { kind: $kind, value: s.to_string() }),
                }
            }
        }
    };
}

string_enum! {
    /// OTLP span kind.
    SpanKind: "span kind" {
        Unspecified = "UNSPECIFIED",
        Internal = "INTERNAL",
        Server = "SERVER",
        Client = "CLIENT",
        Producer = "PRODUCER",
        Consumer = "CONSUMER",
    }
}

string_enum! {
    /// OTLP span status code.
    SpanStatus: "span status" {
        Unset = "UNSET",
        Ok = "OK",
        Error = "ERROR",
    }
}

string_enum! {
    /// Kind of metric data point.
    MetricType: "metric type" {
        Gauge = "gauge",
        Sum = "sum",
        Histogram = "histogram",
        ExponentialHistogram = "exponential_histogram",
        Summary = "summary",
    }
}

string_enum! {
    /// Aggregation temporality of sums and histograms; empty for gauges and summaries.
    Temporality: "temporality" {
        Unspecified = "",
        Delta = "delta",
        Cumulative = "cumulative",
    }
}

string_enum! {
    /// Telemetry signal.
    Signal: "signal" {
        Traces = "traces",
        Logs = "logs",
        Metrics = "metrics",
    }
}

string_enum! {
    /// DuckLake catalog backend.
    CatalogType: "catalog type" {
        DuckDb = "duckdb",
        Sqlite = "sqlite",
        Postgres = "postgres",
        MySql = "mysql",
    }
}

string_enum! {
    /// Sort order of log records.
    LogOrder: "order" {
        /// Oldest first.
        Asc = "asc",
        /// Newest first (default).
        Desc = "desc",
    }
}

// ---------------------------------------------------------------------------
// Errors & health
// ---------------------------------------------------------------------------

/// Error body returned with every non-2xx response (shared with the Sentry API).
pub use mallard_common::ApiError;

/// Response header of `GET /api/v1/traces` and `GET /api/v1/logs` with the
/// cursor for the next page (as `cursor=`); missing on the last page.
pub const NEXT_CURSOR_HEADER: &str = "x-next-cursor";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Stats {
    pub version: String,
    pub uptime_secs: u64,
    pub ingest: IngestStats,
    pub storage: StorageStats,
    /// What this server allows and keeps. Missing from older servers.
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
}

/// Server settings a client needs to know before it queries or sends data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Capabilities {
    /// `POST /api/v1/sql` is enabled (`query.enable_sql`).
    pub sql: bool,
    /// Longest query window (`query.max_window`), in seconds.
    pub max_window_secs: u64,
    /// How long data is kept, in seconds (0 = forever).
    pub retention_secs: RetentionSecs,
    /// Buffered data is written at least this often (`ingest.flush_interval`),
    /// so new data can take this long to show up in queries.
    pub flush_interval_secs: f64,
    /// OTLP ingest needs a bearer token (`auth.ingest_tokens` is set).
    pub ingest_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RetentionSecs {
    pub traces: u64,
    pub logs: u64,
    pub metrics: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IngestStats {
    /// Per signal counters since process start.
    pub signals: Vec<SignalIngestStats>,
    /// Rows waiting in memory for the next flush.
    pub buffered_rows: u64,
    /// Approximate bytes waiting in memory for the next flush.
    pub buffered_bytes: u64,
    /// Requests are rejected (HTTP 429 / gRPC UNAVAILABLE) above this.
    pub max_buffer_bytes: u64,
    pub flushes: u64,
    pub flush_errors: u64,
    /// Rows dropped because their table refused them with a permanent
    /// error, e.g. a column changed by hand (details in the server log).
    #[serde(default)]
    pub dropped_rows: u64,
    pub last_flush: Option<FlushInfo>,
    pub last_flush_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SignalIngestStats {
    pub signal: Signal,
    pub requests: u64,
    /// Spans, log records or metric data points accepted.
    pub accepted: u64,
    /// Items refused because the write buffer was full (HTTP 429 / gRPC
    /// UNAVAILABLE). Exporters retry them, so they usually arrive later.
    #[serde(default)]
    pub throttled: u64,
    /// Requests refused with back-pressure (HTTP 429 / gRPC UNAVAILABLE):
    /// by the in-flight memory limit before the body is read (their items
    /// are unknown, so not in `throttled`), or by the full write buffer.
    /// Exporters retry them.
    #[serde(default)]
    pub throttled_requests: u64,
    /// Items refused for good: invalid (e.g. a span without a valid trace
    /// or span id, reported as OTLP partial success) or in a request too
    /// large for the buffer.
    #[serde(default)]
    pub invalid: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FlushInfo {
    pub at: DateTime<Utc>,
    pub rows: u64,
    pub duration_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StorageStats {
    pub catalog_type: CatalogType,
    pub data_path: String,
    pub snapshot_id: i64,
    pub tables: Vec<TableStats>,
    pub last_maintenance: Option<MaintenanceInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TableStats {
    pub name: String,
    /// Rows in live Parquet files (excludes rows still inlined in the catalog).
    pub rows: i64,
    pub files: i64,
    pub bytes: i64,
    /// Rows currently inlined in the catalog database, waiting to be flushed to Parquet.
    pub inlined_rows: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ServiceSummary {
    pub name: String,
    #[serde(flatten)]
    pub red: RedStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct OperationSummary {
    pub service: String,
    pub name: String,
    pub kind: SpanKind,
    #[serde(flatten)]
    pub red: RedStats,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpanSeries {
    pub step_secs: u64,
    pub points: Vec<SpanSeriesPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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
    /// Continue after the last page: the `x-next-cursor` header of the
    /// previous response (same filters and window).
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct TraceGetQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Trace {
    pub trace_id: String,
    /// Spans ordered by start time.
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: SpanKind,
    pub service_name: String,
    pub start_time: DateTime<Utc>,
    pub duration_ns: i64,
    pub status_code: SpanStatus,
    pub status_message: String,
    pub trace_state: String,
    pub scope_name: String,
    pub scope_version: String,
    pub attributes: Attributes,
    pub resource_attributes: Attributes,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpanEvent {
    pub time: DateTime<Utc>,
    pub name: String,
    pub attributes: Attributes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
    pub trace_state: String,
    pub attributes: Attributes,
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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
    pub order: Option<LogOrder>,
    /// Continue after the last page: the `x-next-cursor` header of the
    /// previous response (same filters, window and order).
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LogSeries {
    pub step_secs: u64,
    pub points: Vec<LogSeriesPoint>,
}

/// Log counts per bucket, split by severity band.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricInfo {
    pub name: String,
    pub metric_type: MetricType,
    pub unit: String,
    pub description: String,
    pub temporality: Temporality,
    pub series_count: u64,
    pub point_count: u64,
    pub services: Vec<String>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
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
    /// Also return exemplars: per series and bucket the one with the
    /// highest value that links to a trace.
    pub exemplars: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricQueryResult {
    pub name: String,
    pub metric_type: MetricType,
    pub unit: String,
    pub agg: String,
    pub step_secs: u64,
    pub series: Vec<MetricSeries>,
    /// Only with `exemplars=true` (at most 1000, oldest first). Missing
    /// from older servers.
    #[serde(default)]
    pub exemplars: Vec<MetricExemplar>,
}

/// A sample measurement recorded with a metric point, linking it to the
/// trace (span) that was active at the time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricExemplar {
    /// Labels of the series it belongs to (the `group_by` keys).
    pub labels: Attributes,
    pub time: DateTime<Utc>,
    pub value: f64,
    /// 32 hex characters.
    pub trace_id: String,
    /// 16 hex characters.
    pub span_id: Option<String>,
    /// The exemplar's filtered attributes (those the aggregation dropped).
    pub attributes: Attributes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricSeries {
    pub labels: Attributes,
    pub points: Vec<MetricPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MetricPoint {
    pub time: DateTime<Utc>,
    pub value: f64,
}

// ---------------------------------------------------------------------------
// Attributes and schema (what to filter on, what to query)
// ---------------------------------------------------------------------------

string_enum! {
    /// Which data `GET /api/v1/attributes` looks at.
    AttributeSignal: "signal" {
        Spans = "spans",
        Logs = "logs",
        Metrics = "metrics",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct AttributeQuery {
    /// `spans`, `logs` or `metrics`.
    pub signal: AttributeSignal,
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d`. Ignored when `from` is set.
    pub since: Option<String>,
    pub service: Option<String>,
    /// Only this metric (signal `metrics` only).
    pub metric: Option<String>,
    /// Top values of this key instead of the keys.
    pub key: Option<String>,
    /// Max keys or values returned (default 50, max 1000).
    pub limit: Option<u32>,
}

/// Attribute keys (or values of one key) with how many sampled rows have them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AttributeStats {
    /// Matching rows in the window.
    pub total_rows: u64,
    /// Rows counted: a random sample of at most 10,000 of the matching rows.
    pub sampled_rows: u64,
    /// Most frequent first.
    pub items: Vec<AttributeCount>,
}

/// One attribute key, or one value of the key asked for. Keys of span, log or
/// point attributes and of resource attributes are counted together, as the
/// `attributes` filters match both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AttributeCount {
    pub key: String,
    /// The value, when a `key` was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Sampled rows with this key (or value).
    pub count: u64,
}

/// The tables `POST /api/v1/sql` can query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Schema {
    pub tables: Vec<SchemaTable>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SchemaTable {
    /// Table name; also reachable as `lake.<schema>.<name>`.
    pub name: String,
    /// Time column the table is partitioned and pruned by; filter on it.
    pub time_column: String,
    /// Columns in storage order.
    pub columns: Vec<SqlColumn>,
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SqlRequest {
    /// One read-only `SELECT`/`WITH` statement. Tables live in schema `lake.otel`
    /// (the default search path, so `FROM spans` works).
    pub query: String,
    /// Max rows returned (default 1000, max 100000).
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SqlResponse {
    pub columns: Vec<SqlColumn>,
    /// Row values as JSON. Non-JSON types (timestamps, UUIDs, maps) are rendered as strings.
    #[schema(value_type = Vec<Vec<Object>>)]
    pub rows: Vec<Vec<serde_json::Value>>,
    /// True when more rows existed than `limit`, or the rows reached the
    /// server's size cap (32 MiB of row data).
    pub truncated: bool,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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
    AttributeQuery,
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

/// An invalid part of an attribute filter.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttributeFilterError {
    #[error("attribute filter `{0}` must look like key=value")]
    NotKeyValue(String),
    #[error("attribute filter `{0}` has an empty key")]
    EmptyKey(String),
}

/// Parse `k=v,k2=v2` attribute filters. Values may not contain commas.
pub fn parse_attribute_filter(s: &str) -> Result<Vec<(String, String)>, AttributeFilterError> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').ok_or_else(|| AttributeFilterError::NotKeyValue(p.to_string()))?;
            let k = k.trim();
            if k.is_empty() {
                return Err(AttributeFilterError::EmptyKey(p.to_string()));
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
        assert_eq!(f, vec![("http.route".into(), "/a".into()), ("k".into(), "v".into())]);
        assert_eq!(parse_attribute_filter("novalue"), Err(AttributeFilterError::NotKeyValue("novalue".into())));
        assert_eq!(parse_attribute_filter("=x"), Err(AttributeFilterError::EmptyKey("=x".into())));
    }

    #[test]
    fn closed_sets_keep_their_wire_strings() {
        assert_eq!(serde_json::to_string(&SpanKind::Server).unwrap(), r#""SERVER""#);
        assert_eq!(serde_json::to_string(&Temporality::Unspecified).unwrap(), r#""""#);
        assert_eq!(
            serde_json::from_str::<MetricType>(r#""exponential_histogram""#).unwrap(),
            MetricType::ExponentialHistogram
        );
        assert_eq!("cumulative".parse(), Ok(Temporality::Cumulative));
        assert_eq!(SpanStatus::Error.to_string(), "ERROR");
        assert!("sideways".parse::<LogOrder>().is_err());
        let q: LogQuery = serde_json::from_str(r#"{"order":"asc"}"#).unwrap();
        assert_eq!(q.order, Some(LogOrder::Asc));
    }

    #[test]
    fn window_query_roundtrip() {
        let q: TraceQuery = serde_json::from_str(r#"{"since":"15m","service":"api"}"#).unwrap();
        assert_eq!(q.window().since.as_deref(), Some("15m"));
        assert_eq!(q.service.as_deref(), Some("api"));
    }
}
