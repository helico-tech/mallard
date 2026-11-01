//! What `mallard serve`, `check --connect`, `maintenance` and
//! `install-extensions` run. The `mallard` binary parses the arguments, loads
//! the config and prints; this is the one place that installs logging and the
//! Prometheus recorder, so nothing else `mallard` does gets them.

use std::path::Path;

use anyhow::{Context, Result};
use mallard_common::{init_tracing, install_prometheus, shutdown_signal};
use tokio_util::sync::CancellationToken;

use crate::{Config, Platform, run_maintenance};

/// Run the server until Ctrl-C or SIGTERM.
pub async fn serve(cfg: Config) -> Result<()> {
    init_tracing(cfg.server.log_format);
    let metrics = install_prometheus()?;
    let platform = Platform::new(cfg).await?.with_metrics(metrics);
    let shutdown = CancellationToken::new();
    let sd = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        sd.cancel();
    });
    platform.serve(shutdown).await
}

/// Open the catalog and storage of `cfg`: extensions, secrets, `ATTACH`.
pub async fn check_connection(cfg: Config) -> Result<()> {
    tokio::task::spawn_blocking(move || mallard_duck::open_connection(&cfg.storage, &cfg.duckdb))
        .await?
        .context("opening the DuckLake catalog")?;
    Ok(())
}

/// [`run_maintenance`] with logging on. Returns one line per step.
pub async fn maintenance(cfg: Config, full: bool) -> Result<Vec<String>> {
    init_tracing(cfg.server.log_format);
    run_maintenance(cfg, full).await
}

/// Download every DuckDB extension any config may need into `dir`. Returns
/// their names.
pub fn install_extensions(dir: &Path) -> Result<&'static [&'static str]> {
    let duck = mallard_duck::DuckDbConfig {
        extension_directory: dir.to_str().context("extension dir must be UTF-8")?.to_string(),
        ..Default::default()
    };
    mallard_duck::install_extensions(&duck, mallard_duck::ALL_EXTENSIONS)?;
    Ok(mallard_duck::ALL_EXTENSIONS)
}
