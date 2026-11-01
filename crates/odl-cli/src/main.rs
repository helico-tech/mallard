//! `odl` binary: global flags + the commands from the `odl_cli` library.

use clap::{Parser, ValueEnum};
use mallard_term::color::ColorChoice;
use odl_cli::{Command, Ctx, run};
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
    /// Print JSON instead of tables.
    #[arg(long, global = true)]
    json: bool,
    /// Old spelling of --json (`-o json`).
    #[arg(short, long, value_enum, global = true, hide = true)]
    output: Option<Output>,
    /// Colour: auto (a terminal without NO_COLOR), always or never.
    #[arg(long, global = true, value_enum, default_value_t)]
    color: ColorChoice,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let json = cli.json || cli.output == Some(Output::Json);
    let result = match Client::new(&cli.url, cli.token.as_deref()) {
        Ok(client) => run(cli.cmd, &Ctx::new(client).with_json(json).with_color(cli.color)).await,
        Err(e) => Err(anyhow::anyhow!(odl_cli::describe(&e, &cli.url, &odl_cli::HINTS))),
    };
    mallard_term::error::report_as(result, cli.color, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn json_flag_and_its_old_spelling() {
        Cli::command().debug_assert();
        for args in [&["odl", "--json", "health"][..], &["odl", "health", "-o", "json"][..]] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(cli.json || cli.output == Some(Output::Json), "{args:?}");
        }
    }
}
