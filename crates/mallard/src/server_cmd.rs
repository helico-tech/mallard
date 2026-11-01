//! `mallard serve | check | install-extensions`.

use std::path::Path;

use anyhow::{Context, Result};
use mallard_common::{init_tracing, install_prometheus, shutdown_signal};
use mallard_server::{Config, Platform};
use tokio_util::sync::CancellationToken;

/// Load and validate the config; with `connect`, also open the catalog and
/// storage (extensions, secrets, `ATTACH`).
pub async fn check(config: Option<&Path>, connect: bool) -> Result<()> {
    let cfg = Config::load(config)?;
    if connect {
        tokio::task::spawn_blocking(move || mallard_duck::open_connection(&cfg.storage, &cfg.duckdb))
            .await?
            .context("opening the DuckLake catalog")?;
        println!("config ok, catalog and storage reachable");
    } else {
        println!("config ok");
    }
    Ok(())
}

pub async fn serve(config: Option<&Path>) -> Result<()> {
    let cfg = Config::load(config)?;
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

pub fn install_extensions(dir: &Path) -> Result<()> {
    let duck = mallard_duck::DuckDbConfig {
        extension_directory: dir.to_str().context("extension dir must be UTF-8")?.to_string(),
        ..Default::default()
    };
    mallard_duck::install_extensions(&duck, mallard_duck::ALL_EXTENSIONS)?;
    for ext in mallard_duck::ALL_EXTENSIONS {
        println!("installed {ext}");
    }
    Ok(())
}
