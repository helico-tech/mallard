//! The telemetry side of mallard: an OpenTelemetry (OTLP) collector that stores traces, logs
//! and metrics in DuckLake and serves them through an OpenAPI query API.
//!
//! `mallard-server` embeds it; on its own (tests) it opens its own lake:
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use mallard_otel_server::{Server, config::Config};
//! let server = Server::new(Config::default()).await?;
//! let api = server.api_router();          // mount anywhere, e.g. .nest("/otel", api)
//! let otlp_http = server.otlp_http_router(); // /v1/traces, /v1/logs, /v1/metrics
//! let grpc = server.grpc_routes();        // tonic routes for OTLP/gRPC
//! server.run_background(async { /* wait for shutdown */ }).await?;
//! # Ok(()) }
//! ```
//!
//! The library installs no global state (no tracing subscriber, no metrics
//! recorder); that is left to the embedding application.

mod api;
pub mod config;
mod otlp;
mod query;
mod stats;
mod store;

pub use otlp::{IngestError, IngestHook};
pub use query::{Counted, attributes::MAX_SERVICE_NAMES};
pub use store::SCHEMA;

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use axum::Router;
use tracing::info;

use mallard_common::{
    auth::{AuthPolicy, Tokens},
    http::ApiErr,
    window::Window,
};
use mallard_otel_api::Span;

use crate::{
    api::ApiState,
    config::Config,
    otlp::{Ingest, Request},
    query::Reader,
    stats::Stats,
    store::{
        Lake,
        maintenance::Maintenance,
        writer::{Flusher, WriteBuffer},
    },
};

/// The OpenAPI document of the query API, without building a server.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api::openapi()
}

/// Open the lake described by `cfg` and run maintenance once: flush inlined
/// rows and merge small files (all files below the target size with
/// `full`), then retention. Blocking; returns one line per step. With a
/// DuckDB-file or SQLite catalog, stop the server first.
pub fn run_maintenance(cfg: &Config, full: bool) -> Result<Vec<String>> {
    cfg.validate()?;
    let lake = Lake::open(cfg)?;
    let conn = lake.connect()?;
    store::maintenance::run_once(&conn, &lake.schema, &lake.locks, &cfg.maintenance, &cfg.retention, full)
}

/// Test hook (not a stable API): OTLP traces as Arrow batches for the `spans` table.
#[doc(hidden)]
pub fn __convert_traces(
    req: &opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest,
) -> Vec<arrow::record_batch::RecordBatch> {
    otlp::convert::convert_traces(req).batches.into_iter().map(|(_, b)| b).collect()
}

fn catalog_type(k: mallard_duck::CatalogKind) -> mallard_otel_api::CatalogType {
    use mallard_duck::CatalogKind as K;
    use mallard_otel_api::CatalogType as T;
    match k {
        K::DuckDb => T::DuckDb,
        K::Sqlite => T::Sqlite,
        K::Postgres => T::Postgres,
        K::MySql => T::MySql,
    }
}

/// A configured collector: storage attached, ready to serve.
pub struct Server {
    cfg: Config,
    lake: Lake,
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
    pub async fn with_connection(cfg: Config, conn: mallard_duck::duckdb::Connection) -> Result<Self> {
        cfg.validate()?;
        let c = cfg.clone();
        let lake = tokio::task::spawn_blocking(move || Lake::with_connection(conn, &c)).await??;
        Self::build(cfg, lake)
    }

    fn build(cfg: Config, lake: Lake) -> Result<Self> {
        let stats = Arc::new(Stats::default());
        let max_buffer_bytes = cfg.ingest.max_buffer_bytes.0;
        let buffer = Arc::new(WriteBuffer::new(max_buffer_bytes, cfg.ingest.flush_rows, stats.clone()));
        let ingest = Arc::new(Ingest::new(
            buffer.clone(),
            stats.clone(),
            Tokens::new(&cfg.auth.ingest_tokens, AuthPolicy::OpenIfEmpty),
            usize::try_from(cfg.ingest.max_request_bytes.0).unwrap_or(usize::MAX),
            max_buffer_bytes,
        ));
        let conns = (0..cfg.query.max_concurrent).map(|_| lake.connect()).collect::<Result<Vec<_>>>()?;
        let reader = Arc::new(Reader::new(conns, cfg.query.timeout, cfg.query.max_window));
        let api_state = Arc::new(ApiState {
            reader,
            stats: stats.clone(),
            // An empty list leaves the API open;
            // mallard always configures tokens.
            tokens: Tokens::new(&cfg.auth.api_tokens, AuthPolicy::OpenIfEmpty),
            schema: lake.schema.clone(),
            catalog_type: catalog_type(lake.catalog_kind),
            data_path: lake.data_path.clone(),
            max_buffer_bytes,
            enable_sql: cfg.query.enable_sql,
            capabilities: mallard_otel_api::Capabilities {
                sql: cfg.query.enable_sql,
                max_window_secs: cfg.query.max_window.as_secs(),
                retention_secs: mallard_otel_api::RetentionSecs {
                    traces: cfg.retention.traces.as_secs(),
                    logs: cfg.retention.logs.as_secs(),
                    metrics: cfg.retention.metrics.as_secs(),
                },
                flush_interval_secs: cfg.ingest.flush_interval.as_secs_f64(),
                ingest_auth: !cfg.auth.ingest_tokens.is_empty(),
            },
            started: Instant::now(),
        });
        Ok(Self { cfg, lake, stats, buffer, ingest, api_state })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Show every OTLP traces and logs request to `hook` once it is
    /// buffered (both receivers). Set it once, before serving; a second
    /// call is an error.
    pub fn set_ingest_hook(&self, hook: Arc<dyn IngestHook>) -> Result<()> {
        self.ingest.hook.set(hook).map_err(|_| anyhow::anyhow!("ingest hook already set"))
    }

    /// Buffer spans built in-process, like an OTLP export without the
    /// network: same conversion, limits, back-pressure and stats. The ingest
    /// hook does not see them. Blocking (CPU work); returns the number of
    /// spans accepted.
    pub fn ingest_traces(
        &self,
        req: opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest,
    ) -> std::result::Result<u64, IngestError> {
        let total: usize = req.resource_spans.iter().flat_map(|r| &r.scope_spans).map(|s| s.spans.len()).sum();
        let a = self.ingest.buffer_request(Request::Traces(req), false)?;
        Ok((total as u64).saturating_sub(a.rejected))
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
        api::router(self.api_state.clone())
    }

    pub fn openapi(&self) -> utoipa::openapi::OpenApi {
        api::openapi()
    }

    /// The longest window the query API accepts (`otel.query.max_window`).
    pub fn max_window(&self) -> Duration {
        self.cfg.query.max_window
    }

    /// The services with any span, log or metric in the window, sorted, at most
    /// [`MAX_SERVICE_NAMES`]; `true` when there are more.
    pub async fn service_names(&self, w: Window) -> std::result::Result<(Vec<String>, bool), ApiErr> {
        Ok(self.api_state.reader.run(move |c| query::attributes::service_names(c, &w)).await?)
    }

    /// How many spans of this trace are in the window, and the first start time.
    pub async fn trace_spans(&self, w: Window, trace_id: String) -> std::result::Result<Counted, ApiErr> {
        Ok(self.api_state.reader.run(move |c| query::traces::count(c, &w, &trace_id)).await?)
    }

    /// How many log records of this trace are in the window, and the first time.
    pub async fn trace_logs(&self, w: Window, trace_id: String) -> std::result::Result<Counted, ApiErr> {
        Ok(self.api_state.reader.run(move |c| query::logs::count_of_trace(c, &w, &trace_id)).await?)
    }

    /// The span with this id in the window.
    pub async fn find_span(&self, w: Window, span_id: String) -> std::result::Result<Option<Span>, ApiErr> {
        match self.api_state.reader.run(move |c| query::traces::span(c, &w, &span_id)).await {
            Ok(s) => Ok(Some(s)),
            Err(query::QueryError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Ready to serve: the last flush (if any) succeeded and the catalog
    /// answers. The error is a short reason (details are logged).
    pub async fn ready(&self) -> std::result::Result<(), String> {
        if let Some(e) = self.stats.last_flush_error() {
            tracing::warn!(error = %e, "readiness: the last flush failed");
            return Err("otel flush failing".into());
        }
        let probe = async {
            let conn = self.lake.connect()?;
            let sql = format!("SELECT id FROM {}.current_snapshot()", store::CATALOG);
            tokio::task::spawn_blocking(move || conn.query_row(&sql, [], |r| r.get::<_, i64>(0))).await??;
            anyhow::Ok(())
        };
        let res = probe.await;
        res.map(|_| ()).map_err(|e| {
            tracing::warn!(error = %store::redact(&format!("{e:#}")), "readiness check failed");
            "otel catalog not ready".to_string()
        })
    }

    /// Run the flusher and (if enabled) maintenance until `shutdown`
    /// completes, then flush remaining buffered data. Stop the receivers
    /// before the shutdown future resolves so nothing arrives afterwards.
    /// Ends earlier, with an error, when the catalog gets invalidated: the
    /// process must exit, as only attaching the catalog again helps.
    pub async fn run_background(&self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let cfg = &self.cfg;
        let flusher = Flusher::start(&self.lake, self.buffer.clone(), cfg.ingest.flush_interval, self.stats.clone())?;
        let maintenance = if cfg.maintenance.enabled {
            Some(Maintenance::start(&self.lake, cfg.maintenance.clone(), cfg.retention.clone(), self.stats.clone())?)
        } else {
            None
        };
        tokio::select! {
            () = shutdown => {}
            () = self.buffer.catalog_lost.cancelled() => {}
        }
        info!("shutting down: flushing buffered data");
        // Start the final flush right away; stopping maintenance may take a
        // moment (it finishes its current statement).
        self.buffer.shutdown();
        tokio::task::spawn_blocking(move || {
            if let Some(m) = maintenance {
                m.stop();
            }
            flusher.stop();
        })
        .await?;
        if self.buffer.catalog_lost.is_cancelled() {
            anyhow::bail!(mallard_duck::CATALOG_INVALIDATED);
        }
        Ok(())
    }
}
