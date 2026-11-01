//! Sentry-compatible error tracking backed by DuckLake.
//!
//! Use it as a binary (`sentry-ducklake serve`) or embed it:
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use sdl_server::{Config, Server};
//! let cfg = Config {
//!     storage_url: Some("s3://my-bucket/errors".into()),
//!     api_token: Some("secret".into()),
//!     ..Config::default()
//! };
//! let server = Server::new(cfg).await?;
//! let app = axum::Router::new().nest("/sentry", server.router());
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
//! axum::serve(listener, app).await?;
//! server.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! The library installs no global tracing subscriber or metrics recorder;
//! it only emits `tracing` events and `metrics` counters.

pub mod api;
pub mod config;
pub mod db;
pub mod ingest;
pub mod sourcemaps;
pub mod storage;

pub use config::Config;

use anyhow::{Context, Result};
use db::Db;
use sourcemaps::SourceMaps;
use std::future::Future;
use std::sync::Arc;
use storage::Storage;

pub struct AppState {
    pub cfg: Config,
    pub db: Db,
    pub storage: Option<Storage>,
    pub sourcemaps: SourceMaps,
    pub tokens: Vec<String>,
    /// Recently stored event ids per project, to drop SDK retries.
    pub recent_events: moka::sync::Cache<(u32, uuid::Uuid), ()>,
}

/// A running server: the database writer plus HTTP routers to mount.
#[derive(Clone)]
pub struct Server {
    state: Arc<AppState>,
}

impl Server {
    /// Standalone: open DuckDB and attach the catalog from `cfg`.
    /// Requires `cfg.storage_url`.
    pub async fn new(cfg: Config) -> Result<Server> {
        cfg.validate()?;
        let url = cfg.storage_url.clone().context("storage_url is required (e.g. s3://bucket/prefix or ./data)")?;
        let storage = Storage::new(&url, &cfg)?;
        let (c, s) = (cfg.clone(), storage.clone());
        let db = tokio::task::spawn_blocking(move || Db::open(&c, &s)).await??;
        Ok(Self::assemble(cfg, db, Some(storage)))
    }

    /// Embedded: `conn` has the DuckLake catalog attached as
    /// `cfg.lake_alias` (with secrets set). We create `cfg.schema` in it.
    /// `cfg.storage_url` is only used for sourcemap bundles (optional).
    /// Global DuckDB settings (threads, memory, temp dir, extensions,
    /// `maintenance_threads`) belong to the host and are left untouched.
    pub async fn with_connection(mut cfg: Config, conn: duckdb::Connection) -> Result<Server> {
        cfg.validate()?;
        cfg.maintenance_threads = 0;
        if cfg.duckdb_threads.is_some()
            || cfg.duckdb_memory_limit.is_some()
            || cfg.extension_dir.is_some()
            || cfg.duckdb_temp_dir.is_some()
        {
            tracing::warn!("duckdb_* settings and extension_dir are ignored with a shared connection");
        }
        let storage = cfg.storage_url.as_deref().map(|u| Storage::new(u, &cfg)).transpose()?;
        let c = cfg.clone();
        let db = tokio::task::spawn_blocking(move || Db::with_connection(conn, &c, "external".into())).await??;
        Ok(Self::assemble(cfg, db, storage))
    }

    fn assemble(cfg: Config, db: Db, storage: Option<Storage>) -> Server {
        let tokens = cfg.api_tokens();
        if tokens.is_empty() {
            tracing::warn!("no api_token configured: the management API rejects all requests");
        }
        let sourcemaps = SourceMaps::new(storage.clone(), cfg.sourcemap_cache_mb);
        let recent_events = moka::sync::Cache::builder()
            .max_capacity(200_000)
            .time_to_live(std::time::Duration::from_secs(3600))
            .build();
        Server { state: Arc::new(AppState { cfg, db, storage, sourcemaps, tokens, recent_events }) }
    }

    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// `/api/{project_id}/envelope/` and `/api/{project_id}/store/`.
    pub fn ingest_router(&self) -> axum::Router {
        ingest::router(self.state.clone())
    }

    /// `/v1/...` query/management API and `/api/0/...` sourcemap upload.
    pub fn api_router(&self) -> axum::Router {
        api::router(self.state.clone())
    }

    /// Everything: ingest + API + `/openapi.json`, `/docs`, `/healthz`, `/readyz`.
    pub fn router(&self) -> axum::Router {
        self.ingest_router().merge(self.api_router()).merge(api::meta_router(self.state.clone()))
    }

    pub fn openapi() -> utoipa::openapi::OpenApi {
        api::openapi()
    }

    /// Wait for `shutdown` to resolve, then stop cleanly (see [`Server::shutdown`]).
    pub async fn run_background(&self, shutdown: impl Future<Output = ()> + Send + 'static) {
        shutdown.await;
        self.shutdown().await;
    }

    /// Commit queued events, flush inlined data and stop the writer thread.
    pub async fn shutdown(&self) {
        self.state.db.shutdown().await;
    }
}
