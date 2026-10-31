//! `otel-ducklake` binary: thin wrapper around the `odl_server` library.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
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
}

fn init_tracing(format: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,tower_http=warn,h2=warn,tonic=warn".into());
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    if format == "json" { b.json().init() } else { b.init() }
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
            info!(version = env!("CARGO_PKG_VERSION"), "starting otel-ducklake");
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
