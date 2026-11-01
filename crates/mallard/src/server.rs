//! The server commands: `serve`, `check`, `maintenance`, `install-extensions`
//! (feature `server`, on by default; ADR 0023).
//!
//! The definitions exist in both builds, hidden in the slim one, so there
//! `mallard serve` is a clear usage error instead of "unknown command". They
//! only need clap and std; the work is in `mallard_server::commands`.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use mallard_term::output::Output;

#[derive(Subcommand)]
pub(crate) enum ServerCmd {
    /// Run the server: Sentry ingest, OTLP receivers and both query APIs.
    #[cfg_attr(not(feature = "server"), command(hide = true))]
    Serve(ConfigArg),
    /// Validate the server config and exit.
    #[cfg_attr(not(feature = "server"), command(hide = true))]
    Check {
        #[command(flatten)]
        config: ConfigArg,
        /// Also open the catalog and storage (extensions, secrets, attach) and build
        /// the sourcemap store (no request to it).
        #[arg(long)]
        connect: bool,
    },
    /// Run both sides' maintenance once and exit: flush, retention, compaction.
    /// Stop the server first when the catalog is a DuckDB file or SQLite.
    #[cfg_attr(not(feature = "server"), command(hide = true))]
    Maintenance {
        #[command(flatten)]
        config: ConfigArg,
        /// Merge all telemetry files below the target size, not only small ones.
        #[arg(long)]
        full: bool,
    },
    /// Download every DuckDB extension any config may need, so the server
    /// runs without internet access (used when building the Docker image).
    #[cfg_attr(not(feature = "server"), command(hide = true))]
    InstallExtensions {
        #[arg(long)]
        dir: PathBuf,
    },
}

#[derive(clap::Args)]
pub(crate) struct ConfigArg {
    /// TOML config file. Every key is optional; MALLARD_<SECTION>__<KEY> env vars override it.
    #[arg(short, long, env = "MALLARD_CONFIG")]
    config: Option<PathBuf>,
}

#[cfg(not(feature = "server"))]
pub(crate) async fn run(_: ServerCmd, _: &Output) -> Result<()> {
    use mallard_term::error::{Described, Kind};
    let d = Described::new(Kind::Usage, "this build of mallard has no server")
        .hint("it was built with --no-default-features; install the default build to run these commands");
    Err(anyhow::Error::new(d))
}

#[cfg(feature = "server")]
pub(crate) async fn run(cmd: ServerCmd, output: &Output) -> Result<()> {
    use mallard_server::commands;
    use serde_json::json;

    match cmd {
        ServerCmd::Serve(config) => commands::serve(config.load()?).await,
        ServerCmd::Check { config, connect } => {
            let cfg = config.load()?;
            if connect {
                // Exit 2: as with any other bad setting, trying again will not help.
                commands::check_artifacts(&cfg).map_err(|e| mallard_term::error::usage(format!("{e:#}")))?;
                commands::check_connection(cfg).await?;
            }
            if output.text() {
                let text = if connect { "config ok, catalog and storage reachable" } else { "config ok" };
                output.out.line(text)?;
                return Ok(());
            }
            output.object(&json!({"ok": true, "connected": connect}))
        }
        ServerCmd::Maintenance { config, full } => {
            let steps = commands::maintenance(config.load()?, full).await?;
            print_lines(output, "", "steps", &steps)
        }
        ServerCmd::InstallExtensions { dir } => {
            let names: Vec<String> = commands::install_extensions(&dir)?.iter().map(|e| e.to_string()).collect();
            print_lines(output, "installed ", "installed", &names)
        }
    }
}

/// One line each as text, else `{key: [..]}`.
#[cfg(feature = "server")]
fn print_lines(output: &Output, prefix: &str, key: &str, lines: &[String]) -> Result<()> {
    if output.text() {
        for line in lines {
            output.out.line(&format!("{prefix}{line}"))?;
        }
        return Ok(());
    }
    output.object(&serde_json::json!({key: lines}))
}

#[cfg(feature = "server")]
impl ConfigArg {
    /// A config that does not load or validate is a usage error: trying again
    /// will not help (ADR 0008).
    fn load(&self) -> Result<mallard_server::Config> {
        mallard_server::Config::load(self.config.as_deref()).map_err(|e| mallard_term::error::usage(format!("{e:#}")))
    }
}
