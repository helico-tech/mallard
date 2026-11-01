//! Sentry-compatible error tracking backed by DuckLake.
//!
//! Embed it on a connection with the DuckLake catalog attached as `lake`
//! (see `mallard_duck::open_connection`); it creates the `sentry` schema:
//!
//! ```no_run
//! # async fn demo(conn: mallard_duck::duckdb::Connection) -> anyhow::Result<()> {
//! use sdl_server::{Config, Host, Server};
//! let host = Host {
//!     public_url: "https://errors.example.com/sentry".into(),
//!     api_tokens: vec!["secret".into()],
//!     artifacts: None,
//!     catalog_maintenance: None,
//!     maintenance_threads: 0,
//! };
//! let server = Server::with_connection(Config::default(), host, conn).await?;
//! let app = axum::Router::new().nest("/sentry", server.router());
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
//! axum::serve(listener, app).await?;
//! server.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! Or run the `sentry-ducklake` binary ([`Standalone`]). The library
//! installs no global tracing subscriber or metrics recorder; it only emits
//! `tracing` events and `metrics` counters.

mod api;
mod config;
mod db;
mod ingest;
mod sourcemaps;
mod standalone;
mod storage;

pub use config::{
    Artifacts, Config, Host, IngestConfig, MaintenanceConfig, QueryConfig, RetentionConfig, SourcemapConfig,
};
pub use standalone::{Secret, Standalone};

use anyhow::{Context, Result};
use db::Db;
use mallard_common::auth::{AuthPolicy, Tokens};
use sourcemaps::SourceMaps;
use std::future::Future;
use std::sync::Arc;
use storage::Storage;

/// DuckLake schema that holds the tables.
pub const SCHEMA: &str = "sentry";

struct AppState {
    cfg: Config,
    public_url: String,
    db: Db,
    storage: Option<Storage>,
    sourcemaps: SourceMaps,
    tokens: Tokens,
    /// Recently stored event ids per project, to drop SDK retries.
    recent_events: moka::sync::Cache<(u32, uuid::Uuid), ()>,
}

/// A running server: the database writer plus HTTP routers to mount.
#[derive(Clone)]
pub struct Server {
    state: Arc<AppState>,
}

impl Server {
    /// Standalone: open DuckDB, attach the catalog and keep the lake and
    /// sourcemaps below `args.storage_url`.
    pub async fn new(args: &Standalone) -> Result<Server> {
        let (st, duck) = args.storage()?;
        let conn = tokio::task::spawn_blocking(move || mallard_duck::open_connection(&st, &duck)).await??;
        Self::with_connection(args.config(), args.host(), conn).await
    }

    /// `conn` has the DuckLake catalog attached as `lake` (with secrets
    /// set). We create the `sentry` schema in it. Global DuckDB settings
    /// belong to the host and are left untouched (except
    /// `host.maintenance_threads`, which must be 0 on a shared connection).
    pub async fn with_connection(cfg: Config, host: Host, conn: mallard_duck::duckdb::Connection) -> Result<Server> {
        cfg.validate().context("sentry")?;
        host.validate()?;
        let c = cfg.clone();
        let maintenance =
            db::writer::Maintenance { catalog_wide: host.catalog_maintenance, threads: host.maintenance_threads };
        let artifacts = host.artifacts.clone();
        // Both create directories and open files: keep them off async threads.
        let (db, storage) = tokio::task::spawn_blocking(move || {
            let storage = artifacts.map(|a| Storage::new(&a.url, &a.options)).transpose()?;
            let catalog = conn
                .query_row(
                    "SELECT path FROM duckdb_databases() WHERE database_name = ?",
                    [mallard_duck::CATALOG],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .unwrap_or_default();
            anyhow::Ok((Db::with_connection(conn, &c, maintenance, mallard_duck::redact(&catalog))?, storage))
        })
        .await??;
        if host.api_tokens.iter().all(|t| t.trim().is_empty()) {
            tracing::warn!("no API token configured: the management API rejects all requests");
        }
        let sourcemaps = SourceMaps::new(storage.clone(), cfg.sourcemaps.cache_size);
        let recent_events = moka::sync::Cache::builder()
            .max_capacity(200_000)
            .time_to_live(std::time::Duration::from_secs(3600))
            .build();
        let tokens = Tokens::new(&host.api_tokens, AuthPolicy::Required);
        Ok(Server {
            state: Arc::new(AppState {
                cfg,
                public_url: host.public_url,
                db,
                storage,
                sourcemaps,
                tokens,
                recent_events,
            }),
        })
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
    /// Same shape as `odl_server::Server::run_background`.
    pub async fn run_background(&self, shutdown: impl Future<Output = ()>) -> Result<()> {
        shutdown.await;
        self.shutdown().await;
        Ok(())
    }

    /// Commit queued events, flush inlined data and stop the writer thread.
    pub async fn shutdown(&self) {
        self.state.db.shutdown().await;
    }
}
