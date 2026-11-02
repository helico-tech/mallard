//! Sentry-compatible error tracking backed by DuckLake.
//!
//! Embed it on a connection with the DuckLake catalog attached as `lake`
//! (see `mallard_duck::open_connection`); it creates the `sentry` schema:
//!
//! ```no_run
//! # async fn demo(conn: mallard_duck::duckdb::Connection) -> anyhow::Result<()> {
//! use mallard_sentry_server::{Config, Host, Server};
//! let host = Host {
//!     public_url: "https://errors.example.com/sentry".into(),
//!     api_tokens: vec!["secret".into()],
//!     artifacts: None,
//!     catalog_maintenance: None,
//!     maintenance_threads: 0,
//!     commit_lock: Default::default(),
//! };
//! let server = Server::with_connection(Config::default(), host, conn).await?;
//! let app = axum::Router::new().nest("/sentry", server.router());
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
//! axum::serve(listener, app).await?;
//! server.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! The library installs no global tracing subscriber or metrics recorder; it
//! only emits `tracing` events and `metrics` counters.

mod api;
mod config;
mod db;
mod hooks;
mod ingest;
mod sourcemaps;
mod storage;

pub use config::{
    Artifacts, Config, Host, IngestConfig, MaintenanceConfig, QueryConfig, RetentionConfig, SourcemapConfig,
};
pub use hooks::{EventObserver, IngestError, SpanSink, Spans, Transaction, TransactionSink};

use anyhow::Result;
use db::Db;
use mallard_common::auth::{AuthPolicy, Tokens};
use mallard_common::http::ApiErr;
use mallard_common::window::Window;
use mallard_sentry_api::{EventDetail, EventSummary, IssueDetail, Project};
use sourcemaps::SourceMaps;
use std::future::Future;
use std::sync::{Arc, OnceLock};
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
    /// Gets transactions instead of dropping them (set by the host).
    transaction_sink: OnceLock<Arc<dyn TransactionSink>>,
    /// Gets span items instead of dropping them (set by the host).
    span_sink: OnceLock<Arc<dyn SpanSink>>,
    /// Told about stored SDK events (set by the host).
    event_observer: OnceLock<Arc<dyn EventObserver>>,
}

/// A running server: the database writer plus HTTP routers to mount.
#[derive(Clone)]
pub struct Server {
    state: Arc<AppState>,
}

impl Server {
    /// `conn` has the DuckLake catalog attached as `lake` (with secrets
    /// set). We create the `sentry` schema in it. Global DuckDB settings
    /// belong to the host and are left untouched (except
    /// `host.maintenance_threads`, which must be 0 on a shared connection).
    pub async fn with_connection(cfg: Config, host: Host, conn: mallard_duck::duckdb::Connection) -> Result<Server> {
        cfg.validate()?;
        host.validate()?;
        let c = cfg.clone();
        let maintenance =
            db::maintenance::Maintenance { catalog_wide: host.catalog_maintenance, threads: host.maintenance_threads };
        let artifacts = host.artifacts.clone();
        let commit_lock = host.commit_lock.clone();
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
            let db = Db::with_connection(conn, &c, maintenance, commit_lock, mallard_duck::redact(&catalog))?;
            anyhow::Ok((db, storage))
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
                transaction_sink: OnceLock::new(),
                span_sink: OnceLock::new(),
                event_observer: OnceLock::new(),
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

    /// Hand transaction items to `sink` instead of dropping them. Set it
    /// once, before serving; a second call is an error.
    pub fn set_transaction_sink(&self, sink: Arc<dyn TransactionSink>) -> Result<()> {
        self.state.transaction_sink.set(sink).map_err(|_| anyhow::anyhow!("transaction sink already set"))
    }

    /// Hand `span` items (span streaming) to `sink` instead of dropping
    /// them. Set it once, before serving; a second call is an error.
    pub fn set_span_sink(&self, sink: Arc<dyn SpanSink>) -> Result<()> {
        self.state.span_sink.set(sink).map_err(|_| anyhow::anyhow!("span sink already set"))
    }

    /// Report stored SDK events to `observer`. Set it once; a second call
    /// is an error.
    pub fn set_event_observer(&self, observer: Arc<dyn EventObserver>) -> Result<()> {
        self.state.event_observer.set(observer).map_err(|_| anyhow::anyhow!("event observer already set"))
    }

    /// Store an event built in-process (not from an SDK request): the same
    /// normalizing, grouping and write path as ingest, without auth or HTTP.
    /// Returns the event id. A repeated `event_id` is stored once. The
    /// [`EventObserver`] is not told.
    pub async fn ingest_event(&self, project_id: u32, payload: serde_json::Value) -> Result<String, IngestError> {
        if self.state.db.project_by_ref(&project_id.to_string()).await.is_none() {
            return Err(IngestError::UnknownProject);
        }
        let source = ingest::Source { project_id, client_ip: None, from_sdk: false, sent_at: None };
        ingest::process_event(&self.state, &source, payload, None).await
    }

    /// The id of the project with this slug.
    pub fn project_by_slug(&self, slug: &str) -> Option<u32> {
        self.state.db.caches.projects.read().expect("lock").values().find(|p| p.slug == slug).map(|p| p.id)
    }

    /// The id and slug of every project this process knows. Reads no catalog; [`Self::projects`] does.
    pub fn project_slugs(&self) -> Vec<(u32, String)> {
        self.state.db.caches.projects().into_iter().map(|p| (p.id, p.slug)).collect()
    }

    /// Create a project (same rules as `POST /v1/projects`) and return its id. When the slug
    /// is taken, the existing project's id; when a project is the same app under the slug
    /// rule, that project's id (ADR 0031).
    pub async fn create_project(&self, name: &str, slug: &str, platform: Option<&str>) -> Result<u32> {
        api::check_slug(slug).map_err(anyhow::Error::msg)?;
        let (name, slug2, platform) = (name.trim().to_string(), slug.to_string(), platform.map(str::to_string));
        anyhow::ensure!(!name.is_empty(), "name is required");
        let r: Result<db::ProjectRow, db::WriteError> =
            self.state.db.call(|reply| db::WriteCmd::CreateProject { name, slug: slug2, platform, reply }).await;
        match r {
            Ok(p) => Ok(p.id),
            Err(db::WriteError::SlugTaken(found) | db::WriteError::SameApp(found)) => {
                self.project_by_slug(&found).ok_or_else(|| anyhow::anyhow!("project {found} vanished"))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The longest window the query API accepts (`sentry.query.max_window`).
    pub fn max_window(&self) -> std::time::Duration {
        self.state.cfg.query.max_window
    }

    /// All projects, read from the catalog now; fails when it cannot be read.
    pub async fn projects(&self) -> Result<Vec<Project>, ApiErr> {
        self.state.all_projects().await
    }

    /// The project with this id or slug.
    pub async fn project(&self, r: &str) -> Option<Project> {
        self.state.project_ref(r).await
    }

    /// The events of one trace in the window, across projects (at most 50).
    pub async fn trace_events(&self, w: Window, trace_id: uuid::Uuid) -> Result<Vec<EventSummary>, ApiErr> {
        self.state.trace_events(w, trace_id).await
    }

    /// One event by id, in any project.
    pub async fn find_event(&self, id: uuid::Uuid) -> Result<Option<EventDetail>, ApiErr> {
        self.state.event_by_id(id).await
    }

    /// One issue by id (a 16 hex group hash), with the id of its project, in
    /// `project` or in all projects.
    pub async fn find_issues(&self, group: u64, project: Option<u32>) -> Result<Vec<(u32, IssueDetail)>, ApiErr> {
        self.state.issues_by_id(group, project).await
    }

    /// Ready to serve: the writer runs and the catalog answers. The error
    /// is a short reason (details are logged).
    pub async fn ready(&self) -> Result<(), String> {
        self.state.ready().await
    }

    /// Run maintenance once, now: flush inlined data, apply retention and
    /// compact (the same as `POST /v1/maintenance`).
    pub async fn run_maintenance(&self) -> Result<()> {
        self.state.db.call(|reply| db::WriteCmd::Maintenance { reply }).await
    }

    /// Wait for `shutdown` to resolve, then stop cleanly (see [`Server::shutdown`]).
    /// Same shape as `mallard_otel_server::Server::run_background`: it ends earlier, with an
    /// error, when the catalog gets invalidated.
    pub async fn run_background(&self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let lost = self.state.db.catalog_lost.clone();
        tokio::select! {
            () = shutdown => {}
            () = lost.cancelled() => {}
        }
        self.shutdown().await;
        if lost.is_cancelled() {
            anyhow::bail!(mallard_duck::CATALOG_INVALIDATED);
        }
        Ok(())
    }

    /// Commit queued events, flush inlined data and stop the writer thread.
    pub async fn shutdown(&self) {
        self.state.db.shutdown().await;
    }
}
