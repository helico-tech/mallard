//! `odl` command line and terminal UI, usable as a library.
//!
//! - [`Command`] + [`run`]: mount the commands in another CLI (global flags
//!   such as the URL and token live in the host and arrive via [`Ctx`]).
//! - [`Embedded`]: the TUI as a component without terminal setup.
//!
//! The same shape as `sdl_cli`: `Command`, `Ctx::new(client).with_json(…)`,
//! `describe`, `HINTS`, `Embedded::open_trace`.

mod compare;
mod errors;
mod fmt;
mod severity;
mod tui;
mod waterfall;

pub use errors::{ErrorEvent, ErrorLookup, ErrorMap, TraceErrors};
pub use mallard_term::error::Hints;
pub use tui::{Embedded, Outcome};

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use mallard_term::color::{ColorChoice, Out, alert};
use mallard_term::error::{Described, Failure, Kind, usage};
use mallard_term::output::{Format, List, Output};
use mallard_term::wait::{WaitArgs, until};
use mallard_term::window::{Window, WindowArgs};
use odl_client::{Client, LogTail, api::*};

use crate::fmt::{log_line, *};

/// Hints for the standalone `odl` binary.
pub const HINTS: Hints = Hints { command: "odl", url_env: "ODL_URL", token_env: "ODL_TOKEN", project_env: "" };

/// Default window of the telemetry commands.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(3600);

/// `services` (default window 1h).
#[derive(clap::Args, Debug, Clone)]
pub struct ServiceArgs {
    #[command(flatten)]
    pub w: WindowArgs,
    /// Count all spans, not only entry spans.
    #[arg(long)]
    pub all_spans: bool,
}

/// `ops` (default window 1h).
#[derive(clap::Args, Debug, Clone)]
pub struct OpsArgs {
    #[command(flatten)]
    pub w: WindowArgs,
    #[arg(short = 'S', long)]
    pub service: Option<String>,
    #[arg(long)]
    pub all_spans: bool,
    /// Also the same window this long earlier (e.g. 1d): p95 and error rate before, and the change.
    #[arg(long, value_name = "DUR", value_parser = mallard_term::window::parse_since)]
    pub compare: Option<String>,
}

/// `traces` (default window 1h).
#[derive(clap::Args, Debug, Clone)]
pub struct TraceListArgs {
    #[command(flatten)]
    pub w: WindowArgs,
    #[arg(short = 'S', long)]
    pub service: Option<String>,
    /// Span name.
    #[arg(short = 'O', long)]
    pub operation: Option<String>,
    /// Only traces with an error span.
    #[arg(short, long)]
    pub errors: bool,
    /// Minimum span duration in ms.
    #[arg(long)]
    pub min_ms: Option<f64>,
    #[arg(long)]
    pub max_ms: Option<f64>,
    /// Attribute filter key=value (repeatable).
    #[arg(short, long)]
    pub attr: Vec<String>,
    /// Max traces (`-l` still works).
    #[arg(short = 'n', short_alias = 'l', long, default_value_t = 30)]
    pub limit: u32,
    #[command(flatten)]
    pub wait: WaitArgs,
}

/// `logs` (default window 1h).
#[derive(clap::Args, Debug, Clone)]
pub struct LogArgs {
    #[command(flatten)]
    pub w: WindowArgs,
    #[arg(short = 'S', long)]
    pub service: Option<String>,
    /// Minimum severity: trace, debug, info, warn, error, fatal or 1-24.
    #[arg(short = 'l', long)]
    pub level: Option<String>,
    /// Substring to find in the body (case-insensitive).
    #[arg(short, long)]
    pub query: Option<String>,
    #[arg(short, long)]
    pub trace: Option<String>,
    #[arg(short, long)]
    pub attr: Vec<String>,
    #[arg(short = 'n', long, default_value_t = 100)]
    pub limit: u32,
    /// Keep printing new records (from --from/--since on, until Ctrl-C, --for or --until-match).
    #[arg(short, long, conflicts_with_all = ["to", "around", "wait"])]
    pub follow: bool,
    /// With -f: stop after this long, e.g. 2m (exit 0; exit 8 with --until-match and no match).
    #[arg(long = "for", value_name = "DUR", value_parser = mallard_term::window::parse_since, requires = "follow")]
    pub for_: Option<String>,
    /// With -f: stop after the first record whose body contains this text (case-insensitive).
    #[arg(long, value_name = "TEXT", requires = "follow")]
    pub until_match: Option<String>,
    /// Logs around this time (RFC 3339 or a duration ago), oldest first:
    /// from --around minus --window to --around plus --window.
    #[arg(long, value_parser = mallard_term::window::parse_time, conflicts_with_all = ["since", "from", "to"])]
    pub around: Option<chrono::DateTime<chrono::Utc>>,
    /// How far before and after --around, e.g. 30s, 2m.
    #[arg(long, default_value = "1m", value_parser = mallard_term::window::parse_since, requires = "around")]
    pub window: String,
    #[command(flatten)]
    pub wait: WaitArgs,
}

/// `metrics` (default window 1h).
#[derive(clap::Args, Debug, Clone)]
pub struct MetricListArgs {
    #[command(flatten)]
    pub w: WindowArgs,
    #[arg(short = 'S', long)]
    pub service: Option<String>,
    /// Name substring.
    #[arg(short, long)]
    pub query: Option<String>,
}

/// `metric <name>` (default window 1h).
#[derive(clap::Args, Debug, Clone)]
pub struct MetricArgs {
    pub name: String,
    #[command(flatten)]
    pub w: WindowArgs,
    #[arg(short = 'S', long)]
    pub service: Option<String>,
    /// avg, min, max, sum, count, rate, increase, p50, p95, p99 ...
    #[arg(short = 'g', long)]
    pub agg: Option<String>,
    /// Group by attribute keys (comma separated).
    #[arg(short, long)]
    pub by: Option<String>,
    #[arg(short, long)]
    pub attr: Vec<String>,
    /// Bucket width, e.g. 1m.
    #[arg(long)]
    pub step: Option<String>,
}

/// `sql`.
#[derive(clap::Args, Debug, Clone)]
pub struct SqlArgs {
    pub query: String,
    #[arg(short = 'n', long, default_value_t = 100)]
    pub limit: u32,
}

/// All `odl` commands. Embedders can mount this as a subcommand, or single
/// commands through their `*Args`.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum Command {
    /// Check that the server is up (no token needed); exit code 6 if not.
    Health,
    /// Server health, ingest counters and storage size.
    Status,
    /// Services with request rate, error rate and latency.
    Services(ServiceArgs),
    /// Operations (span names) with RED metrics.
    Ops(OpsArgs),
    /// Search traces.
    Traces(TraceListArgs),
    /// Show one trace as a waterfall.
    Trace {
        trace_id: String,
        /// Look-back window for the lookup.
        #[arg(short, long, default_value = "7d", value_parser = check_window)]
        since: String,
        /// Show span attributes.
        #[arg(short = 'A', long)]
        attributes: bool,
    },
    /// Search or follow logs.
    Logs(LogArgs),
    /// List metrics.
    Metrics(MetricListArgs),
    /// Query a metric over time.
    Metric(MetricArgs),
    /// Run a read-only SQL query (server must enable it).
    Sql(SqlArgs),
    /// Interactive terminal UI.
    Tui {
        /// Initial time window.
        #[arg(short, long, default_value = "1h", value_parser = check_window)]
        since: String,
    },
}

/// Parse a time window such as `15m`, `2d` or `1h30m`, as the server does.
pub(crate) fn parse_window(s: &str) -> Result<Duration> {
    let d = humantime::parse_duration(s.trim()).map_err(|e| anyhow::anyhow!("invalid time window {s:?}: {e}"))?;
    if d.is_zero() {
        bail!("invalid time window {s:?}: must be longer than zero");
    }
    Ok(d)
}

/// clap value parser: a valid window, kept as typed (the server parses it).
fn check_window(s: &str) -> Result<String> {
    parse_window(s).map(|_| s.trim().to_string())
}

/// The window as the API takes it (`from`, `to`, `since`): always
/// absolute, so the JSON output can say which window it covered.
type ApiWindow = (Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<String>);

fn window(w: &WindowArgs) -> Result<(Window, ApiWindow)> {
    let r = w.resolve(DEFAULT_WINDOW)?;
    Ok((r, (Some(r.from), Some(r.to), None)))
}

/// Attribute filters `k=v` as the API takes them: comma separated.
pub(crate) fn attrs(a: &[String]) -> Option<String> {
    (!a.is_empty()).then(|| a.join(","))
}

/// Print `v` as the output format says, or as the text `text` makes of it.
fn print<T: serde::Serialize + ?Sized>(ctx: &Ctx, v: &T, text: impl FnOnce(&T) -> String) -> Result<()> {
    if ctx.output.text() {
        ctx.out.line(&text(v))?;
        Ok(())
    } else {
        ctx.output.object(v)
    }
}

/// Print a list in the envelope, or as the text `text` makes of the items.
fn print_list<T: serde::Serialize>(ctx: &Ctx, l: List<T>, text: impl FnOnce(&[T]) -> String) -> Result<()> {
    if ctx.output.text() {
        ctx.out.line(&text(&l.items))?;
        ctx.output.truncation_note(&l);
        Ok(())
    } else {
        ctx.output.list(&l)
    }
}

/// What the commands need besides their own arguments:
/// `Ctx::new(client).with_output(o)`. The same builder as `sdl_cli::Ctx`.
#[derive(Debug)]
pub struct Ctx {
    client: Client,
    output: Output,
    out: Out,
    hints: Hints,
    errors: Option<Arc<dyn ErrorLookup>>,
}

impl Ctx {
    /// Text output, colour as `--color=auto` says, [`HINTS`].
    pub fn new(client: Client) -> Self {
        let output = Output::default();
        Ctx { client, out: output.out, output, hints: HINTS, errors: None }
    }

    /// Print JSON instead of tables (`--json`).
    pub fn with_json(mut self, json: bool) -> Self {
        if json {
            self.output.format = Format::Json;
        }
        self
    }

    /// Format, colour, `--fields` and `--raw` at once.
    pub fn with_output(mut self, output: Output) -> Self {
        self.out = output.out;
        self.output = output;
        self
    }

    /// The `--color` flag (for stdout).
    pub fn with_color(mut self, color: ColorChoice) -> Self {
        self.out = Out::stdout(color);
        self.output.out = self.out;
        self
    }

    pub fn with_hints(mut self, hints: Hints) -> Self {
        self.hints = hints;
        self
    }

    /// Error events from the host's error tracker: `traces` gets a
    /// `sentry_errors` count per trace (text: a `sentry` column).
    pub fn with_error_lookup(mut self, lookup: Arc<dyn ErrorLookup>) -> Self {
        self.errors = Some(lookup);
        self
    }

    /// The friendly message, with the client error kept as its source.
    fn err(&self, e: odl_client::Error) -> anyhow::Error {
        let msg = classify(&e, self.client.base_url(), &self.hints);
        anyhow::Error::new(e).context(msg)
    }
}

/// One-line, actionable message for a client error. Safe to print.
pub fn describe(e: &odl_client::Error, url: &str, hints: &Hints) -> String {
    classify(e, url, hints).to_string()
}

/// [`describe`] with the [`Kind`] (exit code) and the hint apart.
pub fn classify(e: &odl_client::Error, url: &str, hints: &Hints) -> Described {
    use odl_client::Error as E;
    let f = match e {
        E::Api { status, code, message } => Failure::Status { status: *status, code, message },
        E::InvalidUrl(m) => Failure::InvalidUrl(m),
        E::InvalidTraceId(_) => return Described::new(Kind::Usage, e.to_string()),
        E::Http(h) if h.is_connect() => Failure::Connect,
        E::Http(h) if h.is_timeout() => Failure::Timeout,
        E::Http(h) if h.is_decode() => Failure::Decode(h.to_string()),
        E::Http(h) => Failure::Other(h.to_string()),
        e => Failure::Other(e.to_string()),
    };
    mallard_term::error::classify(f, url, hints)
}

/// Run one command. Client errors become one friendly line ([`describe`]).
///
/// Output errors are returned too: a closed pipe (`odl logs | head`) is an
/// [`std::io::Error`] of kind `BrokenPipe` in the chain, which
/// `mallard_term::error::report` treats as success.
pub async fn run(cmd: Command, ctx: &Ctx) -> Result<()> {
    run_inner(cmd, ctx).await.map_err(|e| match e.downcast::<odl_client::Error>() {
        Ok(e) => ctx.err(e),
        Err(e) => e,
    })
}

async fn run_inner(cmd: Command, ctx: &Ctx) -> Result<()> {
    let c = &ctx.client;
    match cmd {
        Command::Health => {
            let h = c.health().await?;
            print(ctx, &h, |h| format!("{} {}", clean(&h.status), clean(&h.version)))
        }
        Command::Status => {
            let s = c.stats().await?;
            print(ctx, &s, |s| stats_text(s, c.base_url()))
        }
        Command::Services(ServiceArgs { w, all_spans }) => {
            let (win, (from, to, since)) = window(&w)?;
            let v = c.services(&ServiceQuery { from, to, since, entry_only: Some(!all_spans) }).await?;
            print_list(ctx, List::new(v).window(win), services_text)
        }
        Command::Ops(OpsArgs { w, service, all_spans, compare: None }) => {
            let (win, (from, to, since)) = window(&w)?;
            let v = c.operations(&OperationQuery { from, to, since, service, entry_only: Some(!all_spans) }).await?;
            print_list(ctx, List::new(v).window(win), operations_text)
        }
        Command::Ops(OpsArgs { w, service, all_spans, compare: Some(shift) }) => {
            let win = w.resolve(DEFAULT_WINDOW)?;
            let back = chrono::TimeDelta::from_std(mallard_term::window::parse_duration(&shift).map_err(usage)?)?;
            let q = |from, to| OperationQuery {
                from: Some(from),
                to: Some(to),
                since: None,
                service: service.clone(),
                entry_only: Some(!all_spans),
            };
            let (q_now, q_before) = (q(win.from, win.to), q(win.from - back, win.to - back));
            let (now, before) = tokio::join!(c.operations(&q_now), c.operations(&q_before));
            let v = compare::join(now?, before?);
            print_list(ctx, List::new(v).window(win), |v| compare::compare_text(v, &shift))
        }
        Command::Traces(a) => {
            // The window is resolved on every try: with --wait it moves along.
            let fetch = || async {
                let (win, (from, to, since)) = window(&a.w)?;
                let q = TraceQuery {
                    from,
                    to,
                    since,
                    service: a.service.clone(),
                    operation: a.operation.clone(),
                    errors_only: Some(a.errors),
                    min_duration_ms: a.min_ms,
                    max_duration_ms: a.max_ms,
                    attributes: attrs(&a.attr),
                    limit: Some(a.limit),
                };
                anyhow::Ok((win, c.traces(&q).await?))
            };
            let ((win, v), waited) = until(&ctx.output, &a.wait, fetch, |(_, v)| !v.is_empty()).await?.split();
            let list = List::new(v).limit(Some(a.limit)).window(win);
            let sentry = match &ctx.errors {
                Some(l) => Some(sentry_counts(l.as_ref(), &list.items, win).await),
                None => None,
            };
            if ctx.output.text() {
                ctx.out.line(&traces_text(&list.items, sentry.as_deref()))?;
                ctx.output.truncation_note(&list);
                return waited;
            }
            let mut items = Vec::with_capacity(list.items.len());
            for (i, t) in list.items.iter().enumerate() {
                let mut item = serde_json::to_value(t)?;
                if let Some(s) = &sentry {
                    item["sentry_errors"] = serde_json::to_value(s[i])?;
                }
                items.push(item);
            }
            let List { count, limit, truncated, window, .. } = list;
            ctx.output.list(&List { items, count, limit, truncated, next_cursor: None, window })?;
            waited
        }
        Command::Trace { trace_id, since, attributes } => {
            let id = &trace_id;
            let get = |since: String| async move {
                c.trace(id, &TraceGetQuery { since: Some(since), ..Default::default() }).await
            };
            let t = match get(since.clone()).await {
                // The server's query.max_window is smaller than the window
                // asked for: fall back to one day.
                Err(e) if e.is_window_too_large() && since != "24h" => {
                    ctx.output
                        .note(format!("--since {since} is larger than the server allows; searching the last 24h"));
                    get("24h".into()).await?
                }
                r => r?,
            };
            print(ctx, &t, |t| trace_lines(t, attributes).join("\n"))
        }
        Command::Logs(a) => logs(ctx, a).await,
        Command::Metrics(MetricListArgs { w, service, query }) => {
            let (win, (from, to, since)) = window(&w)?;
            let v = c.metrics(&MetricListQuery { from, to, since, service, q: query }).await?;
            print_list(ctx, List::new(v).window(win), metrics_text)
        }
        Command::Metric(MetricArgs { name, w, service, agg, by, attr, step }) => {
            let (_, (from, to, since)) = window(&w)?;
            let r = c
                .metric_query(&MetricQuery {
                    name,
                    from,
                    to,
                    since,
                    service,
                    agg,
                    group_by: by,
                    attributes: attrs(&attr),
                    step,
                })
                .await?;
            print(ctx, &r, metric_text)
        }
        Command::Sql(SqlArgs { query, limit }) => {
            let r = c.sql(&query, Some(limit)).await?;
            print(ctx, &r, sql_text)
        }
        Command::Tui { since } => tui::run_standalone(c.clone(), &since).await,
    }
}

/// The error count of each trace from the host's lookup; `None` for every
/// trace when the lookup failed (with a warning on stderr).
async fn sentry_counts(l: &dyn ErrorLookup, traces: &[TraceSummary], w: Window) -> Vec<Option<u64>> {
    let ids = traces.iter().map(|t| t.trace_id.clone()).collect();
    match l.errors(ids, w.from, w.to).await {
        Ok(m) => traces.iter().map(|t| Some(m.get(&t.trace_id).map_or(0, |e| e.count))).collect(),
        Err(e) => {
            eprintln!("{} sentry errors: {}", alert("warning:"), clean(&e.to_string()));
            vec![None; traces.len()]
        }
    }
}

/// A log record as JSON: `resource_attributes` only with `--raw` (they
/// repeat on every record; the service name is already a field).
fn log_json(r: &LogRecord, raw: bool) -> Result<serde_json::Value> {
    let mut v = serde_json::to_value(r)?;
    if !raw && let Some(m) = v.as_object_mut() {
        m.remove("resource_attributes");
    }
    Ok(v)
}

async fn logs(ctx: &Ctx, a: LogArgs) -> Result<()> {
    let c = &ctx.client;
    let min_severity = a
        .level
        .as_deref()
        .map(|l| {
            severity::parse(l)
                .ok_or_else(|| usage(format!("unknown level {l:?} (trace, debug, info, warn, error, fatal or 1-24)")))
        })
        .transpose()?;
    let resolve = || -> Result<(Window, LogOrder)> {
        Ok(match a.around {
            Some(t) => {
                let d = chrono::TimeDelta::from_std(mallard_term::window::parse_duration(&a.window).map_err(usage)?)?;
                (Window { from: t - d, to: t + d }, LogOrder::Asc)
            }
            None => (a.w.resolve(DEFAULT_WINDOW)?, LogOrder::Desc),
        })
    };
    let query = |win: Window, order: LogOrder| LogQuery {
        from: Some(win.from),
        to: if a.follow { None } else { Some(win.to) },
        since: None,
        service: a.service.clone(),
        min_severity,
        q: a.query.clone(),
        trace_id: a.trace.clone(),
        attributes: attrs(&a.attr),
        limit: Some(a.limit),
        order: Some(order),
    };
    if !a.follow {
        // The window is resolved on every try: with --wait it moves along.
        let fetch = || async {
            let (win, order) = resolve()?;
            let mut v = c.logs(&query(win, order)).await?;
            if order == LogOrder::Desc {
                v.reverse(); // print oldest first, like a log file
            }
            anyhow::Ok((win, v))
        };
        let ((win, v), waited) = until(&ctx.output, &a.wait, fetch, |(_, v)| !v.is_empty()).await?.split();
        let list = List::new(v).limit(Some(a.limit)).window(win);
        if ctx.output.text() {
            if !list.items.is_empty() {
                ctx.out.line(&list.items.iter().map(log_line).collect::<Vec<_>>().join("\n"))?;
            }
            ctx.output.truncation_note(&list);
            return waited;
        }
        let items = list.items.iter().map(|r| log_json(r, ctx.output.raw)).collect::<Result<Vec<_>>>()?;
        let l = List {
            items,
            count: list.count,
            limit: list.limit,
            truncated: list.truncated,
            next_cursor: None,
            window: list.window,
        };
        ctx.output.list(&l)?;
        return waited;
    }
    let (win, order) = resolve()?;
    let q = query(win, order);
    // Start where --from/--since say; the first poll shows the newest
    // `--limit` records of that window, like `tail -n N -f`.
    let mut tail = LogTail::new(c.clone(), q, win.from);
    let stop = Stop::new(a.for_.as_deref(), a.until_match.as_deref())?;
    // One Ctrl-C future for the whole loop, so a signal that arrives
    // while a request is in flight is not lost. The same for --for.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let time_up = after(stop.for_.as_ref().map(|(_, d)| *d));
    tokio::pin!(time_up);
    loop {
        tokio::select! {
            res = tail.poll() => match res {
                Ok(v) => {
                    for r in &v {
                        if ctx.output.text() {
                            ctx.out.line(&log_line(r))?;
                        } else {
                            // A stream is NDJSON in json mode too.
                            ctx.output.record(&log_json(r, ctx.output.raw)?)?;
                        }
                        if stop.matches(r) {
                            return Ok(());
                        }
                    }
                }
                Err(e) => eprintln!("{} {}", alert("poll failed:"), describe(&e, c.base_url(), &ctx.hints)),
            },
            _ = &mut ctrl_c => return Ok(()),
            _ = &mut time_up => return stop.expired(),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = &mut ctrl_c => return Ok(()),
            _ = &mut time_up => return stop.expired(),
        }
    }
}

/// Resolves after `d`, or never.
async fn after(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// When `logs -f` stops by itself: `--for` and `--until-match`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stop {
    /// `--for`, as typed, and how long.
    for_: Option<(String, Duration)>,
    /// `--until-match`, lowercased.
    needle: Option<String>,
}

impl Stop {
    fn new(for_: Option<&str>, until_match: Option<&str>) -> Result<Stop> {
        let for_ = for_
            .map(|f| mallard_term::window::parse_duration(f).map(|d| (f.to_string(), d)).map_err(usage))
            .transpose()?;
        Ok(Stop { for_, needle: until_match.map(str::to_lowercase) })
    }

    /// The record has the `--until-match` text in its body.
    fn matches(&self, r: &LogRecord) -> bool {
        self.needle.as_ref().is_some_and(|n| r.body.to_lowercase().contains(n.as_str()))
    }

    /// `--for` is over: fine, unless a match was asked for (exit 8).
    fn expired(&self) -> Result<()> {
        match (&self.needle, &self.for_) {
            (Some(_), Some((f, _))) => Err(anyhow::Error::new(
                Described::new(
                    Kind::Unmet,
                    format!("no log matched {:?} within {f}", self.needle.as_deref().unwrap_or_default()),
                )
                .hint("raise --for, or check the text and filters"),
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: Command,
    }

    fn parse(args: &[&str]) -> std::result::Result<Command, clap::Error> {
        Cli::try_parse_from(std::iter::once("odl").chain(args.iter().copied())).map(|c| c.cmd)
    }

    // Review finding 14: `--to` would be ignored by `--follow`.
    #[test]
    fn follow_conflicts_with_to() {
        assert!(parse(&["logs", "--follow", "--to", "2026-01-01T00:00:00Z"]).is_err());
        assert!(parse(&["logs", "--follow", "--since", "5m"]).is_ok());
    }

    // Review finding 13: windows are checked up front, as the server parses them.
    #[test]
    fn windows_are_validated() {
        assert!(parse(&["services", "--since", "bogus"]).is_err());
        assert!(parse(&["tui", "--since", "0s"]).is_err());
        let Command::Services(a) = parse(&["services", "--since", "1h30m"]).unwrap() else { panic!() };
        let (w, (from, to, since)) = window(&a.w).unwrap();
        assert_eq!((w.to - w.from).num_seconds(), 5400);
        assert_eq!((from, to, since), (Some(w.from), Some(w.to), None), "always absolute");
        let Command::Services(a) = parse(&["services"]).unwrap() else { panic!() };
        let w = window(&a.w).unwrap().0;
        assert_eq!((w.to - w.from).to_std().unwrap(), DEFAULT_WINDOW);
    }

    #[test]
    fn logs_around_a_time() {
        assert!(parse(&["logs", "--around", "2026-01-01T00:00:00Z", "--window", "30s"]).is_ok());
        assert!(parse(&["logs", "--around", "10m", "--since", "1h"]).is_err(), "around or a window");
        assert!(parse(&["logs", "--around", "10m", "-f"]).is_err(), "around or follow");
        assert!(parse(&["logs", "--window", "30s"]).is_err(), "--window needs --around");
    }

    #[test]
    fn follow_stops_by_itself() {
        let Command::Logs(a) = parse(&["logs", "-f", "--for", "2m", "--until-match", "Ready"]).unwrap() else {
            panic!()
        };
        let stop = Stop::new(a.for_.as_deref(), a.until_match.as_deref()).unwrap();
        assert_eq!(stop.for_, Some(("2m".into(), Duration::from_secs(120))));
        let mut r = record();
        r.body = "server is READY now".into();
        assert!(stop.matches(&r), "case-insensitive substring of the body");
        assert!(!stop.matches(&record()));
        let e = stop.expired().unwrap_err();
        assert_eq!(mallard_term::error::exit_code(&Err(e)), 8, "--for over without a match");
        let only_for = Stop::new(Some("1s"), None).unwrap();
        assert!(only_for.expired().is_ok() && !only_for.matches(&r));
        assert!(parse(&["logs", "--for", "1m"]).is_err(), "--for needs -f");
        assert!(parse(&["logs", "-f", "--wait", "1m"]).is_err(), "-f or --wait");
    }

    fn record() -> LogRecord {
        LogRecord {
            time: Utc::now(),
            observed_time: Utc::now(),
            service_name: "checkout".into(),
            severity_number: 9,
            severity_text: "INFO".into(),
            body: "hi".into(),
            trace_id: None,
            span_id: None,
            event_name: String::new(),
            scope_name: String::new(),
            attributes: Default::default(),
            resource_attributes: Default::default(),
        }
    }

    #[test]
    fn log_json_drops_resource_attributes_unless_raw() {
        let r = LogRecord {
            time: Utc::now(),
            observed_time: Utc::now(),
            service_name: "checkout".into(),
            severity_number: 9,
            severity_text: "INFO".into(),
            body: "hi".into(),
            trace_id: None,
            span_id: None,
            event_name: String::new(),
            scope_name: String::new(),
            attributes: Default::default(),
            resource_attributes: Default::default(),
        };
        assert!(log_json(&r, false).unwrap().get("resource_attributes").is_none());
        assert!(log_json(&r, true).unwrap().get("resource_attributes").is_some());
    }
}
