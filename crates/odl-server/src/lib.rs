//! otel-ducklake: an OpenTelemetry (OTLP) collector that stores traces, logs
//! and metrics in DuckLake and serves them through an OpenAPI query API.
//!
//! Use as a binary (`otel-ducklake serve`) or embed it:
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use odl_server::{Server, config::Config};
//! let server = Server::new(Config::default()).await?;
//! let api = server.api_router();          // mount anywhere, e.g. .nest("/otel", api)
//! let otlp_http = server.otlp_http_router(); // /v1/traces, /v1/logs, /v1/metrics
//! let grpc = server.grpc_routes();        // tonic routes for OTLP/gRPC
//! server.run_background(async { /* wait for shutdown */ }).await?;
//! # Ok(()) }
//! ```
//!
//! The library installs no global state (no tracing subscriber, no metrics
//! recorder); that is left to the binary or the embedding application.

pub mod api;
pub mod auth;
pub mod config;
pub mod otlp;
pub mod query;
pub mod stats;
pub mod store;

use std::{future::Future, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use axum::Router;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::{compression::CompressionLayer, trace::TraceLayer};
use tracing::info;

use crate::{
    api::ApiState,
    auth::Tokens,
    config::Config,
    otlp::Ingest,
    query::Reader,
    stats::Stats,
    store::{
        Lake,
        maintenance::{Maintenance, Settings},
        writer::{Flusher, WriteBuffer},
    },
};

/// A configured collector: storage attached, ready to serve.
pub struct Server {
    cfg: Arc<Config>,
    lake: Arc<Lake>,
    stats: Arc<Stats>,
    buffer: Arc<WriteBuffer>,
    ingest: Arc<Ingest>,
    api_state: Arc<ApiState>,
}

impl Server {
    /// Open DuckDB, attach the DuckLake catalog and create tables.
    pub async fn new(cfg: Config) -> Result<Self> {
        cfg.validate()?;
        let c = cfg.clone();
        let lake = tokio::task::spawn_blocking(move || Lake::open(&c)).await??;
        Self::build(cfg, lake)
    }

    /// Use a DuckDB connection owned by the embedding application. The
    /// DuckLake catalog must already be attached as `lake`. Only `try_clone()`
    /// of this connection is used, and no global DuckDB settings are changed.
    pub async fn with_connection(cfg: Config, conn: duckdb::Connection) -> Result<Self> {
        cfg.validate()?;
        let c = cfg.clone();
        let lake = tokio::task::spawn_blocking(move || Lake::with_connection(conn, &c)).await??;
        Self::build(cfg, lake)
    }

    fn build(cfg: Config, lake: Lake) -> Result<Self> {
        let stats = Arc::new(Stats::default());
        let buffer = Arc::new(WriteBuffer::new(
            cfg.max_buffer_bytes(),
            cfg.ingest.flush_rows,
            stats.clone(),
        ));
        let ingest = Arc::new(Ingest {
            buffer: buffer.clone(),
            stats: stats.clone(),
            tokens: Tokens::new(&cfg.auth.ingest_tokens),
            max_request_bytes: cfg.max_request_bytes(),
        });
        let conns = (0..cfg.query.max_concurrent)
            .map(|_| lake.connect())
            .collect::<Result<Vec<_>>>()?;
        let reader = Arc::new(Reader::new(conns, cfg.query_timeout(), cfg.max_window()));
        let api_state = Arc::new(ApiState {
            reader,
            stats: stats.clone(),
            tokens: Tokens::new(&cfg.auth.api_tokens),
            schema: lake.schema.clone(),
            catalog_type: lake.catalog_kind.as_str().to_string(),
            data_path: lake.data_path.clone(),
            max_buffer_bytes: cfg.max_buffer_bytes(),
            enable_sql: cfg.query.enable_sql,
            started: Instant::now(),
        });
        Ok(Self {
            cfg: Arc::new(cfg),
            lake: Arc::new(lake),
            stats,
            buffer,
            ingest,
            api_state,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// OTLP/HTTP routes: `POST /v1/traces`, `/v1/logs`, `/v1/metrics`.
    pub fn otlp_http_router(&self) -> Router {
        otlp::http::router(self.ingest.clone())
    }

    /// OTLP/gRPC services (trace, logs, metrics).
    pub fn grpc_routes(&self) -> tonic::service::Routes {
        otlp::grpc::routes(self.ingest.clone())
    }

    /// Query API: `/api/v1/...`, `/openapi.json`, `/docs`. Paths are relative,
    /// so the router can be nested under a prefix.
    pub fn api_router(&self) -> Router {
        api::router(self.api_state.clone()).0
    }

    pub fn openapi(&self) -> utoipa::openapi::OpenApi {
        api::openapi()
    }

    /// Run the flusher and (if enabled) maintenance until `shutdown`
    /// completes, then flush remaining buffered data. Stop the receivers
    /// before the shutdown future resolves so nothing arrives afterwards.
    pub async fn run_background(&self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let flusher = Flusher::start(
            &self.lake,
            self.buffer.clone(),
            self.cfg.flush_interval(),
            self.stats.clone(),
        )?;
        let maintenance = if self.cfg.maintenance.enabled {
            Some(Maintenance::start(
                &self.lake,
                Settings::from_config(&self.cfg)?,
                self.stats.clone(),
            )?)
        } else {
            None
        };
        shutdown.await;
        info!("shutting down: flushing buffered data");
        tokio::task::spawn_blocking(move || {
            if let Some(m) = maintenance {
                m.stop();
            }
            flusher.stop();
        })
        .await?;
        Ok(())
    }

    /// Bind the configured addresses and serve OTLP/gRPC, OTLP/HTTP and the
    /// query API until `shutdown` is cancelled.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let s = &self.cfg.server;
        let http_l = TcpListener::bind(&s.otlp_http_addr)
            .await
            .with_context(|| format!("binding {}", s.otlp_http_addr))?;
        let api_l = TcpListener::bind(&s.api_addr)
            .await
            .with_context(|| format!("binding {}", s.api_addr))?;
        let grpc_addr = s
            .otlp_grpc_addr
            .parse()
            .with_context(|| format!("parsing {}", s.otlp_grpc_addr))?;
        info!(grpc = %s.otlp_grpc_addr, http = %s.otlp_http_addr, api = %s.api_addr, "listening");

        let receivers_down = CancellationToken::new();
        let otlp_http = self.otlp_http_router().layer(TraceLayer::new_for_http());
        let api = self
            .api_router()
            .layer(CompressionLayer::new())
            .layer(TraceLayer::new_for_http());
        let grpc = tonic::transport::Server::builder().add_routes(self.grpc_routes());

        let t1 = {
            let sd = shutdown.clone();
            tokio::spawn(async move {
                axum::serve(http_l, otlp_http)
                    .with_graceful_shutdown(async move { sd.cancelled().await })
                    .await
            })
        };
        let t2 = {
            let sd = shutdown.clone();
            tokio::spawn(async move {
                axum::serve(api_l, api)
                    .with_graceful_shutdown(async move { sd.cancelled().await })
                    .await
            })
        };
        let t3 = {
            let sd = shutdown.clone();
            tokio::spawn(async move {
                grpc.serve_with_shutdown(grpc_addr, async move { sd.cancelled().await })
                    .await
            })
        };
        let rd = receivers_down.clone();
        let background = self.run_background(async move { rd.cancelled().await });
        let receivers = async {
            let (a, b, c) = tokio::join!(t1, t2, t3);
            receivers_down.cancel();
            a??;
            b??;
            c??;
            anyhow::Ok(())
        };
        let (r, b) = tokio::join!(receivers, background);
        r?;
        b?;
        Ok(())
    }
}
