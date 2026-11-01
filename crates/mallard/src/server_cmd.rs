//! `mallard serve | check | install-extensions`.

use std::path::Path;

use anyhow::{Context, Result};
use mallard_server::{Config, Platform};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

const ALL_EXTENSIONS: &[&str] =
    &["ducklake", "httpfs", "aws", "azure", "postgres", "sqlite", "mysql", "json", "parquet"];

pub fn check(config: Option<&Path>) -> Result<()> {
    Config::load(config)?;
    println!("config ok");
    Ok(())
}

pub async fn serve(config: Option<&Path>) -> Result<()> {
    let cfg = Config::load(config)?;
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

pub fn install_extensions(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let conn = duckdb::Connection::open_in_memory()?;
    let d = dir.to_str().context("extension dir must be UTF-8")?.replace('\'', "''");
    conn.execute_batch(&format!("SET extension_directory = '{d}'"))?;
    for ext in ALL_EXTENSIONS {
        conn.execute_batch(&format!("INSTALL {ext}")).with_context(|| format!("installing extension {ext}"))?;
        println!("installed {ext}");
    }
    Ok(())
}

async fn wait_for_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
