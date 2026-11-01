//! A standalone set-up from flags or `SDL_*` environment variables, used by
//! the tests (the `sentry-ducklake` binary is gone). One storage URL holds
//! everything:
//!
//! ```text
//! <storage_url>/lake/        DuckLake Parquet data files
//! <storage_url>/artifacts/   sourcemap chunks and bundles
//! ```
//!
//! Library settings left unset keep the defaults of [`Config`].

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use clap::Args;
use mallard_common::ByteSize;
use mallard_duck::{DuckDbConfig, ObjectStoreOptions, StorageConfig};

use crate::config::{Artifacts, Config, Host};
use crate::storage;

#[derive(Debug, Clone, Args)]
pub struct Standalone {
    /// Address to listen on.
    #[arg(long, env = "SDL_LISTEN", default_value = "0.0.0.0:8000")]
    pub listen: std::net::SocketAddr,

    /// Public base URL of this server, used to build DSNs.
    #[arg(long, env = "SDL_PUBLIC_URL", default_value = "http://localhost:8000")]
    pub public_url: String,

    /// DuckLake catalog: a path (DuckDB file, one process only),
    /// `sqlite:<path>`, `postgres:<libpq string>` or `mysql:<string>`.
    #[arg(long, env = "SDL_CATALOG", default_value = "./.data/catalog.ducklake")]
    pub catalog: String,

    /// Where data files and sourcemap bundles live:
    /// `s3://bucket/prefix` (also R2, MinIO, Garage, ...), `gs://bucket/prefix`,
    /// `az://container/prefix`, or a local path / `file:///path`.
    /// Required.
    #[arg(long, env = "SDL_STORAGE_URL")]
    pub storage_url: Option<String>,

    /// Custom S3 endpoint including scheme, e.g. `http://minio:9000`.
    #[arg(long, env = "SDL_S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,

    /// S3 region.
    #[arg(long, env = "SDL_S3_REGION", default_value = "us-east-1")]
    pub s3_region: String,

    /// Use path-style S3 URLs (needed by most self-hosted S3 servers).
    #[arg(long, env = "SDL_S3_PATH_STYLE")]
    pub s3_path_style: bool,

    /// Bearer token(s) for the management API and sourcemap uploads, comma
    /// separated. Required.
    #[arg(long, env = "SDL_API_TOKEN", hide_env_values = true)]
    pub api_token: Option<Secret>,

    /// Delete events older than this many days (0 = keep forever; default 90).
    #[arg(long, env = "SDL_RETENTION_DAYS")]
    pub retention_days: Option<u32>,

    /// Run catalog-wide maintenance (snapshot expiry, old file cleanup).
    /// Turn off on all but one service sharing a catalog.
    #[arg(long, env = "SDL_CATALOG_WIDE_MAINTENANCE", default_value_t = true, action = clap::ArgAction::Set)]
    pub catalog_wide_maintenance: bool,

    /// Keep DuckLake snapshots (and the files they reference) this long.
    #[arg(long, env = "SDL_SNAPSHOT_RETENTION", value_parser = mallard_common::duration::parse, default_value = "1h")]
    pub snapshot_retention: Duration,

    /// DuckDB threads used while compacting (0 = leave the thread count alone).
    #[arg(long, env = "SDL_MAINTENANCE_THREADS", default_value_t = 1)]
    pub maintenance_threads: usize,

    /// Max events per write batch.
    #[arg(long, env = "SDL_BATCH_MAX")]
    pub batch_max: Option<usize>,

    /// How long the writer collects events before committing, e.g. `500ms`.
    #[arg(long, env = "SDL_BATCH_WINDOW", value_parser = mallard_common::duration::parse)]
    pub batch_window: Option<Duration>,

    /// Events waiting to be written before ingest answers 429.
    #[arg(long, env = "SDL_QUEUE_CAPACITY")]
    pub queue_capacity: Option<usize>,

    /// How often small inlined batches are flushed into Parquet files.
    #[arg(long, env = "SDL_FLUSH_INTERVAL", value_parser = mallard_common::duration::parse)]
    pub flush_interval: Option<Duration>,

    /// Also flush once this many events are inlined.
    #[arg(long, env = "SDL_FLUSH_ROWS")]
    pub flush_rows: Option<usize>,

    /// How often to compact files, apply retention and expire snapshots.
    #[arg(long, env = "SDL_COMPACT_INTERVAL", value_parser = mallard_common::duration::parse)]
    pub compact_interval: Option<Duration>,

    /// Max size of one event after decompression, e.g. `1MiB`.
    #[arg(long, env = "SDL_MAX_EVENT_BYTES")]
    pub max_event_bytes: Option<ByteSize>,

    /// Max size of a request body after decompression, e.g. `20MiB`.
    #[arg(long, env = "SDL_MAX_BODY_BYTES")]
    pub max_body_bytes: Option<ByteSize>,

    /// Ingest requests processed at once.
    #[arg(long, env = "SDL_INGEST_CONCURRENCY")]
    pub ingest_concurrency: Option<usize>,

    /// Read connections for the query API.
    #[arg(long, env = "SDL_READERS")]
    pub readers: Option<usize>,

    /// DuckDB worker threads (default: number of CPUs).
    #[arg(long, env = "SDL_DUCKDB_THREADS")]
    pub duckdb_threads: Option<usize>,

    /// DuckDB memory limit, e.g. `2GB` (default: DuckDB's 80% of RAM).
    #[arg(long, env = "SDL_DUCKDB_MEMORY_LIMIT")]
    pub duckdb_memory_limit: Option<ByteSize>,

    /// DuckDB spill directory for large queries.
    #[arg(long, env = "SDL_DUCKDB_TEMP_DIR")]
    pub duckdb_temp_dir: Option<PathBuf>,

    /// Directory with DuckDB extensions (container images pre-install them).
    #[arg(long, env = "SDL_EXTENSION_DIR")]
    pub extension_dir: Option<PathBuf>,

    /// Use the last `X-Forwarded-For` address as client IP. Enable only
    /// behind exactly one proxy hop.
    #[arg(long, env = "SDL_TRUST_FORWARDED_FOR")]
    pub trust_forwarded_for: bool,

    /// Enable `POST /v1/sql`: read-only SQL over the error tables (admin access).
    #[arg(long, env = "SDL_ENABLE_SQL")]
    pub enable_sql: bool,

    /// Memory budget for cached sourcemaps, in MiB.
    #[arg(long, env = "SDL_SOURCEMAP_CACHE_MB")]
    pub sourcemap_cache_mb: Option<u64>,
}

/// A string that `Debug` does not print, so `?args` cannot leak a token.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("\"***\"")
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Secret(s.to_string())
    }
}

impl std::str::FromStr for Secret {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.into())
    }
}

impl Standalone {
    /// Defaults plus the required values (tests, examples).
    pub fn new(storage_url: &str, catalog: &str, api_token: &str) -> Self {
        use clap::Parser;
        #[derive(Parser)]
        struct P {
            #[command(flatten)]
            s: Standalone,
        }
        let mut s = P::parse_from(["sentry-ducklake"]).s;
        s.storage_url = Some(storage_url.into());
        s.api_token = Some(api_token.into());
        s.catalog = catalog.into();
        s
    }

    /// The library settings: defaults, then the flags that are set.
    pub fn config(&self) -> Config {
        let mut c = Config::default();
        if let Some(d) = self.retention_days {
            c.retention.events = Duration::from_secs(u64::from(d) * 86_400);
        }
        let i = &mut c.ingest;
        set(&mut i.batch_max, self.batch_max);
        set(&mut i.batch_window, self.batch_window);
        set(&mut i.queue_capacity, self.queue_capacity);
        set(&mut i.max_event_bytes, self.max_event_bytes);
        set(&mut i.max_body_bytes, self.max_body_bytes);
        set(&mut i.concurrency, self.ingest_concurrency);
        i.trust_forwarded_for = self.trust_forwarded_for;
        set(&mut c.maintenance.flush_interval, self.flush_interval);
        set(&mut c.maintenance.flush_rows, self.flush_rows);
        set(&mut c.maintenance.compact_interval, self.compact_interval);
        set(&mut c.query.readers, self.readers);
        c.query.enable_sql = self.enable_sql;
        set(&mut c.sourcemaps.cache_size, self.sourcemap_cache_mb.map(ByteSize::mib));
        c
    }

    /// The host settings: tokens, public URL, `<storage_url>/artifacts`.
    pub fn host(&self) -> Host {
        let tokens = self.api_token.as_ref().map_or("", Secret::expose);
        Host {
            public_url: self.public_url.clone(),
            api_tokens: tokens.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            artifacts: self.storage_url.clone().map(|url| Artifacts { url, options: self.object_store_options() }),
            catalog_maintenance: self.catalog_wide_maintenance.then_some(self.snapshot_retention),
            maintenance_threads: self.maintenance_threads,
        }
    }

    fn object_store_options(&self) -> ObjectStoreOptions {
        ObjectStoreOptions {
            endpoint: self.s3_endpoint.clone(),
            region: Some(self.s3_region.clone()),
            path_style: self.s3_path_style,
            ..Default::default()
        }
    }

    /// DuckDB and DuckLake settings for `mallard_duck::open_connection`.
    pub fn storage(&self) -> Result<(StorageConfig, DuckDbConfig)> {
        let url = self.storage_url.as_deref().context("SDL_STORAGE_URL (--storage-url) is required")?;
        let lake = storage::lake(url, &self.object_store_options())?;
        let st = StorageConfig {
            catalog: self.catalog.clone(),
            data_path: lake.data_path,
            secrets: lake.secret.into_iter().collect(),
            ..Default::default()
        };
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let db = DuckDbConfig {
            memory_limit: self.duckdb_memory_limit,
            threads: self.duckdb_threads.unwrap_or(0),
            temp_directory: path(&self.duckdb_temp_dir),
            extension_directory: path(&self.extension_dir),
            ..Default::default()
        };
        Ok((st, db))
    }
}

fn set<T>(field: &mut T, v: Option<T>) {
    if let Some(v) = v {
        *field = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_flags_keep_library_defaults() {
        let s = Standalone::new("./data", "./c.ducklake", "a, b");
        let c = s.config();
        let d = Config::default();
        assert_eq!(format!("{c:?}"), format!("{d:?}"));
        let h = s.host();
        assert_eq!(h.api_tokens, ["a", "b"]);
        assert_eq!(h.catalog_maintenance, Some(Duration::from_secs(3600)));
        assert!(!format!("{s:?}").contains("a, b"), "the token is not printed");
    }

    #[test]
    fn flags_map_to_sections() {
        let mut s = Standalone::new("./data", "./c.ducklake", "t");
        s.retention_days = Some(30);
        s.batch_window = Some(Duration::from_millis(20));
        s.sourcemap_cache_mb = Some(64);
        s.catalog_wide_maintenance = false;
        let c = s.config();
        assert_eq!(c.retention.events, Duration::from_secs(30 * 86_400));
        assert_eq!(c.ingest.batch_window, Duration::from_millis(20));
        assert_eq!(c.sourcemaps.cache_size, ByteSize::mib(64));
        assert_eq!(s.host().catalog_maintenance, None);
    }
}
