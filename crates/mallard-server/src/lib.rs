//! mallard: the Sentry side and the OpenTelemetry side in one process, on one
//! DuckLake catalog (see docs/adr/0001-shared-ducklake-connection.md).
//!
//! Routes on `server.http_addr`:
//! - `/sentry/...`  everything of the Sentry side (ingest, `/v1` API, sentry-cli)
//! - `/api/0/...`   sentry-cli's API again (it only accepts a URL root)
//! - `/otel/...`    telemetry query API (`/otel/api/v1/...`, `/otel/docs`)
//! - `/v1/traces|logs|metrics`  OTLP/HTTP (also on `server.otlp_http_addr`)
//! - `/healthz`, `/readyz`, `/metrics` (on `server.metrics_addr` instead
//!   when that is set)
//!
//! OTLP/gRPC is served on `server.otlp_grpc_addr`.

pub mod bridge;
pub mod commands;
pub mod config;

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use axum::{Router, routing::get};
use mallard_common::http::readiness;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tonic::transport::server::TcpIncoming;
use tower_http::{catch_panic::CatchPanicLayer, compression::CompressionLayer, trace::TraceLayer};
use tracing::info;

pub use config::Config;
use config::{OTEL_PREFIX, SENTRY_PREFIX};

/// Path of the sentry-cli API.
const SENTRY_CLI_PREFIX: &str = "/api/0";

/// sentry-cli (Rust, 2.x and 3.x) refuses a `--url` with a path ("not on URL
/// root"), so its API is served on the root as well as under `/sentry`.
/// `nest_service` strips the prefix (and, unlike a route with a wildcard,
/// adds no path parameter the Sentry handlers would see); put it back so
/// the Sentry router matches its own `/api/0/...` routes.
fn sentry_cli_at_root(
    sentry: Router,
) -> impl tower::Service<
    axum::extract::Request,
    Response = axum::response::Response,
    Error = std::convert::Infallible,
    Future = impl Send + 'static,
> + Clone
+ Send
+ Sync
+ 'static {
    use tower::ServiceExt;
    sentry.into_service().map_request(|mut req: axum::extract::Request| {
        let rest = req.uri().path_and_query().map_or("/", |pq| pq.as_str());
        let mut parts = req.uri().clone().into_parts();
        parts.path_and_query = format!("{SENTRY_CLI_PREFIX}{rest}").parse().ok();
        if let Ok(uri) = axum::http::Uri::from_parts(parts) {
            *req.uri_mut() = uri;
        }
        req
    })
}

pub struct Platform {
    cfg: Arc<Config>,
    otel: Arc<mallard_otel_server::Server>,
    sentry: mallard_sentry_server::Server,
    /// The bridge's workers (Sentry transactions, OTel exceptions).
    bridge: bridge::Workers,
    metrics: Option<PrometheusHandle>,
}

impl Platform {
    /// Open the shared DuckLake catalog and start both libraries on it.
    pub async fn new(cfg: Config) -> Result<Platform> {
        let ocfg = cfg.otel_config();
        let (st, db) = (cfg.storage.clone(), cfg.duckdb.clone());
        let conn = tokio::task::spawn_blocking(move || mallard_duck::open_connection(&st, &db))
            .await?
            .context("opening the DuckLake catalog")?;
        let otel = Arc::new(
            mallard_otel_server::Server::with_connection(ocfg, conn.try_clone()?).await.context("starting otel")?,
        );
        let sentry = mallard_sentry_server::Server::with_connection(cfg.sentry.clone(), cfg.sentry_host(), conn)
            .await
            .context("starting sentry")?;
        let bridge = bridge::install(&cfg.bridge, &sentry, &otel).context("starting the bridge")?;
        Ok(Platform { cfg: Arc::new(cfg), otel, sentry, bridge, metrics: None })
    }

    /// Serve this Prometheus recorder at `/metrics`.
    pub fn with_metrics(mut self, handle: PrometheusHandle) -> Self {
        self.metrics = Some(handle);
        self
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// `/metrics`, when a recorder was given.
    fn metrics_router(&self) -> Router {
        match self.metrics.clone() {
            Some(h) => Router::new().route("/metrics", get(move || std::future::ready(h.render()))),
            None => Router::new(),
        }
    }

    /// Everything served on `server.http_addr`.
    pub fn http_router(&self) -> Router {
        let (sentry, otel) = (self.sentry.clone(), self.otel.clone());
        let mut r = Router::new()
            .nest(SENTRY_PREFIX, self.sentry.router())
            .nest_service(SENTRY_CLI_PREFIX, sentry_cli_at_root(self.sentry.router()))
            .nest(OTEL_PREFIX, self.otel.api_router())
            .merge(self.otel.otlp_http_router())
            .route("/healthz", get(|| async { "ok" }))
            .route("/readyz", get(move || ready(sentry.clone(), otel.clone())));
        if self.cfg.server.metrics_addr.is_empty() {
            r = r.merge(self.metrics_router());
        }
        r
    }

    /// Bind the configured addresses, then [`Platform::serve_on`].
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let listeners = Listeners::bind(&self.cfg.server).await?;
        self.serve_on(listeners, shutdown).await
    }

    /// Serve on already bound listeners until `shutdown` is cancelled or
    /// any listener fails. Then stop the receivers first and flush both
    /// libraries, so nothing arrives after the final flush. The bridge
    /// stores the Sentry transactions it still holds before otel stops,
    /// and the OTel exceptions it still holds before Sentry stops.
    pub async fn serve_on(mut self, listeners: Listeners, shutdown: CancellationToken) -> Result<()> {
        let Listeners { http, otlp_http, otlp_grpc, metrics } = listeners;
        info!(
            http = %http.local_addr()?, otlp_http = %otlp_http.local_addr()?, otlp_grpc = %otlp_grpc.local_addr()?,
            "listening"
        );

        let http_app = self
            .http_router()
            .layer(CompressionLayer::new())
            .layer(CatchPanicLayer::new())
            .layer(TraceLayer::new_for_http())
            .into_make_service_with_connect_info::<SocketAddr>();
        let otlp_app = self
            .otel
            .otlp_http_router()
            .layer(CatchPanicLayer::new())
            .layer(TraceLayer::new_for_http())
            .into_make_service_with_connect_info::<SocketAddr>();
        let grpc = tonic::transport::Server::builder().add_routes(self.otel.grpc_routes());
        let metrics_app = self.metrics_router();

        // Whichever receiver stops first (error or shutdown) stops the others.
        let sd = shutdown.clone();
        let t1 = tokio::spawn(async move {
            let r = axum::serve(http, http_app).with_graceful_shutdown(sd.clone().cancelled_owned()).await;
            sd.cancel();
            r.context("HTTP server")
        });
        let sd = shutdown.clone();
        let t2 = tokio::spawn(async move {
            let r = axum::serve(otlp_http, otlp_app).with_graceful_shutdown(sd.clone().cancelled_owned()).await;
            sd.cancel();
            r.context("OTLP/HTTP server")
        });
        let sd = shutdown.clone();
        let t3 = tokio::spawn(async move {
            let incoming = TcpIncoming::from(otlp_grpc);
            let r = grpc.serve_with_incoming_shutdown(incoming, sd.clone().cancelled_owned()).await;
            sd.cancel();
            r.context("OTLP/gRPC server")
        });

        let sd = shutdown.clone();
        let t4 = tokio::spawn(async move {
            let Some(l) = metrics else { return Ok(()) };
            info!(metrics = %l.local_addr()?, "listening");
            let r = axum::serve(l, metrics_app).with_graceful_shutdown(sd.clone().cancelled_owned()).await;
            sd.cancel();
            r.context("metrics server")
        });

        let receivers_down = CancellationToken::new();
        let bridge::Workers { transactions, exceptions } = std::mem::take(&mut self.bridge);
        // The bridge buffers the transactions it holds before otel's final flush.
        let spans_done = CancellationToken::new();
        let transactions_bg = async {
            match transactions {
                Some(w) => w.run(receivers_down.clone()).await,
                None => receivers_down.cancelled().await,
            }
            spans_done.cancel();
        };
        // If a background job ends early (e.g. fails to start), stop the
        // receivers too: data they accept would never be written.
        let otel_bg = async {
            let r = self.otel.run_background(spans_done.clone().cancelled_owned()).await;
            shutdown.cancel();
            r
        };
        let events_done = CancellationToken::new();
        let exceptions_bg = async {
            match exceptions {
                Some(w) => w.run(receivers_down.clone()).await,
                None => receivers_down.cancelled().await,
            }
            events_done.cancel();
        };
        let sentry_bg = async {
            let r = self.sentry.run_background(events_done.clone().cancelled_owned()).await;
            shutdown.cancel();
            r
        };
        let receivers = async {
            let (a, b, c, d) = tokio::join!(t1, t2, t3, t4);
            receivers_down.cancel();
            a??;
            b??;
            c??;
            d??;
            anyhow::Ok(())
        };
        let (r, o, s, (), ()) = tokio::join!(receivers, otel_bg, sentry_bg, transactions_bg, exceptions_bg);
        info!("stopped");
        r?;
        o?;
        s?;
        Ok(())
    }
}

/// Run both sides' maintenance once on the lake of `cfg` (`mallard
/// maintenance`). Sentry: flush inlined rows, retention, compaction. Otel:
/// the same for the telemetry tables (`full`: merge all files below the
/// target size), then the catalog-wide snapshot expiry and file cleanup, so
/// otel goes last (ADR 0001). Returns one line per step. With a DuckDB-file
/// or SQLite catalog, stop the server first.
pub async fn run_maintenance(cfg: Config, full: bool) -> Result<Vec<String>> {
    let (st, db) = (cfg.storage.clone(), cfg.duckdb.clone());
    let conn = tokio::task::spawn_blocking(move || mallard_duck::open_connection(&st, &db))
        .await?
        .context("opening the DuckLake catalog")?;
    let sentry = mallard_sentry_server::Server::with_connection(cfg.sentry.clone(), cfg.sentry_host(), conn)
        .await
        .context("starting sentry")?;
    let result = sentry.run_maintenance().await;
    sentry.shutdown().await;
    // All of its connections must be gone before otel opens the catalog.
    drop(sentry);
    result.context("sentry maintenance")?;
    let mut done = vec!["sentry: flushed inlined rows, applied retention, compacted".to_string()];
    let ocfg = cfg.otel_config();
    let otel = tokio::task::spawn_blocking(move || mallard_otel_server::run_maintenance(&ocfg, full))
        .await?
        .context("otel maintenance")?;
    done.extend(otel.into_iter().map(|line| format!("otel: {line}")));
    Ok(done)
}

/// The listening sockets. Bind them up front so a busy port fails
/// start-up instead of leaving the server half running.
pub struct Listeners {
    pub http: TcpListener,
    pub otlp_http: TcpListener,
    pub otlp_grpc: TcpListener,
    /// `server.metrics_addr`, when set.
    pub metrics: Option<TcpListener>,
}

impl Listeners {
    pub async fn bind(cfg: &config::ServerConfig) -> Result<Listeners> {
        let metrics = match cfg.metrics_addr.as_str() {
            "" => None,
            addr => Some(bind(addr).await?),
        };
        Ok(Listeners {
            http: bind(&cfg.http_addr).await?,
            otlp_http: bind(&cfg.otlp_http_addr).await?,
            otlp_grpc: bind(&cfg.otlp_grpc_addr).await?,
            metrics,
        })
    }
}

/// Ready when both sides are ready; the body names what is not.
async fn ready(
    sentry: mallard_sentry_server::Server,
    otel: Arc<mallard_otel_server::Server>,
) -> axum::response::Response {
    let (s, o) = tokio::join!(sentry.ready(), otel.ready());
    let why: Vec<String> = [s, o].into_iter().filter_map(Result::err).collect();
    readiness(if why.is_empty() { Ok(()) } else { Err(why.join("; ")) })
}

async fn bind(addr: &str) -> Result<TcpListener> {
    TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// If a background job cannot start, the receivers stop and `serve_on` returns
    /// the error, instead of accepting data that is never flushed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn serve_stops_when_background_fails_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.server.api_tokens = vec!["t".into()];
        cfg.storage.catalog = dir.path().join("catalog.ducklake").display().to_string();
        cfg.storage.data_path = format!("{}/lake/", dir.path().display());
        let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
        let otel = Arc::new(
            mallard_otel_server::Server::with_connection(cfg.otel_config(), conn.try_clone().unwrap()).await.unwrap(),
        );
        let sentry = mallard_sentry_server::Server::with_connection(
            cfg.sentry.clone(),
            cfg.sentry_host(),
            conn.try_clone().unwrap(),
        )
        .await
        .unwrap();
        let bridge = bridge::install(&cfg.bridge, &sentry, &otel).unwrap();
        let platform = Platform { cfg: Arc::new(cfg), otel, sentry, bridge, metrics: None };
        // New connections can no longer select the lake schema, so the flusher fails to start.
        conn.execute_batch("DETACH lake").unwrap();
        let bound = || TcpListener::bind("127.0.0.1:0");
        let listeners = Listeners {
            http: bound().await.unwrap(),
            otlp_http: bound().await.unwrap(),
            otlp_grpc: bound().await.unwrap(),
            metrics: None,
        };
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            platform.serve_on(listeners, CancellationToken::new()),
        )
        .await
        .expect("serve_on must return when a background job fails");
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn a_busy_port_fails_start_up() {
        let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = config::ServerConfig {
            http_addr: "127.0.0.1:0".into(),
            otlp_http_addr: "127.0.0.1:0".into(),
            ..Default::default()
        };
        cfg.otlp_grpc_addr = busy.local_addr().unwrap().to_string();
        let err = Listeners::bind(&cfg).await.err().expect("gRPC port is taken");
        assert!(err.to_string().contains("binding"), "{err}");
    }
}
