//! Server configuration.
//!
//! Three ways to build it:
//! - the `sentry-ducklake` binary: flags or `SDL_*` environment variables;
//! - embedded, in code: `Config { public_url: ..., ..Config::default() }`
//!   or deserialize it from a config file section (serde);
//! - embedded, from env with your own prefix: [`Config::from_env_prefix`].

use clap::Args;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Args, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Address to listen on (binary only).
    #[arg(long, env = "SDL_LISTEN", default_value = "0.0.0.0:8000")]
    pub listen: SocketAddr,

    /// Public base URL of this server, used to build DSNs. May contain a
    /// path prefix when mounted below one (`https://host/sentry`).
    #[arg(long, env = "SDL_PUBLIC_URL", default_value = "http://localhost:8000")]
    pub public_url: String,

    /// DuckLake catalog: a path (DuckDB file, one process only),
    /// `sqlite:<path>`, `postgres:<libpq string>` or `mysql:<string>`.
    /// Ignored when the server is given an already attached connection.
    #[arg(long, env = "SDL_CATALOG", default_value = "./.data/catalog.ducklake")]
    pub catalog: String,

    /// Alias the DuckLake catalog is attached as.
    #[arg(long, env = "SDL_LAKE_ALIAS", default_value = "lake")]
    pub lake_alias: String,

    /// Schema inside the lake that holds our tables.
    #[arg(long, env = "SDL_SCHEMA", default_value = "sentry")]
    pub schema: String,

    /// Where data files and sourcemap bundles live:
    /// `s3://bucket/prefix` (also R2, MinIO, Garage, ...), `gs://bucket/prefix`,
    /// `az://container/prefix`, or a local path / `file:///path`.
    /// With an attached connection only sourcemap bundles go here; if unset
    /// there, sourcemap uploads are disabled.
    #[arg(long, env = "SDL_STORAGE_URL")]
    pub storage_url: Option<String>,

    /// Custom S3 endpoint including scheme, e.g. `http://minio:9000`.
    #[arg(long, env = "SDL_S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,

    /// S3 region.
    #[arg(long, env = "SDL_S3_REGION", default_value = "us-east-1")]
    pub s3_region: String,

    /// Use path-style S3 URLs (needed by most self-hosted S3 servers).
    #[arg(long, env = "SDL_S3_PATH_STYLE", default_value_t = false)]
    pub s3_path_style: bool,

    /// Bearer token(s) for the management API and sourcemap uploads, comma
    /// separated. The binary requires one; without one the API rejects all
    /// requests.
    #[arg(long, env = "SDL_API_TOKEN", hide_env_values = true)]
    pub api_token: Option<String>,

    /// Delete events older than this many days (0 = keep forever).
    #[arg(long, env = "SDL_RETENTION_DAYS", default_value_t = 90)]
    pub retention_days: u32,

    /// Run catalog-wide maintenance (snapshot expiry, old file cleanup).
    /// Turn off on all but one service sharing a catalog.
    #[arg(long, env = "SDL_CATALOG_WIDE_MAINTENANCE", default_value_t = true, action = clap::ArgAction::Set)]
    pub catalog_wide_maintenance: bool,

    /// Max events per write batch.
    #[arg(long, env = "SDL_BATCH_MAX", default_value_t = 2000)]
    pub batch_max: usize,

    /// Minimum time between two commits (group commit). Bounds the number of
    /// DuckLake snapshots at low load; under load commits run back to back.
    #[arg(long, env = "SDL_BATCH_WINDOW", value_parser = humantime::parse_duration, default_value = "100ms")]
    #[serde(with = "humantime_serde")]
    pub batch_window: Duration,

    /// Events waiting to be written before ingest answers 429.
    #[arg(long, env = "SDL_QUEUE_CAPACITY", default_value_t = 20000)]
    pub queue_capacity: usize,

    /// How often small inlined batches are flushed into Parquet files.
    #[arg(long, env = "SDL_FLUSH_INTERVAL", value_parser = humantime::parse_duration, default_value = "5m")]
    #[serde(with = "humantime_serde")]
    pub flush_interval: Duration,

    /// Also flush once this many events are inlined (bounds catalog size).
    #[arg(long, env = "SDL_FLUSH_ROWS", default_value_t = 50_000)]
    pub flush_rows: usize,

    /// How often to compact files, apply retention and expire snapshots.
    #[arg(long, env = "SDL_COMPACT_INTERVAL", value_parser = humantime::parse_duration, default_value = "1h")]
    #[serde(with = "humantime_serde")]
    pub compact_interval: Duration,

    /// Keep DuckLake snapshots (and the files they reference) this long.
    /// Must exceed the age of your oldest catalog backup you may restore.
    #[arg(long, env = "SDL_SNAPSHOT_RETENTION", value_parser = humantime::parse_duration, default_value = "1h")]
    #[serde(with = "humantime_serde")]
    pub snapshot_retention: Duration,

    /// Max size of one event after decompression.
    #[arg(long, env = "SDL_MAX_EVENT_BYTES", default_value_t = 1024 * 1024)]
    pub max_event_bytes: usize,

    /// Max size of a request body after decompression.
    #[arg(long, env = "SDL_MAX_BODY_BYTES", default_value_t = 50 * 1024 * 1024)]
    pub max_body_bytes: usize,

    /// Read connections for the query API.
    #[arg(long, env = "SDL_READERS", default_value_t = 4)]
    pub readers: usize,

    /// DuckDB worker threads (default: number of CPUs).
    #[arg(long, env = "SDL_DUCKDB_THREADS")]
    pub duckdb_threads: Option<usize>,

    /// DuckDB memory limit, e.g. `2GB` (default: DuckDB's 80% of RAM).
    #[arg(long, env = "SDL_DUCKDB_MEMORY_LIMIT")]
    pub duckdb_memory_limit: Option<String>,

    /// DuckDB spill directory for large queries (default: `.tmp` in the
    /// working directory).
    #[arg(long, env = "SDL_DUCKDB_TEMP_DIR")]
    pub duckdb_temp_dir: Option<PathBuf>,

    /// Directory with DuckDB extensions (the Docker image pre-installs them).
    #[arg(long, env = "SDL_EXTENSION_DIR")]
    pub extension_dir: Option<PathBuf>,

    /// Use the first `X-Forwarded-For` address as client IP.
    #[arg(long, env = "SDL_TRUST_FORWARDED_FOR", default_value_t = false)]
    pub trust_forwarded_for: bool,

    /// Memory budget for cached sourcemaps, in MiB.
    #[arg(long, env = "SDL_SOURCEMAP_CACHE_MB", default_value_t = 256)]
    pub sourcemap_cache_mb: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "0.0.0.0:8000".parse().expect("valid addr"),
            public_url: "http://localhost:8000".into(),
            catalog: "./.data/catalog.ducklake".into(),
            lake_alias: "lake".into(),
            schema: "sentry".into(),
            storage_url: None,
            s3_endpoint: None,
            s3_region: "us-east-1".into(),
            s3_path_style: false,
            api_token: None,
            retention_days: 90,
            catalog_wide_maintenance: true,
            batch_max: 2000,
            batch_window: Duration::from_millis(100),
            queue_capacity: 20000,
            flush_interval: Duration::from_secs(300),
            flush_rows: 50_000,
            compact_interval: Duration::from_secs(3600),
            snapshot_retention: Duration::from_secs(3600),
            max_event_bytes: 1024 * 1024,
            max_body_bytes: 50 * 1024 * 1024,
            readers: 4,
            duckdb_threads: None,
            duckdb_memory_limit: None,
            duckdb_temp_dir: None,
            extension_dir: None,
            trust_forwarded_for: false,
            sourcemap_cache_mb: 256,
        }
    }
}

impl Config {
    /// Read `<PREFIX><FIELD>` environment variables, e.g. with prefix
    /// `OBS_SENTRY_`: `OBS_SENTRY_PUBLIC_URL`, `OBS_SENTRY_BATCH_WINDOW=250ms`.
    /// Unset fields keep their defaults.
    pub fn from_env_prefix(prefix: &str) -> anyhow::Result<Config> {
        Ok(envy::prefixed(prefix).from_env::<Config>()?)
    }

    pub fn api_tokens(&self) -> Vec<String> {
        self.api_token
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Reject values that would break SQL identifiers or the server.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (name, v) in [("schema", &self.schema), ("lake_alias", &self.lake_alias)] {
            let ok = !v.is_empty()
                && v.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && v.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            anyhow::ensure!(ok, "{name} must match [a-z_][a-z0-9_]*, got {v:?}");
        }
        anyhow::ensure!(url::Url::parse(&self.public_url).is_ok(), "public_url must be an absolute URL");
        anyhow::ensure!(self.batch_max > 0 && self.queue_capacity > 0, "batch_max and queue_capacity must be > 0");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        c: Config,
    }

    #[test]
    fn clap_defaults_match_default_impl() {
        // Assumes no SDL_* variables are set in the test environment.
        let a = T::parse_from(["t"]).c;
        let b = Config::default();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn serde_partial_with_durations() {
        let c: Config = serde_json::from_str(r#"{"schema":"errors","batch_window":"250ms","api_token":"t"}"#).unwrap();
        assert_eq!(c.schema, "errors");
        assert_eq!(c.batch_window, Duration::from_millis(250));
        assert_eq!(c.retention_days, 90);
        assert_eq!(c.api_tokens(), vec!["t"]);
    }

    #[test]
    fn validates_identifiers() {
        let c = Config { schema: "bad;drop".into(), ..Config::default() };
        assert!(c.validate().is_err());
        assert!(Config::default().validate().is_ok());
    }
}
