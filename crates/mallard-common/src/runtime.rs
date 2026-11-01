//! Process set-up for the server binaries: logging, signals, metrics.
//! Libraries never call these; binaries do, once.

use anyhow::{Context, Result};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use serde::{Deserialize, Serialize};

/// Log output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

/// Install the global tracing subscriber. `RUST_LOG` overrides the default
/// filter. No ANSI colors when stdout is not a terminal (files, container
/// log drivers).
pub fn init_tracing(format: LogFormat) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,tower_http=warn,h2=warn,tonic=warn".into());
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let b = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(ansi);
    match format {
        LogFormat::Json => b.json().init(),
        LogFormat::Text => b.init(),
    }
}

/// Resolves on Ctrl-C or SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutdown signal received");
}

/// Histogram buckets in seconds (all `*_seconds` metrics).
const SECONDS: &[f64] = &[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0];
/// Buckets for histograms that count items per batch.
const COUNTS: &[f64] = &[1.0, 5.0, 10.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0, 20000.0, 100000.0];

/// Install the global Prometheus recorder, with histograms (not summaries)
/// for every metric. Serve [`PrometheusHandle::render`] at `/metrics`.
pub fn install_prometheus() -> Result<PrometheusHandle> {
    PrometheusBuilder::new()
        .set_buckets(SECONDS)?
        .set_buckets_for_metric(Matcher::Full("sdl_write_batch_events".into()), COUNTS)?
        .set_buckets_for_metric(Matcher::Full("odl_flush_rows".into()), COUNTS)?
        .install_recorder()
        .context("installing the metrics recorder")
}
