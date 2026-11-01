//! `odl` binary: global flags + the commands from the `odl_cli` library.

use clap::Parser;
use odl_cli::{Cmd, Ctx, Output, run};
use odl_client::Client;

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
        let ctx = Ctx { client: Client::new(&cli.url, cli.token)?, output: cli.output };
        run(cli.cmd, &ctx).await
    }
    .await;
    match result {
        Ok(()) => {}
        // `odl logs | head`: the reader is gone, which is not a failure.
        Err(e) if is_broken_pipe(&e) => {}
        Err(e) => {
            eprintln!("{} {}", odl_cli::paint_err("error:"), odl_cli::clean(&format!("{e:#}")));
            std::process::exit(1);
        }
    }
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe))
}
