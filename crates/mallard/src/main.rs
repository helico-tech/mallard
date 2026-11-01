//! `mallard`: the CLI and TUI. Talks to a `mallard-server` over HTTP.
//!
//! One flat command tree (ADR 0008): `issues`, `events`, `traces`, `logs`,
//! … The commands come from the two libraries (`sdl-cli`, `odl-cli`);
//! `status`, `setup`, `find`, `trace` and `help agents` span both sides and
//! live here. The old `errors …` / `telemetry …` paths stay as hidden
//! aliases of the same commands.

mod errors;
mod find;
mod help;
mod series;
mod setup;
mod status;
mod trace;
mod tui;

use std::process::ExitCode;

use anyhow::Result;
use clap::{FromArgMatches, Parser, Subcommand};
use mallard_client::Client;
use mallard_term::color::ColorChoice;
use mallard_term::error::{Described, Hints, Kind};
use mallard_term::output::{Format, Output};
use mallard_term::wait::WaitArgs;
use mallard_term::window::WindowArgs;

#[derive(Parser)]
#[command(
    name = "mallard",
    version,
    about = "Errors, traces, logs and metrics on one DuckLake",
    propagate_version = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// Server base URL (its http_addr).
    #[arg(long, env = "MALLARD_URL", default_value = "http://localhost:8080", global = true)]
    url: String,
    /// API token (one of the server's server.api_tokens).
    #[arg(long, env = "MALLARD_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,
    /// Sentry project id or slug (issues, events, …); optional with one project.
    #[arg(short, long, env = "MALLARD_PROJECT", global = true)]
    project: Option<String>,
    /// Output format: text, json (compact when piped) or ndjson (one item per line).
    #[arg(long, env = "MALLARD_FORMAT", value_enum, global = true)]
    format: Option<Format>,
    /// Same as --format json.
    #[arg(long, global = true)]
    json: bool,
    /// Keep only these top-level fields, comma separated (e.g. id,title,count).
    #[arg(long, global = true, value_name = "A,B")]
    fields: Option<String>,
    /// Include raw payloads: the Sentry event payload, log resource attributes.
    #[arg(long, global = true)]
    raw: bool,
    /// Colour: auto (a terminal without NO_COLOR), always or never.
    #[arg(long, global = true, value_enum, default_value_t)]
    color: ColorChoice,
    /// No notes on stderr (truncation, progress, fallbacks); data, warnings
    /// and errors still print. (No `-q`: it is `--query` on several commands.)
    #[arg(long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Health of both sides and what the server allows; exit code 6 when a side is down.
    Status,
    /// Create a project if it does not exist; print its DSN and the OTLP env vars.
    Setup(setup::SetupArgs),
    /// What is this id (trace, event or issue)? Prints matches and the command to see more.
    Find {
        /// A trace or event id (32 hex) or an issue id (16 hex).
        id: String,
        /// Where to look for traces and events (default 7d).
        #[command(flatten)]
        window: WindowArgs,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Error issues: list, show, resolve, ignore, unresolve.
    #[command(subcommand)]
    Issues(sdl_cli::IssuesCmd),
    /// Error events: list, show.
    #[command(subcommand)]
    Events(sdl_cli::EventsCmd),
    /// Sentry projects: list, show, create.
    #[command(subcommand)]
    Projects(sdl_cli::ProjectsCmd),
    /// Releases seen in error events: first/last seen, events, issues, new issues.
    #[command(subcommand)]
    Releases(sdl_cli::ReleasesCmd),
    /// Traces: list (search), show (spans, errors and logs of one trace).
    #[command(subcommand)]
    Traces(TracesCmd),
    /// Everything about one trace: spans, errors and logs (= traces show).
    Trace(TraceShowArgs),
    /// Search, follow or show logs around a time (default window 1h).
    Logs(odl_cli::LogArgs),
    /// Services with request rate, error rate and latency (default window 1h).
    Services(odl_cli::ServiceArgs),
    /// Operations (span names) with request rate, error rate and latency; --compare 1d for before/after.
    Ops(odl_cli::OpsArgs),
    /// Counts per time bucket: spans (with p95), logs, Sentry errors (default window 1h).
    #[command(subcommand)]
    Series(series::SeriesCmd),
    /// Metrics: list, show (values over time).
    #[command(subcommand)]
    Metrics(MetricsCmd),
    /// Read-only SQL over the telemetry tables, or the error tables with --errors (the server must enable it).
    Sql(SqlCmd),
    /// Uploaded sourcemap bundles.
    #[command(subcommand)]
    Sourcemaps(sdl_cli::SourcemapsCmd),
    /// Send a test error to a DSN (checks the Sentry SDK setup end to end).
    SendTest {
        /// DSN to send to (from `mallard setup`).
        #[arg(long, env = "SENTRY_DSN")]
        dsn: String,
        /// Error message of the test event.
        #[arg(long, default_value = "send-test: hello from the mallard CLI")]
        message: String,
    },
    /// Terminal UI: F1 errors, F2 telemetry.
    Tui,
    /// Help for a command, or `mallard help agents`: the guide for AI agents.
    Help {
        /// A command (e.g. `issues list`) or `agents`.
        topic: Vec<String>,
    },
    /// The Sentry-side commands under their old name (also `sentry`).
    #[command(hide = true, alias = "sentry")]
    Errors {
        #[command(subcommand)]
        cmd: sdl_cli::Command,
    },
    /// The OpenTelemetry-side commands under their old name (also `otel`).
    #[command(hide = true, alias = "otel")]
    Telemetry {
        #[command(subcommand)]
        cmd: odl_cli::Command,
    },
}

#[derive(Subcommand)]
enum TracesCmd {
    /// Search traces (default window 1h), with the Sentry error count of each.
    List(odl_cli::TraceListArgs),
    /// Spans, Sentry errors and logs of one trace.
    Show(TraceShowArgs),
}

#[derive(clap::Args)]
struct TraceShowArgs {
    /// Trace id (32 hex characters).
    trace_id: String,
    /// Where to look (default 7d).
    #[command(flatten)]
    window: WindowArgs,
    #[command(flatten)]
    wait: WaitArgs,
}

/// `sql`: one command for both sides' tables (ADR 0011).
#[derive(clap::Args)]
struct SqlCmd {
    #[command(flatten)]
    sql: odl_cli::SqlArgs,
    /// Query the error tables (events, issues, issue_activity) instead of the
    /// telemetry tables (spans, logs, metric_*).
    #[arg(long)]
    errors: bool,
}

#[derive(Subcommand)]
enum MetricsCmd {
    /// Metric names, types and series counts (default window 1h).
    List(odl_cli::MetricListArgs),
    /// One metric over time: aggregate, group by attributes.
    Show(odl_cli::MetricArgs),
}

/// Hints in messages name mallard's commands and env vars.
const ERRORS_HINTS: Hints =
    Hints { command: "mallard", url_env: "MALLARD_URL", token_env: "MALLARD_TOKEN", project_env: "MALLARD_PROJECT" };

/// The same for the telemetry side.
const TELEMETRY_HINTS: Hints = ERRORS_HINTS;

/// What every command gets: the client and how to print.
struct Run {
    client: Client,
    project: Option<String>,
    output: Output,
}

impl Run {
    fn sentry(&self) -> sdl_cli::Ctx {
        sdl_cli::Ctx::new(self.client.sentry.clone())
            .with_project(self.project.clone())
            .with_output(self.output.clone())
            .with_hints(ERRORS_HINTS)
    }

    /// Trace lists get the Sentry error count per trace.
    fn otel(&self) -> odl_cli::Ctx {
        odl_cli::Ctx::new(self.client.otel.clone())
            .with_output(self.output.clone())
            .with_hints(TELEMETRY_HINTS)
            .with_error_lookup(std::sync::Arc::new(errors::SentryErrors(self.client.clone())))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match parse(std::env::args_os()) {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    let (color, json) = (cli.color, Format::pick(cli.format, cli.json).is_json());
    mallard_term::error::report_as(run(cli).await, color, json)
}

/// Parse the arguments. Usage errors exit 2; with JSON output asked for
/// (`--json`, `--format json|ndjson`, `MALLARD_FORMAT`) they are one JSON
/// line on stderr, like every other error.
fn parse<I: IntoIterator<Item = std::ffi::OsString>>(args: I) -> Result<Cli, ExitCode> {
    let args: Vec<std::ffi::OsString> = args.into_iter().collect();
    let res = help::command().try_get_matches_from(&args).and_then(|m| Cli::from_arg_matches(&m));
    match res {
        Ok(cli) => Ok(cli),
        // --help and --version.
        Err(e) if !e.use_stderr() => {
            let _ = e.print();
            Err(ExitCode::SUCCESS)
        }
        Err(e) if wants_json(&args) => {
            let text = e.to_string();
            let first = text.lines().next().unwrap_or("").trim_start_matches("error: ");
            let d = Described::new(Kind::Usage, first).hint("see mallard --help");
            eprintln!("{}", mallard_term::error::error_line(&d, ColorChoice::Never, true));
            Err(ExitCode::from(Kind::Usage.exit_code()))
        }
        Err(e) => {
            let _ = e.print();
            Err(ExitCode::from(Kind::Usage.exit_code()))
        }
    }
}

/// Whether the raw arguments (or `MALLARD_FORMAT`) ask for JSON output.
fn wants_json(args: &[std::ffi::OsString]) -> bool {
    let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let is_json = |v: &str| v == "json" || v == "ndjson";
    args.iter().any(|a| a == "--json" || a.strip_prefix("--format=").is_some_and(is_json))
        || args.windows(2).any(|w| w[0] == "--format" && is_json(&w[1]))
        || std::env::var("MALLARD_FORMAT").is_ok_and(|v| is_json(&v))
}

async fn run(cli: Cli) -> Result<()> {
    let client = Client::new(&cli.url, cli.token.as_deref()).map_err(|e| described(e, &cli.url))?;
    let output = Output::new(Format::pick(cli.format, cli.json), cli.color)
        .with_fields(cli.fields.as_deref())
        .with_raw(cli.raw)
        .with_quiet(cli.quiet);
    let r = Run { client, project: cli.project, output };
    match cli.cmd {
        Cmd::Status => status::run(&r).await,
        Cmd::Setup(a) => setup::run(&r, a).await,
        Cmd::Find { id, window, wait } => find::run(&r, &id, &window, &wait).await,
        Cmd::Issues(c) => sdl_cli::run(sdl_cli::Command::Issues(c), &r.sentry()).await,
        Cmd::Events(c) => sdl_cli::run(sdl_cli::Command::Events(c), &r.sentry()).await,
        Cmd::Projects(c) => sdl_cli::run(sdl_cli::Command::Projects(c), &r.sentry()).await,
        Cmd::Releases(c) => sdl_cli::run(sdl_cli::Command::Releases(c), &r.sentry()).await,
        Cmd::Sourcemaps(c) => sdl_cli::run(sdl_cli::Command::Sourcemaps(c), &r.sentry()).await,
        Cmd::SendTest { dsn, message } => sdl_cli::run(sdl_cli::Command::SendTest { dsn, message }, &r.sentry()).await,
        Cmd::Traces(TracesCmd::List(a)) => odl_cli::run(odl_cli::Command::Traces(a), &r.otel()).await,
        Cmd::Traces(TracesCmd::Show(a)) | Cmd::Trace(a) => trace::run(&r, &a.trace_id, &a.window, &a.wait).await,
        Cmd::Logs(a) => odl_cli::run(odl_cli::Command::Logs(a), &r.otel()).await,
        Cmd::Services(a) => odl_cli::run(odl_cli::Command::Services(a), &r.otel()).await,
        Cmd::Ops(a) => odl_cli::run(odl_cli::Command::Ops(a), &r.otel()).await,
        Cmd::Series(c) => series::run(&r, c).await,
        Cmd::Metrics(MetricsCmd::List(a)) => odl_cli::run(odl_cli::Command::Metrics(a), &r.otel()).await,
        Cmd::Metrics(MetricsCmd::Show(a)) => odl_cli::run(odl_cli::Command::Metric(a), &r.otel()).await,
        Cmd::Sql(SqlCmd { sql, errors: true }) => {
            let a = sdl_cli::SqlArgs { query: sql.query, limit: sql.limit };
            sdl_cli::run(sdl_cli::Command::Sql(a), &r.sentry()).await
        }
        Cmd::Sql(SqlCmd { sql, errors: false }) => odl_cli::run(odl_cli::Command::Sql(sql), &r.otel()).await,
        Cmd::Tui => tui::run(&r.client, r.project.clone()).await,
        Cmd::Help { topic } => help::run(&topic, &r.output),
        Cmd::Errors { cmd } => sdl_cli::run(cmd, &r.sentry()).await,
        Cmd::Telemetry { cmd } => odl_cli::run(cmd, &r.otel()).await,
    }
}

/// What went wrong with a mallard-client call, worded like the side's own
/// commands do it.
fn classify(e: &mallard_client::Error, base_url: &str) -> Described {
    let base_url = base_url.trim_end_matches('/');
    match e {
        mallard_client::Error::Sentry(e) => sdl_cli::classify(e, &format!("{base_url}/sentry"), &ERRORS_HINTS),
        mallard_client::Error::Otel(e) => odl_cli::classify(e, &format!("{base_url}/otel"), &TELEMETRY_HINTS),
        mallard_client::Error::InvalidTraceId(_) => Described::new(Kind::Usage, e.to_string()),
        e => Described::new(Kind::Internal, e.to_string()),
    }
}

/// A mallard-client error with its message and kind attached.
fn described(e: mallard_client::Error, base_url: &str) -> anyhow::Error {
    let d = classify(&e, base_url);
    anyhow::Error::new(e).context(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Cli {
        try_parse(args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let args: Vec<&str> = std::iter::once("mallard").chain(args.iter().copied()).collect();
        help::command().try_get_matches_from(args).and_then(|m| Cli::from_arg_matches(&m))
    }

    #[test]
    fn cli_definition_is_valid() {
        help::command().debug_assert();
    }

    #[test]
    fn the_flat_tree() {
        for args in [
            &["status"][..],
            &["setup", "checkout", "--platform", "node"],
            &["find", "0af7651916cd43dd8448eb211c80319c"],
            &["issues", "list", "--new", "--first-release", "web@1", "--since", "1h"],
            &["issues", "list", "--from", "2h", "--to", "1h", "--sort", "count", "-n", "10"],
            &["issues", "show", "85fede14250ec6be", "--since", "1h"],
            &["issues", "resolve", "85fede14250ec6be", "--reason", "fixed in #123"],
            &["issues", "ignore", "a", "b"],
            &["events", "list", "--issue", "85fede14250ec6be", "--since", "1d"],
            &["events", "show", "0af7651916cd43dd8448eb211c80319c", "--raw"],
            &["issues", "list", "--tag", "url=/checkout", "--tag", "user=u-42", "--cursor", "c"],
            &["events", "list", "--since", "1h", "--tag", "user=u-42", "--release", "web@1", "--env", "prod"],
            &["events", "list", "-q", "timeout", "--cursor", "c", "-n", "5"],
            &["events", "list", "--since", "1h", "--tag", "user=u-42", "--cursor", "c", "--wait", "30s"],
            &["releases", "list", "--since", "30d", "-n", "5"],
            &["sql", "--errors", "SELECT count(*) FROM events"],
            &["projects", "create", "web", "--if-not-exists"],
            &["traces", "list", "-S", "checkout", "--errors", "--since", "15m"],
            &["traces", "show", "0af7651916cd43dd8448eb211c80319c", "--from", "1d"],
            &["trace", "0af7651916cd43dd8448eb211c80319c"],
            &["logs", "-S", "checkout", "-l", "warn", "--since", "10m"],
            &["logs", "--around", "2026-11-01T10:15:03Z", "--window", "2m"],
            &["logs", "-f"],
            &["services", "--since", "1h"],
            &["ops", "-S", "checkout"],
            &["metrics", "list", "-q", "http"],
            &["metrics", "show", "http.server.duration", "-g", "p95", "-b", "http.route"],
            &["sql", "SELECT 1"],
            &["sourcemaps", "list"],
            &["send-test", "--dsn", "http://k@h/1"],
            &["tui"],
            &["help", "agents"],
            &["help", "issues", "list"],
            &["events", "list", "--trace", "0af7651916cd43dd8448eb211c80319c", "--wait", "30s"],
            &["traces", "list", "--wait", "1m"],
            &["traces", "show", "0af7651916cd43dd8448eb211c80319c", "--wait", "30s"],
            &["trace", "0af7651916cd43dd8448eb211c80319c", "--wait", "30s"],
            &["find", "85fede14250ec6be", "--wait", "10s"],
            &["logs", "-t", "0af7651916cd43dd8448eb211c80319c", "--wait", "30s"],
            &["logs", "-f", "--for", "2m", "--until-match", "ready"],
            &["ops", "-S", "checkout", "--since", "1h", "--compare", "1d"],
            &["series", "spans", "-S", "checkout", "-O", "POST /pay", "--since", "6h", "--step", "5m"],
            &["series", "logs", "-S", "checkout", "-q", "timeout"],
            &["series", "errors", "--since", "1d", "--step", "1h"],
            &["setup", "checkout", "--sentry-cli"],
            &["--quiet", "issues", "list"],
        ] {
            if let Err(e) = try_parse(args) {
                panic!("{args:?}: {e}");
            }
        }
    }

    #[test]
    fn old_paths_are_hidden_aliases_of_the_same_commands() {
        for args in [
            &["errors", "issues", "list", "-s", "1h"][..],
            &["sentry", "events", "list", "-t", "abc", "-n", "3"],
            &["errors", "stats", "--since", "1h"],
            &["errors", "maintenance"],
            &["errors", "projects", "create", "web"],
            &["telemetry", "traces", "-n", "5"],
            &["otel", "logs", "-f"],
            &["telemetry", "trace", "0af7651916cd43dd8448eb211c80319c"],
            &["telemetry", "metric", "x", "-g", "p95"],
            &["telemetry", "health"],
        ] {
            if let Err(e) = try_parse(args) {
                panic!("{args:?}: {e}");
            }
        }
        let help = help::command().render_help().to_string();
        assert!(!help.contains("  errors") && !help.contains("  telemetry"), "hidden: {help}");
        assert!(help.contains("issues") && help.contains("logs"), "{help}");
    }

    #[test]
    fn project_and_output_flags_are_global() {
        for args in [
            &["-p", "web", "issues", "list"][..],
            &["issues", "list", "-p", "web"],
            &["errors", "issues", "list", "--project", "web"],
        ] {
            assert_eq!(parse_ok(args).project.as_deref(), Some("web"), "{args:?}");
        }
        let cli = parse_ok(&["traces", "list", "-n", "5", "--json", "--color", "never", "--fields", "trace_id"]);
        assert!(cli.json && cli.color == ColorChoice::Never && cli.fields.as_deref() == Some("trace_id"));
        let cli = parse_ok(&["logs", "--format", "ndjson", "--raw"]);
        assert_eq!((cli.format, cli.raw), (Some(Format::Ndjson), true));
        assert!(try_parse(&["logs", "--format", "yaml"]).is_err());
    }

    #[test]
    fn tags_cursor_and_sql_side() {
        let Cmd::Issues(sdl_cli::IssuesCmd::List(a)) = parse_ok(&["issues", "list", "--tag", "url=/a=b"]).cmd else {
            panic!("issues list")
        };
        assert_eq!(a.tag, ["url:/a=b"], "split at the first =; sent as key:value");
        for bad in [
            &["issues", "list", "--tag", "novalue"][..],
            &["issues", "list", "--tag", "=v"],
            &["issues", "list", "--tag", "a:b=c"],
        ] {
            assert!(try_parse(bad).is_err(), "{bad:?}");
        }
        assert!(try_parse(&["events", "list", "--trace", "0af7651916cd43dd8448eb211c80319c", "--tag", "a=b"]).is_err());
        let Cmd::Sql(s) = parse_ok(&["sql", "--errors", "SELECT 1", "-n", "5"]).cmd else { panic!("sql") };
        assert!(s.errors && s.sql.limit == 5 && s.sql.query == "SELECT 1");
        let Cmd::Sql(s) = parse_ok(&["sql", "SELECT 1"]).cmd else { panic!("sql") };
        assert!(!s.errors, "telemetry by default");
    }

    #[test]
    fn wait_and_follow_flags_are_checked() {
        for args in [
            &["traces", "list", "--wait", "soon"][..],
            &["logs", "-f", "--wait", "10s"],
            &["logs", "--for", "1m"],
            &["logs", "--until-match", "x"],
            &["ops", "--compare", "0s"],
            &["-q", "issues", "list"],
        ] {
            assert!(try_parse(args).is_err(), "{args:?}");
        }
        assert!(parse_ok(&["status", "--quiet"]).quiet);
    }

    #[test]
    fn json_is_asked_for() {
        let a = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(wants_json(&a(&["mallard", "x", "--json"])));
        assert!(wants_json(&a(&["mallard", "--format", "ndjson", "x"])));
        assert!(wants_json(&a(&["mallard", "--format=json"])));
    }

    /// Both sides fail with one line in the same style, naming mallard's
    /// env vars (review 2.M), and the "unavailable" exit code.
    #[tokio::test]
    async fn both_sides_report_errors_alike() {
        let mut lines = vec![];
        for args in [
            &["--url", "http://127.0.0.1:9", "issues", "list", "-p", "web"][..],
            &["--url", "http://127.0.0.1:9", "services"],
            &["--url", "http://127.0.0.1:9", "trace", "0af7651916cd43dd8448eb211c80319c"],
            &["--url", "http://127.0.0.1:9", "errors", "-p", "web", "issues", "list"],
        ] {
            let res = run(parse_ok(args)).await;
            assert_eq!(mallard_term::error::exit_code(&res), 6, "{args:?}");
            let msg = mallard_term::error::message(&res.unwrap_err());
            assert!(msg.contains("cannot reach server at http://127.0.0.1:9/"), "{args:?}: {msg}");
            assert!(msg.contains("set MALLARD_URL"), "{args:?}: {msg}");
            assert!(!msg.contains('\n'), "{msg}");
            lines.push(msg);
        }
        assert!(lines[0].starts_with("cannot reach server at http://127.0.0.1:9/sentry"), "{}", lines[0]);
        assert!(lines[1].starts_with("cannot reach server at http://127.0.0.1:9/otel"), "{}", lines[1]);
    }

    #[tokio::test]
    async fn bad_ids_are_usage_errors() {
        let res = run(parse_ok(&["--url", "http://127.0.0.1:9", "trace", "nope"])).await;
        assert_eq!(mallard_term::error::exit_code(&res), 2);
        let res = run(parse_ok(&["--url", "http://127.0.0.1:9", "find", "not-an-id"])).await;
        assert_eq!(mallard_term::error::exit_code(&res), 2);
    }
}
