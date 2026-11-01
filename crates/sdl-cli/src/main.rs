//! `sdl` binary: global flags, then hands off to the library.

use clap::Parser;
use mallard_term::color::ColorChoice;

#[derive(Parser, Debug)]
#[command(name = "sdl", version, about = "Command-line tool and terminal UI for sentry-ducklake")]
struct Cli {
    /// Server base URL.
    #[arg(long, global = true, env = "SDL_URL", default_value = "http://localhost:8000")]
    url: String,
    /// API token (the server's SDL_API_TOKEN).
    #[arg(long, global = true, env = "SDL_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Project id or slug. Optional when the server has only one project.
    #[arg(long, short = 'p', global = true, env = "SDL_PROJECT")]
    project: Option<String>,
    /// Print JSON instead of tables.
    #[arg(long, global = true)]
    json: bool,
    /// Colour: auto (a terminal without NO_COLOR), always or never.
    #[arg(long, global = true, value_enum, default_value_t)]
    color: ColorChoice,
    #[command(subcommand)]
    command: sdl_cli::Command,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let result = match sdl_client::Client::new(&cli.url, cli.token.as_deref()) {
        Ok(client) => {
            let ctx = sdl_cli::Ctx::new(client).with_project(cli.project).with_json(cli.json).with_color(cli.color);
            sdl_cli::run(cli.command, &ctx).await
        }
        Err(e) => Err(anyhow::anyhow!(sdl_cli::describe(&e, &cli.url, &sdl_cli::HINTS))),
    };
    mallard_term::error::report_as(result, cli.color, json)
}
