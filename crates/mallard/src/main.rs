//! `mallard`: one binary for the server (`mallard serve`) and everything that
//! talks to it (status, trace, errors, telemetry, tui).

mod server_cmd;
mod trace;
mod tui;

use std::path::PathBuf;

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mallard_client::Client;
use mallard_term::color::{ColorChoice, Out};
use mallard_term::error::{Described, Hints};
use mallard_term::text::clean;

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
    /// Colour: auto (a terminal without NO_COLOR), always or never.
    #[arg(long, global = true, value_enum, default_value_t)]
    color: ColorChoice,
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
        /// Also open the catalog and storage (extensions, secrets, attach).
        #[arg(long)]
        connect: bool,
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
        #[arg(short, long, env = "MALLARD_PROJECT", global = true)]
        project: Option<String>,
        #[command(subcommand)]
        cmd: sdl_cli::Command,
    },
    /// OpenTelemetry data: services, traces, logs, metrics.
    #[command(alias = "otel")]
    Telemetry {
        #[command(subcommand)]
        cmd: odl_cli::Command,
    },
}

/// Hints in `mallard errors …` messages name mallard's command and env vars.
const ERRORS_HINTS: Hints = Hints {
    command: "mallard errors",
    url_env: "MALLARD_URL",
    token_env: "MALLARD_TOKEN",
    project_env: "MALLARD_PROJECT",
};

/// The same for `mallard telemetry …`.
const TELEMETRY_HINTS: Hints = Hints { command: "mallard telemetry", ..ERRORS_HINTS };

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let color = cli.color;
    mallard_term::error::report(run(cli).await, color)
}

async fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Serve { config } => return server_cmd::serve(config.as_deref()).await,
        Cmd::Check { config, connect } => return server_cmd::check(config.as_deref(), connect).await,
        Cmd::InstallExtensions { dir } => return server_cmd::install_extensions(&dir),
        _ => {}
    }
    let client = Client::new(&cli.url, cli.token.as_deref()).map_err(|e| described(e, &cli.url))?;
    let out = Out::stdout(cli.color);
    match cli.cmd {
        Cmd::Serve { .. } | Cmd::Check { .. } | Cmd::InstallExtensions { .. } => unreachable!(),
        Cmd::Status => status(&client, cli.json, out).await,
        Cmd::Tui { project } => tui::run(&client, project).await,
        Cmd::Errors { project, cmd } => {
            let ctx = sdl_cli::Ctx::new(client.sentry.clone())
                .with_project(project)
                .with_json(cli.json)
                .with_color(cli.color)
                .with_hints(ERRORS_HINTS);
            sdl_cli::run(cmd, &ctx).await
        }
        Cmd::Telemetry { cmd } => {
            let ctx = odl_cli::Ctx::new(client.otel.clone())
                .with_json(cli.json)
                .with_color(cli.color)
                .with_hints(TELEMETRY_HINTS);
            odl_cli::run(cmd, &ctx).await
        }
        Cmd::Trace { trace_id, since } => {
            let t = client.trace_overview(&trace_id, since.as_deref()).await.map_err(|e| described(e, &cli.url))?;
            if cli.json {
                out.line(&mallard_term::text::json_safe(&serde_json::to_string_pretty(&t)?))?;
            } else {
                out.line(&trace::render(&t))?;
            }
            Ok(())
        }
    }
}

/// One friendly line for a client error, worded and hinted like the
/// `errors` and `telemetry` commands do it.
fn describe(e: &mallard_client::Error, base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    match e {
        mallard_client::Error::Sentry(e) => {
            format!("errors: {}", sdl_cli::describe(e, &format!("{base_url}/sentry"), &ERRORS_HINTS))
        }
        mallard_client::Error::Otel(e) => {
            format!("telemetry: {}", odl_cli::describe(e, &format!("{base_url}/otel"), &TELEMETRY_HINTS))
        }
        e => clean(&e.to_string()).into_owned(),
    }
}

fn described(e: mallard_client::Error, base_url: &str) -> anyhow::Error {
    let msg = describe(&e, base_url);
    anyhow::Error::new(e).context(Described(msg))
}

async fn status(c: &Client, json: bool, out: Out) -> Result<()> {
    let (s, o) = tokio::join!(c.sentry.info(), c.otel.stats());
    let s = s.map_err(|e| sdl_cli::describe(&e, c.sentry.base_url(), &ERRORS_HINTS));
    let o = o.map_err(|e| odl_cli::describe(&e, c.otel.base_url(), &TELEMETRY_HINTS));
    if json {
        fn side<T: serde::Serialize>(r: &Result<T, String>) -> serde_json::Value {
            match r {
                Ok(v) => serde_json::json!({ "ok": true, "info": v }),
                Err(e) => serde_json::json!({ "ok": false, "error": e }),
            }
        }
        let v = serde_json::json!({ "sentry": side(&s), "otel": side(&o) });
        out.line(&mallard_term::text::json_safe(&serde_json::to_string_pretty(&v)?))?;
        return Ok(());
    }
    out.line(&status_text(c.base_url(), s, o))?;
    Ok(())
}

/// The text of `mallard status`: one line per side, server text cleaned.
fn status_text(
    base_url: &str,
    s: Result<mallard_client::sdl_client::types::ServerInfo, String>,
    o: Result<mallard_client::odl_client::api::Stats, String>,
) -> String {
    use mallard_term::fmt::{bytes, compact};
    let mut text = format!("server    {}\n", clean(base_url));
    let error = |e: &str| format!("{} {}", mallard_term::color::red("ERROR"), clean(e));
    match s {
        Ok(i) => text.push_str(&format!(
            "errors    ok  v{}  duckdb {}  queue {}/{}\n",
            clean(&i.version),
            clean(&i.duckdb_version),
            i.queue_depth,
            i.queue_capacity
        )),
        Err(e) => text.push_str(&format!("errors    {}\n", error(&e))),
    }
    match o {
        Ok(st) => {
            let rows: i64 = st.storage.tables.iter().map(|t| t.rows + t.inlined_rows).sum();
            let size: i64 = st.storage.tables.iter().map(|t| t.bytes).sum();
            text.push_str(&format!(
                "telemetry ok  v{}  up {}s  buffered {} rows  stored {} rows / {}  flush errors {}",
                clean(&st.version),
                st.uptime_secs,
                compact(st.ingest.buffered_rows as f64),
                compact(rows as f64),
                bytes(size.max(0) as u64),
                st.ingest.flush_errors
            ));
        }
        Err(e) => text.push_str(&format!("telemetry {}", error(&e))),
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn errors_project_works_before_and_after_the_subcommand() {
        for args in [
            &["mallard", "errors", "-p", "web", "issues", "list"][..],
            &["mallard", "errors", "issues", "list", "-p", "web"][..],
            &["mallard", "errors", "issues", "list", "--project", "web"][..],
        ] {
            let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(matches!(cli.cmd, Cmd::Errors { project: Some(p), .. } if p == "web"), "{args:?}");
        }
    }

    /// Both sides fail with one line in the same style, naming mallard's
    /// env vars (review 2.M).
    #[tokio::test]
    async fn both_sides_report_errors_alike() {
        let mut lines = vec![];
        for args in [
            &["mallard", "--url", "http://127.0.0.1:9", "errors", "-p", "web", "issues", "list"][..],
            &["mallard", "--url", "http://127.0.0.1:9", "telemetry", "services"][..],
            &["mallard", "--url", "http://127.0.0.1:9", "trace", "0af7651916cd43dd8448eb211c80319c"][..],
        ] {
            let e = run(Cli::try_parse_from(args).unwrap()).await.unwrap_err();
            let msg = mallard_term::error::message(&e);
            assert!(msg.contains("cannot reach server at http://127.0.0.1:9/"), "{args:?}: {msg}");
            assert!(msg.contains("set MALLARD_URL"), "{args:?}: {msg}");
            assert!(!msg.contains('\n'), "{msg}");
            lines.push(msg);
        }
        assert!(lines[0].starts_with("cannot reach server at http://127.0.0.1:9/sentry"), "{}", lines[0]);
        assert!(lines[1].starts_with("cannot reach server at http://127.0.0.1:9/otel"), "{}", lines[1]);
    }

    #[test]
    fn shared_global_flags() {
        let cli =
            Cli::try_parse_from(["mallard", "telemetry", "traces", "-n", "5", "--json", "--color", "never"]).unwrap();
        assert!(cli.json && cli.color == ColorChoice::Never);
        assert!(Cli::try_parse_from(["mallard", "errors", "events", "list", "-t", "abc", "-n", "3"]).is_ok());
        assert!(Cli::try_parse_from(["mallard", "errors", "issues", "list", "-s", "1h"]).is_ok());
    }

    #[test]
    fn status_names_each_side_once_and_cleans_server_text() {
        let text = status_text(
            "http://h",
            Err("cannot reach server at http://h/sentry (is it running? set MALLARD_URL)".into()),
            Err("down\x1b[2J".into()),
        );
        let plain = mallard_term::color::strip_ansi(&text);
        assert_eq!(
            plain,
            "server    http://h\nerrors    ERROR cannot reach server at http://h/sentry (is it running? set MALLARD_URL)\ntelemetry ERROR down\\u{1b}[2J"
        );
    }
}
