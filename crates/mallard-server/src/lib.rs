//! mallard: sentry-ducklake and otel-ducklake in one process, on one
//! DuckLake catalog (see docs/adr/0001-shared-ducklake-connection.md).
//!
//! Routes on `server.http_addr`:
//! - `/sentry/...`  everything of sentry-ducklake (ingest, `/v1` API, sentry-cli)
//! - `/otel/...`    otel-ducklake query API (`/otel/api/v1/...`, `/otel/docs`)
//! - `/v1/traces|logs|metrics`  OTLP/HTTP (also on `server.otlp_http_addr`)
//! - `/healthz`, `/readyz`, `/metrics`
//!
//! OTLP/gRPC is served on `server.otlp_grpc_addr`.

pub mod config;

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use axum::{Router, response::IntoResponse, routing::get};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tonic::transport::server::TcpIncoming;
use tower_http::{catch_panic::CatchPanicLayer, compression::CompressionLayer, trace::TraceLayer};
use tracing::info;

pub use config::Config;
use config::{OTEL_PREFIX, SENTRY_PREFIX};

pub struct Platform {
    cfg: Arc<Config>,
    otel: odl_server::Server,
    sentry: sdl_server::Server,
    metrics: Option<PrometheusHandle>,
}

impl Platform {
    /// Open the shared DuckLake catalog and start both libraries on it.
    pub async fn new(cfg: Config) -> Result<Platform> {
        let ocfg = cfg.otel_config();
        let scfg = cfg.sentry_config();
        let c = ocfg.clone();
        let conn = tokio::task::spawn_blocking(move || odl_server::store::open_connection(&c))
            .await?
            .context("opening the DuckLake catalog")?;
        let otel = odl_server::Server::with_connection(ocfg, conn.try_clone()?).await.context("starting otel")?;
        let sentry = sdl_server::Server::with_connection(scfg, conn).await.context("starting sentry")?;
        Ok(Platform { cfg: Arc::new(cfg), otel, sentry, metrics: None })
    }

    /// Serve this Prometheus recorder at `/metrics`.
    pub fn with_metrics(mut self, handle: PrometheusHandle) -> Self {
        self.metrics = Some(handle);
        self
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Everything served on `server.http_addr`.
    pub fn http_router(&self) -> Router {
        // Ready = sentry's writer thread is alive (otel's health is static).
        let sentry_ready = self.sentry.router();
        let mut r = Router::new()
            .nest(SENTRY_PREFIX, self.sentry.router())
            .nest(OTEL_PREFIX, self.otel.api_router())
            .merge(self.otel.otlp_http_router())
            .route("/healthz", get(|| async { "ok" }))
            .route("/readyz", get(move || readyz(sentry_ready.clone())));
        if let Some(h) = self.metrics.clone() {
            r = r.route("/metrics", get(move || std::future::ready(h.render())));
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
    /// libraries, so nothing arrives after the final flush.
    pub async fn serve_on(self, listeners: Listeners, shutdown: CancellationToken) -> Result<()> {
        let Listeners { http, otlp_http, otlp_grpc } = listeners;
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

        let receivers_down = CancellationToken::new();
        let otel_bg = self.otel.run_background(receivers_down.clone().cancelled_owned());
        let sentry_bg = self.sentry.run_background(receivers_down.clone().cancelled_owned());
        let receivers = async {
            let (a, b, c) = tokio::join!(t1, t2, t3);
            receivers_down.cancel();
            a??;
            b??;
            c??;
            anyhow::Ok(())
        };
        let (r, o, ()) = tokio::join!(receivers, otel_bg, sentry_bg);
        info!("stopped");
        r?;
        o?;
        Ok(())
    }
}

/// The three listening sockets. Bind them up front so a busy port fails
/// start-up instead of leaving the server half running.
pub struct Listeners {
    pub http: TcpListener,
    pub otlp_http: TcpListener,
    pub otlp_grpc: TcpListener,
}

impl Listeners {
    pub async fn bind(cfg: &config::ServerConfig) -> Result<Listeners> {
        Ok(Listeners {
            http: bind(&cfg.http_addr).await?,
            otlp_http: bind(&cfg.otlp_http_addr).await?,
            otlp_grpc: bind(&cfg.otlp_grpc_addr).await?,
        })
    }
}

async fn readyz(sentry: Router) -> axum::response::Response {
    use tower::ServiceExt;
    let req = axum::http::Request::get("/readyz").body(axum::body::Body::empty()).expect("static request");
    match sentry.oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(e) => match e {},
    }
}

async fn bind(addr: &str) -> Result<TcpListener> {
    TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
