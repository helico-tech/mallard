//! `obs`: command line for obs-ducklake.

mod trace;

use anyhow::Result;
use clap::{Parser, Subcommand};
use obs_client::Client;

#[derive(Parser)]
#[command(name = "obs", version, about = "Errors, traces, logs and metrics from obs-ducklake", propagate_version = true)]
struct Cli {
    /// Server base URL (its http_addr).
    #[arg(long, env = "OBS_URL", default_value = "http://localhost:8080", global = true)]
    url: String,
    /// API token (one of the server's server.api_tokens).
    #[arg(long, env = "OBS_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Health, ingest and storage of both sides.
    Status,
    /// Everything about one trace: spans, Sentry errors and logs.
    Trace {
        /// Trace id (32 hex characters).
        trace_id: String,
        /// Search window, e.g. 24h, 7d.
        #[arg(short, long)]
        since: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.url, cli.token.as_deref())?;
    match cli.cmd {
        Cmd::Status => status(&client, cli.json).await,
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
