//! obs-ducklake: sentry-ducklake and otel-ducklake in one process, on one
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

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{Router, response::IntoResponse, routing::get};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
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
        let otel = odl_server::Server::with_connection(ocfg, conn.try_clone()?)
            .await
            .context("starting otel")?;
        let sentry = sdl_server::Server::with_connection(scfg, conn)
            .await
            .context("starting sentry")?;
        Ok(Platform {
            cfg: Arc::new(cfg),
            otel,
            sentry,
            metrics: None,
        })
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

    /// Bind all listeners and serve until `shutdown` is cancelled. Then stop
    /// the receivers first and flush both libraries.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let s = &self.cfg.server;
        let http_l = bind(&s.http_addr).await?;
        let otlp_l = bind(&s.otlp_http_addr).await?;
        let grpc_addr = s
            .otlp_grpc_addr
            .parse()
            .with_context(|| format!("parsing {}", s.otlp_grpc_addr))?;
        info!(http = %s.http_addr, otlp_http = %s.otlp_http_addr, otlp_grpc = %s.otlp_grpc_addr, "listening");

        let http = self.http_router().layer(TraceLayer::new_for_http());
        let otlp = self
            .otel
            .otlp_http_router()
            .layer(TraceLayer::new_for_http());
        let grpc = tonic::transport::Server::builder().add_routes(self.otel.grpc_routes());

        let sd = shutdown.clone();
        let t1 = tokio::spawn(async move {
            axum::serve(http_l, http)
                .with_graceful_shutdown(sd.cancelled_owned())
                .await
        });
        let sd = shutdown.clone();
        let t2 = tokio::spawn(async move {
            axum::serve(otlp_l, otlp)
                .with_graceful_shutdown(sd.cancelled_owned())
                .await
        });
        let sd = shutdown.clone();
        let t3 = tokio::spawn(async move {
            grpc.serve_with_shutdown(grpc_addr, sd.cancelled_owned())
                .await
        });

        // Background jobs stop only after all receivers are down, so
        // nothing arrives after the final flush.
        let receivers_down = CancellationToken::new();
        let otel_bg = self
            .otel
            .run_background(receivers_down.clone().cancelled_owned());
        let sentry_bg = self
            .sentry
            .run_background(receivers_down.clone().cancelled_owned());
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

async fn readyz(sentry: Router) -> axum::response::Response {
    use tower::ServiceExt;
    let req = axum::http::Request::get("/readyz")
        .body(axum::body::Body::empty())
        .expect("static request");
    match sentry.oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(e) => match e {},
    }
}

async fn bind(addr: &str) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))
}
