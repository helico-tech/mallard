use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use obs_server::{Config, Platform};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// Errors (Sentry SDKs) and traces, logs and metrics (OpenTelemetry) on one DuckLake.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// TOML config file. Every key is optional; `OBS_*` env vars override it.
    #[arg(short, long, env = "OBS_CONFIG")]
    config: Option<PathBuf>,
    /// Validate the config and exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = Config::load(args.config.as_deref())?;
    if args.check {
        println!("config ok");
        return Ok(());
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if cfg.server.log_format == "json" {
        tracing_subscriber::fmt().json().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let metrics = metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder()?;

    let platform = Platform::new(cfg).await?.with_metrics(metrics);
    let shutdown = CancellationToken::new();
    let sd = shutdown.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received");
        sd.cancel();
    });
    platform.serve(shutdown).await
}

async fn wait_for_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
