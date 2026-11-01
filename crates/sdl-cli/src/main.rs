//! `sdl` binary: global flags, then hands off to the library.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "sdl", version, about = "Command-line tool and terminal UI for sentry-ducklake")]
struct Cli {
    /// Server base URL.
    #[arg(long, global = true, env = "SDL_URL", default_value = "http://localhost:8000")]
    url: String,
    /// API token (the server's SDL_API_TOKEN).
    #[arg(long, global = true, env = "SDL_TOKEN", hide_env_values = true, default_value = "")]
    token: String,
    /// Project id or slug. Optional when the server has only one project.
    #[arg(long, short = 'p', global = true, env = "SDL_PROJECT")]
    project: Option<String>,
    /// Print raw JSON instead of tables.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: sdl_cli::Command,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match sdl_client::Client::new(&cli.url, &cli.token) {
        Ok(client) => {
            let ctx = sdl_cli::Ctx::new(client).with_project(cli.project).with_json(cli.json);
            sdl_cli::run(cli.command, &ctx).await
        }
        Err(e) => Err(anyhow::anyhow!(sdl_cli::describe(&e, &cli.url, &sdl_cli::Hints::SDL))),
    };
    if let Err(e) = result {
        let red = std::io::IsTerminal::is_terminal(&std::io::stderr()) && std::env::var_os("NO_COLOR").is_none();
        if red {
            eprintln!("\x1b[31merror:\x1b[0m {e}");
        } else {
            eprintln!("error: {e}");
        }
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
