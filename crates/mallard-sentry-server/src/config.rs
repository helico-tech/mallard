//! Configuration.
//!
//! - [`Config`]: the settings this library owns (retention, ingest limits,
//!   batching, maintenance, queries, sourcemaps). It is the `[sentry]`
//!   section of mallard's config file (serde, unknown keys rejected).
//! - [`Host`]: what the application around it decides: the public URL,
//!   API tokens, where sourcemap bundles go, and whether this side runs
//!   catalog-wide maintenance.

use std::time::Duration;

use mallard_common::{ByteSize, duration};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub retention: RetentionConfig,
    pub ingest: IngestConfig,
    pub maintenance: MaintenanceConfig,
    pub query: QueryConfig,
    pub sourcemaps: SourcemapConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    /// Delete events older than this (`0` = keep forever), e.g. `90d`.
    /// Whole days are dropped at once, so it is rounded up to days.
    #[serde(with = "duration")]
    pub events: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// Max size of one event after decompression.
    pub max_event_bytes: ByteSize,
    /// Max size of a request body after decompression.
    pub max_body_bytes: ByteSize,
    /// Ingest requests processed at once (bounds memory for request
    /// bodies); more wait for a slot.
    pub concurrency: usize,
    /// Events waiting to be written before ingest answers 429.
    pub queue_capacity: usize,
    /// Max events per write batch.
    pub batch_max: usize,
    /// How long the writer collects events after the first one before
    /// committing (bounds DuckLake snapshots at low load to one per
    /// window). Under load, batches of 200+ commit immediately.
    #[serde(with = "duration")]
    pub batch_window: Duration,
    /// Use the last `X-Forwarded-For` address (the one the proxy in front
    /// of this server adds) as client IP. Enable only behind exactly one
    /// proxy hop.
    pub trust_forwarded_for: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaintenanceConfig {
    /// How often small inlined batches are flushed into Parquet files.
    #[serde(with = "duration")]
    pub flush_interval: Duration,
    /// Also flush once this many events are inlined (bounds catalog size).
    pub flush_rows: usize,
    /// How often to compact files and apply retention.
    #[serde(with = "duration")]
    pub compact_interval: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryConfig {
    /// Read connections for the query API.
    pub readers: usize,
    /// Max time span a single query may cover, e.g. `366d`.
    #[serde(with = "duration")]
    pub max_window: Duration,
    /// Enable `POST /v1/sql`: read-only SQL over `events`, `issues` and
    /// `issue_activity` (ADR 0011). Treat the API tokens as admin access then.
    pub enable_sql: bool,
    /// Longest run of one SQL query; it is interrupted after this.
    #[serde(with = "duration")]
    pub sql_timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourcemapConfig {
    /// Memory for parsed sourcemaps and bundles, e.g. `256MiB`.
    pub cache_size: ByteSize,
}

const DAY: Duration = Duration::from_secs(86_400);

impl Default for RetentionConfig {
    fn default() -> Self {
        Self { events: 90 * DAY }
    }
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            max_event_bytes: ByteSize::mib(1),
            max_body_bytes: ByteSize::mib(20),
            concurrency: 256,
            queue_capacity: 20_000,
            batch_max: 2000,
            batch_window: Duration::from_millis(500),
            trust_forwarded_for: false,
        }
    }
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(300),
            // Both measured in 2 GiB at 300 events/s: flushing 50k events at once got the
            // container killed, and merging 10 min of events ran out of memory
            // (docs/otel/capacity.md).
            flush_rows: 10_000,
            compact_interval: Duration::from_secs(120),
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self { readers: 4, max_window: 366 * DAY, enable_sql: false, sql_timeout: Duration::from_secs(30) }
    }
}

impl Default for SourcemapConfig {
    fn default() -> Self {
        Self { cache_size: ByteSize::mib(256) }
    }
}

/// Upper bound for every duration setting; keeps date arithmetic in range.
const MAX_DURATION: Duration = Duration::from_secs(100 * 366 * 86_400);

impl Config {
    /// Check ranges. Units are checked while parsing.
    pub fn validate(&self) -> anyhow::Result<()> {
        let (i, m, q) = (&self.ingest, &self.maintenance, &self.query);
        for (name, d) in [
            ("sentry.retention.events", self.retention.events),
            ("sentry.ingest.batch_window", i.batch_window),
            ("sentry.maintenance.flush_interval", m.flush_interval),
            ("sentry.maintenance.compact_interval", m.compact_interval),
            ("sentry.query.max_window", q.max_window),
            ("sentry.query.sql_timeout", q.sql_timeout),
        ] {
            anyhow::ensure!(d <= MAX_DURATION, "{name} is longer than 100 years");
        }
        for (name, d) in [
            ("sentry.maintenance.flush_interval", m.flush_interval),
            ("sentry.maintenance.compact_interval", m.compact_interval),
            ("sentry.query.max_window", q.max_window),
            ("sentry.query.sql_timeout", q.sql_timeout),
        ] {
            anyhow::ensure!(!d.is_zero(), "{name} must be greater than 0");
        }
        for (name, n) in [
            ("sentry.ingest.batch_max", i.batch_max),
            ("sentry.ingest.queue_capacity", i.queue_capacity),
            ("sentry.ingest.concurrency", i.concurrency),
            ("sentry.maintenance.flush_rows", m.flush_rows),
            ("sentry.query.readers", q.readers),
        ] {
            anyhow::ensure!(n > 0, "{name} must be greater than 0");
        }
        Ok(())
    }
}

/// Settings the host application decides (not part of [`Config`]).
#[derive(Clone)]
pub struct Host {
    /// Public base URL of the routes, used to build DSNs. May contain a
    /// path prefix when mounted below one (`https://host/sentry`).
    pub public_url: String,
    /// Bearer tokens for the `/v1` API and sourcemap uploads. Without one,
    /// every API request is refused.
    pub api_tokens: Vec<String>,
    /// Where sourcemap bundles go. `None` disables sourcemap upload.
    pub artifacts: Option<Artifacts>,
    /// Also expire DuckLake snapshots and delete old files catalog-wide,
    /// keeping snapshots this long. `None` when another component of a
    /// shared catalog does it (ADR 0001).
    pub catalog_maintenance: Option<Duration>,
    /// DuckDB threads while compacting (`SET threads` is global). 0 = leave
    /// the setting alone; must be 0 on a shared connection.
    pub maintenance_threads: usize,
}

/// Object storage for sourcemap bundles.
#[derive(Clone, Debug)]
pub struct Artifacts {
    /// `s3://bucket/prefix`, `gs://...`, `az://container/prefix` or a local directory.
    pub url: String,
    /// Endpoint, region and credentials; unset values come from the
    /// environment (`AWS_*`, `GCS_HMAC_*`, `AZURE_*`).
    pub options: mallard_duck::ObjectStoreOptions,
}

impl Artifacts {
    /// Build the store, as start-up does: an unknown scheme or missing
    /// credentials fail here. It does not contact the store.
    pub fn check(&self) -> anyhow::Result<()> {
        crate::storage::Storage::new(&self.url, &self.options).map(|_| ())
    }
}

impl Host {
    pub fn validate(&self) -> anyhow::Result<()> {
        let public_url = url::Url::parse(&self.public_url).ok();
        anyhow::ensure!(
            public_url.is_some_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host()),
            "public_url must be an http(s) URL with a host, got {:?}",
            self.public_url
        );
        Ok(())
    }
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("public_url", &self.public_url)
            .field("api_tokens", &self.api_tokens.len())
            .field("artifacts", &self.artifacts)
            .field("catalog_maintenance", &self.catalog_maintenance)
            .field("maintenance_threads", &self.maintenance_threads)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> anyhow::Result<Config> {
        mallard_common::config::load_from(Some(text), "TEST_", std::iter::empty())
    }

    #[test]
    fn sections_with_units() {
        let c = load(
            r#"
            retention.events = "30d"
            ingest = { batch_window = "250ms", max_event_bytes = "2MiB", trust_forwarded_for = true }
            sourcemaps.cache_size = "64MB"
            "#,
        )
        .unwrap();
        assert_eq!(c.retention.events, 30 * DAY);
        assert_eq!(c.ingest.batch_window, Duration::from_millis(250));
        assert_eq!(c.ingest.max_event_bytes, ByteSize(2 << 20));
        assert!(c.ingest.trust_forwarded_for);
        assert_eq!(c.sourcemaps.cache_size, ByteSize::mb(64));
        assert_eq!(c.ingest.max_body_bytes, ByteSize::mib(20), "defaults stay");
        assert_eq!(load("retention.events = \"0\"").unwrap().retention.events, Duration::ZERO);
        let q = load("query = { enable_sql = true, sql_timeout = \"5s\" }").unwrap().query;
        assert!(q.enable_sql && q.sql_timeout == Duration::from_secs(5));
        assert!(!c.query.enable_sql, "SQL is off by default");
        c.validate().unwrap();
        for bad in ["retention_days = 90", "storage_url = \"x\"", "ingest.typo = 1", "public_url = \"http://x\""] {
            assert!(load(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_ranges() {
        Config::default().validate().unwrap();
        let mut c = Config::default();
        c.ingest.batch_max = 0;
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.query.max_window = Duration::ZERO;
        assert!(c.validate().is_err());
    }

    #[test]
    fn host_needs_an_http_url() {
        let host = |u: &str| Host {
            public_url: u.into(),
            api_tokens: vec!["hunter2".into()],
            artifacts: None,
            catalog_maintenance: None,
            maintenance_threads: 0,
        };
        host("https://x.example/sentry").validate().unwrap();
        for bad in ["data:x", "localhost:8000", "/sentry"] {
            assert!(host(bad).validate().is_err(), "{bad}");
        }
        assert!(!format!("{:?}", host("http://x")).contains("hunter2"));
    }
}
