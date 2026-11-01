//! `odl` binary: global flags + the commands from the `odl_cli` library.

use clap::Parser;
use odl_cli::{Cmd, Ctx, Output, run};
use odl_client::Client;
use owo_colors::OwoColorize;

#[derive(Parser)]
#[command(name = "odl", version, about = "Query otel-ducklake from the terminal", propagate_version = true)]
struct Cli {
    /// Query API base URL.
    #[arg(long, env = "ODL_URL", default_value = "http://localhost:8080", global = true)]
    url: String,
    /// API bearer token.
    #[arg(long, env = "ODL_TOKEN", global = true, hide_env_values = true)]
    token: Option<String>,
    /// Output format.
    #[arg(short, long, value_enum, default_value_t = Output::Table, global = true)]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = async {
        let ctx = Ctx { client: Client::new(&cli.url, cli.token.clone())?, output: cli.output };
        run(cli.cmd, &ctx).await
    }
    .await;
    if let Err(e) = result {
        eprintln!("{} {e:#}", "error:".red().bold());
        std::process::exit(1);
    }
}
