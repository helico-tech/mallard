//! `mallard`: one binary for the server (`mallard serve`) and everything that
//! talks to it (status, trace, errors, telemetry, tui).

mod server_cmd;
mod trace;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mallard_client::Client;

#[derive(Parser)]
#[command(
    name = "mallard",
    version,
    about = "Errors, traces, logs and metrics on one DuckLake",
    propagate_version = true
)]
struct Cli {
    /// Server base URL (its http_addr).
    #[arg(long, env = "MALLARD_URL", default_value = "http://localhost:8080", global = true)]
    url: String,
    /// API token (one of the server's server.api_tokens).
    #[arg(long, env = "MALLARD_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server.
    Serve {
        /// TOML config file. Every key is optional; MALLARD_<SECTION>__<KEY> env vars override it.
        #[arg(short, long, env = "MALLARD_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Validate a server config file and exit.
    Check {
        #[arg(short, long, env = "MALLARD_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Download every DuckDB extension any config may need, so the server
    /// runs without internet access (used when building the Docker image).
    #[command(hide = true)]
    InstallExtensions {
        #[arg(long)]
        dir: PathBuf,
    },
    /// Health, ingest and storage of the server.
    Status,
    /// Everything about one trace: spans, Sentry errors and logs.
    Trace {
        /// Trace id (32 hex characters).
        trace_id: String,
        /// Search window, e.g. 24h, 7d.
        #[arg(short, long)]
        since: Option<String>,
    },
    /// Terminal UI: F1 errors, F2 telemetry.
    Tui {
        /// Sentry project id or slug; optional when there is only one project.
        #[arg(short, long, env = "MALLARD_PROJECT")]
        project: Option<String>,
    },
    /// Errors from Sentry SDKs: projects, issues, events, sourcemaps.
    #[command(alias = "sentry")]
    Errors {
        /// Project id or slug; optional when there is only one project.
        #[arg(short, long, env = "MALLARD_PROJECT")]
        project: Option<String>,
        #[command(subcommand)]
        cmd: sdl_cli::Command,
    },
    /// OpenTelemetry data: services, traces, logs, metrics.
    #[command(alias = "otel")]
    Telemetry {
        #[command(subcommand)]
        cmd: odl_cli::Cmd,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { config } => return server_cmd::serve(config.as_deref()).await,
        Cmd::Check { config } => return server_cmd::check(config.as_deref()),
        Cmd::InstallExtensions { dir } => return server_cmd::install_extensions(&dir),
        _ => {}
    }
    let client = Client::new(&cli.url, cli.token.as_deref())?;
    match cli.cmd {
        Cmd::Serve { .. } | Cmd::Check { .. } | Cmd::InstallExtensions { .. } => unreachable!(),
        Cmd::Status => status(&client, cli.json).await,
        Cmd::Tui { project } => tui::run(&client, project).await,
        Cmd::Errors { project, cmd } => {
            sdl_cli::run(cmd, &sdl_cli::Ctx { client: client.sentry.clone(), project, json: cli.json }).await
        }
        Cmd::Telemetry { cmd } => {
            let output = if cli.json { odl_cli::Output::Json } else { odl_cli::Output::Table };
            odl_cli::run(cmd, &odl_cli::Ctx { client: client.otel.clone(), output }).await
        }
        Cmd::Trace { trace_id, since } => {
            let t = client.trace_overview_since(&trace_id, since.as_deref()).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&t)?);
            } else {
                print!("{}", trace::render(&t));
            }
            Ok(())
        }
    }
}

async fn status(c: &Client, json: bool) -> Result<()> {
    let (s, o) = tokio::join!(c.sentry.info(), c.otel.stats());
    if json {
        let v = serde_json::json!({
            "sentry": s.as_ref().map(|x| serde_json::to_value(x).unwrap()).map_err(|e| e.to_string()),
            "otel": o.as_ref().map(|x| serde_json::to_value(x).unwrap()).map_err(|e| e.to_string()),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("server   {}", c.base_url());
    match s {
        Ok(i) => println!(
            "sentry   ok  v{}  duckdb {}  queue {}/{}  catalog {}",
            i.version, i.duckdb_version, i.queue_depth, i.queue_capacity, i.catalog
        ),
        Err(e) => println!("sentry   ERROR {e}"),
    }
    match o {
        Ok(st) => {
            let rows: i64 = st.storage.tables.iter().map(|t| t.rows + t.inlined_rows).sum();
            let bytes: i64 = st.storage.tables.iter().map(|t| t.bytes).sum();
            println!(
                "otel     ok  v{}  up {}s  buffered {} rows  stored {} rows / {:.1} MB  flush errors {}",
                st.version,
                st.uptime_secs,
                st.ingest.buffered_rows,
                rows,
                bytes as f64 / 1e6,
                st.ingest.flush_errors
            );
        }
        Err(e) => println!("otel     ERROR {e}"),
    }
    Ok(())
}
