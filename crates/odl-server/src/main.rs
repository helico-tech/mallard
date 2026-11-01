//! `otel-ducklake` binary: thin wrapper around the `odl_server` library.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use odl_server::{Server, config::Config};
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Parser)]
#[command(
    name = "otel-ducklake",
    version,
    about = "OpenTelemetry collector backed by DuckLake"
)]
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
        #[arg(default_values_t = ["ducklake", "httpfs", "aws", "azure", "postgres", "sqlite", "icu"].map(String::from))]
        names: Vec<String>,
    },
}

fn init_tracing(format: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,tower_http=warn,h2=warn,tonic=warn".into());
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    if format == "json" {
        b.json().init()
    } else {
        b.init()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Serve) {
        Cmd::Openapi => {
            println!("{}", odl_server::api::openapi().to_pretty_json()?);
            Ok(())
        }
        Cmd::Config => {
            print!("{}", Config::example_toml());
            Ok(())
        }
        Cmd::InstallExtensions { names } => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(&cfg.server.log_format);
            odl_server::store::install_extensions(&cfg, &names)?;
            Ok(())
        }
        Cmd::Maintenance { full } => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(&cfg.server.log_format);
            let lake = odl_server::store::Lake::open(&cfg)?;
            let conn = lake.connect()?;
            let settings = odl_server::store::maintenance::Settings::from_config(&cfg)?;
            let t0 = std::time::Instant::now();
            let small = odl_server::store::maintenance::compact(
                &conn,
                &lake.schema,
                Some(settings.small_file_size),
            )?;
            info!(detail = %small, "compaction");
            if full {
                let d = odl_server::store::maintenance::compact(&conn, &lake.schema, None)?;
                info!(detail = %d, "full compaction");
            }
            let d = odl_server::store::maintenance::retention(&conn, &lake.schema, &settings)?;
            info!(detail = %d, elapsed_ms = t0.elapsed().as_millis() as u64, "retention");
            Ok(())
        }
        Cmd::Check => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(&cfg.server.log_format);
            Server::new(cfg).await?;
            info!("configuration and storage OK");
            Ok(())
        }
        Cmd::Serve => {
            let cfg = Config::load(cli.config.as_deref())?;
            init_tracing(&cfg.server.log_format);
            info!(
                version = env!("CARGO_PKG_VERSION"),
                "starting otel-ducklake"
            );
            let server = Server::new(cfg).await?;
            let shutdown = CancellationToken::new();
            let sd = shutdown.clone();
            tokio::spawn(async move {
                wait_for_signal().await;
                info!("signal received");
                sd.cancel();
            });
            server.serve(shutdown).await?;
            info!("stopped");
            Ok(())
        }
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
