//! Server configuration.
//!
//! Loaded from a TOML file (optional) and then overridden by environment
//! variables with the `ODL_` prefix, using `__` as section separator, e.g.
//! `ODL_STORAGE__DATA_PATH=s3://bucket/otel/`.
//! Values of the form `${NAME}` are replaced by the environment variable NAME,
//! so secrets can stay out of the file.
//!
//! An embedding application (mallard) uses only the sections it does not
//! own itself: `ingest`, `retention`, `maintenance` and `query`.

use std::{path::Path, time::Duration};

use anyhow::{Result, bail};
use mallard_common::{ByteSize, LogFormat, duration};
use mallard_duck::{DuckDbConfig, StorageConfig};
use serde::{Deserialize, Serialize};

use crate::store::schema::Signal;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub ingest: IngestConfig,
    pub retention: RetentionConfig,
    pub maintenance: MaintenanceConfig,
    pub query: QueryConfig,
    pub auth: AuthConfig,
    pub duckdb: DuckDbConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// OTLP/gRPC listen address.
    pub otlp_grpc_addr: String,
    /// OTLP/HTTP listen address.
    pub otlp_http_addr: String,
    /// Query API (OpenAPI) listen address.
    pub api_addr: String,
    /// Log format: `text` or `json`.
    pub log_format: LogFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// Flush buffered rows to DuckLake at least this often, e.g. `5s`.
    #[serde(with = "duration")]
    pub flush_interval: Duration,
    /// Flush early when this many rows are buffered.
    pub flush_rows: u64,
    /// Reject new requests (back-pressure) when the buffer holds more than this, e.g. `512MB`.
    /// Also the budget for request bodies being received and decoded.
    pub max_buffer_bytes: ByteSize,
    /// Max size of one OTLP request body (after decompression), e.g. `32MB`.
    pub max_request_bytes: ByteSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    /// Keep spans this long (`0` = forever), e.g. `7d`.
    #[serde(with = "duration")]
    pub traces: Duration,
    #[serde(with = "duration")]
    pub logs: Duration,
    #[serde(with = "duration")]
    pub metrics: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaintenanceConfig {
    /// Run maintenance tasks in this process. Replicas sharing a catalog need no
    /// change: only the holder of a lease in the catalog runs them.
    pub enabled: bool,
    /// Flush inlined rows and merge small files this often.
    #[serde(with = "duration")]
    pub compact_interval: Duration,
    /// Files below this size are merged by the frequent compaction.
    pub small_file_size: ByteSize,
    /// Merge everything below `target_file_size` this often.
    #[serde(with = "duration")]
    pub full_compact_interval: Duration,
    /// Retention deletes, snapshot expiry and file cleanup this often.
    #[serde(with = "duration")]
    pub retention_interval: Duration,
    /// Keep old snapshots (time travel) this long.
    #[serde(with = "duration")]
    pub snapshot_retention: Duration,
    /// Delete files no longer referenced after this grace period.
    #[serde(with = "duration")]
    pub file_cleanup_delay: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryConfig {
    /// Number of concurrent query connections.
    pub max_concurrent: usize,
    /// Abort queries running longer than this.
    #[serde(with = "duration")]
    pub timeout: Duration,
    /// Enable `POST /api/v1/sql` (read-only ad-hoc SQL). Requires `auth.api_tokens`.
    /// Treat those tokens as admin access: a query's memory and CPU use is
    /// bounded only by `timeout` and `duckdb.memory_limit`, not fully.
    pub enable_sql: bool,
    /// Max time span a single query may cover, e.g. `31d`.
    #[serde(with = "duration")]
    pub max_window: Duration,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Bearer tokens accepted by the query API. Empty = no auth.
    pub api_tokens: Vec<String>,
    /// Bearer tokens accepted by OTLP receivers. Empty = no auth.
    pub ingest_tokens: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            otlp_grpc_addr: "0.0.0.0:4317".into(),
            otlp_http_addr: "0.0.0.0:4318".into(),
            api_addr: "0.0.0.0:8080".into(),
            log_format: LogFormat::Text,
        }
    }
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(5),
            flush_rows: 200_000,
            max_buffer_bytes: ByteSize::mb(512),
            max_request_bytes: ByteSize::mb(32),
        }
    }
}

const fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

impl RetentionConfig {
    /// Retention of one signal; zero keeps data forever.
    pub(crate) fn of(&self, s: Signal) -> Duration {
        match s {
            Signal::Traces => self.traces,
            Signal::Logs => self.logs,
            Signal::Metrics => self.metrics,
        }
    }
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self { traces: days(7), logs: days(7), metrics: days(30) }
    }
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            compact_interval: Duration::from_secs(10 * 60),
            small_file_size: ByteSize::mb(16),
            full_compact_interval: Duration::from_secs(6 * 3600),
            retention_interval: Duration::from_secs(3600),
            snapshot_retention: Duration::from_secs(3600),
            file_cleanup_delay: Duration::from_secs(15 * 60),
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self { max_concurrent: 4, timeout: Duration::from_secs(30), enable_sql: false, max_window: days(31) }
    }
}

impl Config {
    /// Load config: defaults <- TOML file <- `ODL_*` environment variables
    /// (see [`mallard_common::config`]), then validate it.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let cfg: Config = mallard_common::config::load(path, "ODL_")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Check ranges and combinations. Units are checked while parsing.
    pub fn validate(&self) -> Result<()> {
        let (i, m, q, r) = (&self.ingest, &self.maintenance, &self.query, &self.retention);
        for (name, d) in [
            ("ingest.flush_interval", i.flush_interval),
            ("maintenance.compact_interval", m.compact_interval),
            ("maintenance.full_compact_interval", m.full_compact_interval),
            ("maintenance.retention_interval", m.retention_interval),
            ("maintenance.snapshot_retention", m.snapshot_retention),
            ("maintenance.file_cleanup_delay", m.file_cleanup_delay),
            ("query.timeout", q.timeout),
            ("query.max_window", q.max_window),
            ("retention.traces", r.traces),
            ("retention.logs", r.logs),
            ("retention.metrics", r.metrics),
        ] {
            if d > MAX_DURATION {
                bail!("{name} is longer than 100 years");
            }
        }
        for (name, d) in [
            ("ingest.flush_interval", i.flush_interval),
            ("maintenance.compact_interval", m.compact_interval),
            ("maintenance.full_compact_interval", m.full_compact_interval),
            ("maintenance.retention_interval", m.retention_interval),
            ("query.timeout", q.timeout),
            ("query.max_window", q.max_window),
        ] {
            if d.is_zero() {
                bail!("{name} must be greater than 0");
            }
        }
        if i.flush_rows == 0 {
            bail!("ingest.flush_rows must be greater than 0");
        }
        // An empty token is almost always a missing secret; refuse it instead
        // of silently running without authentication.
        for (name, list) in
            [("auth.api_tokens", &self.auth.api_tokens), ("auth.ingest_tokens", &self.auth.ingest_tokens)]
        {
            if list.iter().any(|t| t.trim().is_empty()) {
                bail!("{name} contains an empty token (is the secret/env var set?); remove it or set a value");
            }
        }
        if q.enable_sql && self.auth.api_tokens.iter().all(|t| t.trim().is_empty()) {
            bail!("query.enable_sql requires auth.api_tokens to be set");
        }
        if !self.storage.artifacts_path.is_empty() {
            bail!("storage.artifacts_path is a mallard setting (Sentry sourcemaps); otel-ducklake does not use it");
        }
        if q.max_concurrent == 0 {
            bail!("query.max_concurrent must be at least 1");
        }
        Ok(())
    }
}

/// Upper bound for every duration setting; keeps date arithmetic in range.
const MAX_DURATION: Duration = Duration::from_secs(100 * 366 * 86_400);

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str, vars: &[(&str, &str)]) -> Result<Config> {
        let vars = vars.iter().map(|(k, v)| (k.to_string(), v.to_string()));
        mallard_common::config::load_from(Some(text), "ODL_", vars)
    }

    #[test]
    fn typed_values_parse_at_load_time() {
        let cfg = load(
            r#"
            server.log_format = "json"
            ingest = { flush_interval = "250ms", max_buffer_bytes = "1GiB", max_request_bytes = 1024 }
            retention = { traces = "0", logs = "3d" }
            duckdb.memory_limit = "2GB"
            "#,
            &[],
        )
        .unwrap();
        assert_eq!(cfg.server.log_format, LogFormat::Json);
        assert_eq!(cfg.ingest.flush_interval, Duration::from_millis(250));
        assert_eq!(cfg.ingest.max_buffer_bytes, ByteSize(1 << 30));
        assert_eq!(cfg.ingest.max_request_bytes, ByteSize(1024));
        assert_eq!(cfg.retention.traces, Duration::ZERO);
        assert_eq!(cfg.retention.logs, days(3));
        assert_eq!(cfg.duckdb.memory_limit, Some(ByteSize(2_000_000_000)));
        assert_eq!(load("duckdb.memory_limit = \"\"", &[]).unwrap().duckdb.memory_limit, None);
        // Defaults survive the round trip through TOML.
        assert_eq!(load("", &[]).unwrap().retention.metrics, days(30));
        for bad in [
            "server.log_format = \"xml\"",
            "ingest.flush_interval = \"5 parsecs\"",
            "ingest.flush_interval = 5",
            "ingest.max_buffer_bytes = \"5XB\"",
            "storage.schema = \"otel\"",
            "maintenance.catalog_wide = true",
        ] {
            assert!(load(bad, &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn env_overrides_and_interpolation() {
        let cfg = load(
            "",
            &[
                ("ODL_STORAGE__DATA_PATH", "s3://b/p/"),
                ("ODL_QUERY__MAX_CONCURRENT", "8"),
                ("ODL_AUTH__API_TOKENS", "[\"${TOK}\"]"),
                ("ODL_AUTH__INGEST_TOKENS", "a, b"),
                ("ODL_RETENTION__TRACES", "0"),
                ("TOK", "secret"),
                ("UNRELATED", "x"),
            ],
        )
        .unwrap();
        assert_eq!(cfg.storage.data_path, "s3://b/p/");
        assert_eq!(cfg.query.max_concurrent, 8);
        assert_eq!(cfg.auth.api_tokens, vec!["secret".to_string()]);
        assert_eq!(cfg.auth.ingest_tokens, vec!["a", "b"]);
        assert_eq!(cfg.retention.traces, Duration::ZERO);
    }

    #[test]
    fn sql_requires_tokens() {
        let mut cfg = Config::default();
        cfg.query.enable_sql = true;
        assert!(cfg.validate().is_err());
        cfg.auth.api_tokens = vec!["".into()];
        assert!(cfg.validate().is_err(), "empty token must not enable SQL");
        cfg.auth.api_tokens = vec!["t".into()];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn zero_intervals_and_empty_tokens_are_rejected() {
        let mut cfg = Config::default();
        cfg.ingest.flush_interval = Duration::ZERO;
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.retention.traces = humantime::parse_duration("300000y").unwrap();
        assert!(cfg.validate().is_err(), "durations are bounded");
        let mut cfg = Config::default();
        cfg.query.max_window = Duration::ZERO;
        assert!(cfg.validate().is_err(), "a zero max_window makes every query fail");
        let mut cfg = Config::default();
        cfg.ingest.flush_rows = 0;
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.auth.ingest_tokens = vec!["ok".into(), " ".into()];
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.storage.artifacts_path = "data/artifacts".into();
        assert!(cfg.validate().is_err(), "artifacts_path is mallard's");
    }
}
