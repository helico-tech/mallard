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

/// Build the sourcemap store of `cfg`, as start-up does, so a bad
/// `storage.artifacts_path` fails here too. It does not contact the store.
pub fn check_artifacts(cfg: &Config) -> Result<()> {
    let Some(artifacts) = cfg.sentry_host().artifacts else { return Ok(()) };
    artifacts
        .check()
        .map_err(|e| anyhow::anyhow!(mallard_duck::redact(&format!("{e:#}"))))
        .with_context(|| format!("storage.artifacts_path ({})", artifacts.url))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> Config {
        let vars = [("MALLARD_SERVER__API_TOKENS".to_string(), "t".to_string())];
        Config::load_from(Some(toml), vars).unwrap()
    }

    /// The `azure` secret that serves the lake also serves the sourcemap store.
    #[test]
    fn the_azure_secret_builds_the_sourcemap_store() {
        let cfg = config(
            "[storage]\nartifacts_path = \"az://c/artifacts\"\n[[storage.secrets]]\ntype = \"azure\"\n\
             connection_string = \"AccountName=a;AccountKey=aGVsbG8=\"",
        );
        check_artifacts(&cfg).unwrap();
    }

    #[test]
    fn no_sourcemap_store_is_fine() {
        check_artifacts(&config("")).unwrap();
    }

    #[test]
    fn a_bad_scheme_names_the_setting() {
        let err = check_artifacts(&config("[storage]\nartifacts_path = \"r2://b/x\"")).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.starts_with("storage.artifacts_path (r2://b/x)") && text.contains("az://"), "{text}");
    }
}
