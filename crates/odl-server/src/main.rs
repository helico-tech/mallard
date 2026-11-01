//! `otel-ducklake` binary: thin wrapper around the `odl_server` library.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mallard_common::{init_tracing, shutdown_signal};
use odl_server::{Server, config::Config};
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Parser)]
#[command(name = "otel-ducklake", version, about = "OpenTelemetry collector backed by DuckLake")]
struct Cli {
    /// Config file (TOML). Environment variables ODL_<SECTION>__<KEY> override it.
    #[arg(short, long, env = "ODL_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the collector (default).
    Serve,
    /// Print the OpenAPI document as JSON.
    Openapi,
    /// Print an example configuration file.
    Config,
    /// Load the config, attach storage, create tables, then exit.
    Check,
    /// Run maintenance once (flush inlined rows, merge files, retention) and exit.
    /// With a DuckDB-file or SQLite catalog, stop the server first.
    Maintenance {
        /// Also merge files up to the target size (not only small files).
        #[arg(long)]
        full: bool,
    },
    /// Download DuckDB extensions into `duckdb.extension_directory` (used when
    /// building container images, so they can run without internet access).
    InstallExtensions {
        /// Extensions to install.
        /// json and parquet are compiled into the binary and need no install.
        #[arg(default_values_t = mallard_duck::ALL_EXTENSIONS.iter().map(ToString::to_string))]
        names: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Serve) {
        Cmd::Openapi => {
            println!("{}", odl_server::openapi().to_pretty_json()?);
            Ok(())
        }
        Cmd::Config => {
            print!("{}", Config::example_toml());
            Ok(())
        }
        Cmd::InstallExtensions { names } => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(cfg.server.log_format);
            // DuckDB work blocks; keep it off the async runtime threads.
            tokio::task::spawn_blocking(move || mallard_duck::install_extensions(&cfg.duckdb, &names)).await??;
            Ok(())
        }
        Cmd::Maintenance { full } => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(cfg.server.log_format);
            let t0 = std::time::Instant::now();
            let done = tokio::task::spawn_blocking(move || odl_server::run_maintenance(&cfg, full)).await??;
            for d in done {
                info!(detail = %d, "maintenance");
            }
            info!(elapsed_ms = t0.elapsed().as_millis() as u64, "maintenance done");
            Ok(())
        }
        Cmd::Check => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(cfg.server.log_format);
            Server::new(cfg).await?;
            info!("configuration and storage OK");
            Ok(())
        }
        Cmd::Serve => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(cfg.server.log_format);
            info!(version = env!("CARGO_PKG_VERSION"), "starting otel-ducklake");
            let server = Server::new(cfg).await?;
            let shutdown = CancellationToken::new();
            let sd = shutdown.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                sd.cancel();
            });
            server.serve(shutdown).await?;
            info!("stopped");
            Ok(())
        }
    }
}
