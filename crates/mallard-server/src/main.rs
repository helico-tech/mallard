//! `mallard-server`: runs mallard (Sentry ingest, OTLP receivers, both query
//! APIs) on one DuckLake. Talk to it with the `mallard` CLI/TUI.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mallard_common::{init_tracing, install_prometheus, shutdown_signal};
use mallard_server::{Config, Platform};
use tokio_util::sync::CancellationToken;

/// Errors (Sentry SDKs) plus traces, logs and metrics (OpenTelemetry) on one DuckLake.
#[derive(Parser)]
#[command(name = "mallard-server", version)]
struct Args {
    /// TOML config file. Every key is optional; MALLARD_<SECTION>__<KEY> env vars override it.
    #[arg(short, long, env = "MALLARD_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (default).
    Serve,
    /// Validate the config and exit.
    Check {
        /// Also open the catalog and storage (extensions, secrets, attach).
        #[arg(long)]
        connect: bool,
    },
    /// Run both sides' maintenance once and exit: flush, retention, compaction.
    /// Stop the server first when the catalog is a DuckDB file or SQLite.
    Maintenance {
        /// Merge all telemetry files below the target size, not only small ones.
        #[arg(long)]
        full: bool,
    },
    /// Download every DuckDB extension any config may need, so the server
    /// runs without internet access (used when building the Docker image).
    InstallExtensions {
        #[arg(long)]
        dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = args.config.as_deref();
    match args.cmd.unwrap_or(Cmd::Serve) {
        Cmd::Serve => serve(config).await,
        Cmd::Check { connect } => check(config, connect).await,
        Cmd::Maintenance { full } => maintenance(config, full).await,
        Cmd::InstallExtensions { dir } => install_extensions(&dir),
    }
}

/// Load and validate the config; with `connect`, also open the catalog and
/// storage (extensions, secrets, `ATTACH`).
async fn check(config: Option<&Path>, connect: bool) -> Result<()> {
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

async fn serve(config: Option<&Path>) -> Result<()> {
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

async fn maintenance(config: Option<&Path>, full: bool) -> Result<()> {
    let cfg = Config::load(config)?;
    init_tracing(cfg.server.log_format);
    for line in mallard_server::run_maintenance(cfg, full).await? {
        println!("{line}");
    }
    Ok(())
}

fn install_extensions(dir: &Path) -> Result<()> {
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
