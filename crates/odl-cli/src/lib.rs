//! `odl` command line and terminal UI, usable as a library.
//!
//! - [`Command`] + [`run`]: mount the commands in another CLI (global flags
//!   such as the URL and token live in the host and arrive via [`Ctx`]).
//! - [`Embedded`]: the TUI as a component without terminal setup.
//!
//! The same shape as `sdl_cli`: `Command`, `Ctx::new(client).with_json(…)`,
//! `describe`, `HINTS`, `Embedded::open_trace`.

mod fmt;
mod severity;
mod tui;
mod waterfall;

pub use mallard_term::error::Hints;
pub use tui::{Embedded, Outcome};

use std::time::Duration;

use anyhow::{Result, bail};
use clap::Subcommand;
use mallard_term::color::{ColorChoice, Out, alert};
use mallard_term::error::{Described, Failure};
use odl_client::{Client, LogTail, api::*};

use crate::fmt::{json_safe, log_line, *};

/// Hints for the standalone `odl` binary.
pub const HINTS: Hints = Hints { command: "odl", url_env: "ODL_URL", token_env: "ODL_TOKEN", project_env: "" };

#[derive(clap::Args, Debug, Clone)]
pub struct Win {
    /// Relative window ending now, e.g. 15m, 6h, 2d, 1h30m.
    #[arg(short, long, default_value = "1h", value_parser = check_window)]
    pub since: String,
    /// Absolute start (RFC 3339). Overrides --since.
    #[arg(long)]
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    /// Absolute end (RFC 3339). Default: now.
    #[arg(long)]
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

/// All `odl` commands. Embedders can mount this as a subcommand.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum Command {
    /// Check that the server is up (no token needed); exit code 1 if not.
    Health,
    /// Server health, ingest counters and storage size.
    Status,
    /// Services with request rate, error rate and latency.
    Services {
        #[command(flatten)]
        w: Win,
        /// Count all spans, not only entry spans.
        #[arg(long)]
        all_spans: bool,
    },
    /// Operations (span names) with RED metrics.
    Ops {
        #[command(flatten)]
        w: Win,
        #[arg(short = 'S', long)]
        service: Option<String>,
        #[arg(long)]
        all_spans: bool,
    },
    /// Search traces.
    Traces {
        #[command(flatten)]
        w: Win,
        #[arg(short = 'S', long)]
        service: Option<String>,
        /// Span name.
        #[arg(short = 'O', long)]
        operation: Option<String>,
        /// Only traces with an error span.
        #[arg(short, long)]
        errors: bool,
        /// Minimum span duration in ms.
        #[arg(long)]
        min_ms: Option<f64>,
        #[arg(long)]
        max_ms: Option<f64>,
        /// Attribute filter key=value (repeatable).
        #[arg(short, long)]
        attr: Vec<String>,
        /// Max traces (`-l` still works).
        #[arg(short = 'n', short_alias = 'l', long, default_value_t = 30)]
        limit: u32,
    },
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
    Logs {
        #[command(flatten)]
        w: Win,
        #[arg(short = 'S', long)]
        service: Option<String>,
        /// Minimum severity: trace, debug, info, warn, error, fatal or 1-24.
        #[arg(short = 'l', long)]
        level: Option<String>,
        /// Substring to find in the body (case-insensitive).
        #[arg(short, long)]
        query: Option<String>,
        #[arg(short, long)]
        trace: Option<String>,
        #[arg(short, long)]
        attr: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 100)]
        limit: u32,
        /// Keep printing new records (from --from/--since on, until Ctrl-C).
        #[arg(short, long, conflicts_with = "to")]
        follow: bool,
    },
    /// List metrics.
    Metrics {
        #[command(flatten)]
        w: Win,
        #[arg(short = 'S', long)]
        service: Option<String>,
        /// Name substring.
        #[arg(short, long)]
        query: Option<String>,
    },
    /// Query a metric over time.
    Metric {
        name: String,
        #[command(flatten)]
        w: Win,
        #[arg(short = 'S', long)]
        service: Option<String>,
        /// avg, min, max, sum, count, rate, increase, p50, p95, p99 ...
        #[arg(short = 'g', long)]
        agg: Option<String>,
        /// Group by attribute keys (comma separated).
        #[arg(short, long)]
        by: Option<String>,
        #[arg(short, long)]
        attr: Vec<String>,
        /// Bucket width, e.g. 1m.
        #[arg(long)]
        step: Option<String>,
    },
    /// Run a read-only SQL query (server must enable it).
    Sql {
        query: String,
        #[arg(short = 'n', long, default_value_t = 100)]
        limit: u32,
    },
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

impl Win {
    fn parts(&self) -> (Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>, Option<String>) {
        (self.from, self.to, if self.from.is_some() { None } else { Some(self.since.clone()) })
    }
}

/// Attribute filters `k=v` as the API takes them: comma separated.
pub(crate) fn attrs(a: &[String]) -> Option<String> {
    (!a.is_empty()).then(|| a.join(","))
}

/// Print `v` as pretty JSON or as the text `text` makes of it.
fn print<T: serde::Serialize + ?Sized>(ctx: &Ctx, v: &T, text: impl FnOnce(&T) -> String) -> Result<()> {
    if ctx.json {
        ctx.out.line(&json_safe(&serde_json::to_string_pretty(v)?))?;
    } else {
        ctx.out.line(&text(v))?;
    }
    Ok(())
}

/// What the commands need besides their own arguments:
/// `Ctx::new(client).with_json(true)`. The same builder as `sdl_cli::Ctx`.
#[derive(Debug)]
pub struct Ctx {
    client: Client,
    json: bool,
    out: Out,
    hints: Hints,
}

impl Ctx {
    /// Table output, colour as `--color=auto` says, [`HINTS`].
    pub fn new(client: Client) -> Self {
        Ctx { client, json: false, out: Out::stdout(ColorChoice::Auto), hints: HINTS }
    }

    /// Print JSON instead of tables.
    pub fn with_json(mut self, json: bool) -> Self {
        self.json = json;
        self
    }

    /// The `--color` flag (for stdout).
    pub fn with_color(mut self, color: ColorChoice) -> Self {
        self.out = Out::stdout(color);
        self
    }

    pub fn with_hints(mut self, hints: Hints) -> Self {
        self.hints = hints;
        self
    }

    /// The friendly message, with the client error kept as its source.
    fn err(&self, e: odl_client::Error) -> anyhow::Error {
        let msg = describe(&e, self.client.base_url(), &self.hints);
        anyhow::Error::new(e).context(Described(msg))
    }
}

/// One-line, actionable message for a client error. Safe to print.
pub fn describe(e: &odl_client::Error, url: &str, hints: &Hints) -> String {
    use odl_client::Error as E;
    let f = match e {
        E::Api { status, message, .. } => Failure::Status { status: *status, message },
        E::InvalidUrl(m) => Failure::InvalidUrl(m),
        E::Http(h) if h.is_connect() => Failure::Connect,
        E::Http(h) if h.is_timeout() => Failure::Timeout,
        E::Http(h) if h.is_decode() => Failure::Decode(h.to_string()),
        E::Http(h) => Failure::Other(h.to_string()),
        e => return clean(&e.to_string()).into_owned(),
    };
    mallard_term::error::describe(f, url, hints)
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
        Command::Services { w, all_spans } => {
            let (from, to, since) = w.parts();
            let v = c.services(&ServiceQuery { from, to, since, entry_only: Some(!all_spans) }).await?;
            print(ctx, &v, |v| services_text(v))
        }
        Command::Ops { w, service, all_spans } => {
            let (from, to, since) = w.parts();
            let v = c.operations(&OperationQuery { from, to, since, service, entry_only: Some(!all_spans) }).await?;
            print(ctx, &v, |v| operations_text(v))
        }
        Command::Traces { w, service, operation, errors, min_ms, max_ms, attr, limit } => {
            let (from, to, since) = w.parts();
            let v = c
                .traces(&TraceQuery {
                    from,
                    to,
                    since,
                    service,
                    operation,
                    errors_only: Some(errors),
                    min_duration_ms: min_ms,
                    max_duration_ms: max_ms,
                    attributes: attrs(&attr),
                    limit: Some(limit),
                })
                .await?;
            print(ctx, &v, |v| traces_text(v))
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
                    eprintln!("note: --since {since} is larger than the server allows; searching the last 24h");
                    get("24h".into()).await?
                }
                r => r?,
            };
            print(ctx, &t, |t| trace_lines(t, attributes).join("\n"))
        }
        Command::Logs { w, service, level, query, trace, attr, limit, follow } => {
            let min_severity = level
                .as_deref()
                .map(|l| severity::parse(l).ok_or_else(|| anyhow::anyhow!("unknown level {l:?}")))
                .transpose()?;
            let (from, to, since) = w.parts();
            let q = LogQuery {
                from,
                to,
                since,
                service,
                min_severity,
                q: query,
                trace_id: trace,
                attributes: attrs(&attr),
                limit: Some(limit),
                order: Some(LogOrder::Desc),
            };
            if !follow {
                let mut v = c.logs(&q).await?;
                v.reverse(); // print oldest first, like a log file
                return print(ctx, &v, |v| v.iter().map(log_line).collect::<Vec<_>>().join("\n"));
            }
            // Start where --from/--since say; the first poll shows the newest
            // `--limit` records of that window, like `tail -n N -f`.
            let start = match from {
                Some(f) => f,
                None => chrono::Utc::now() - chrono::TimeDelta::from_std(parse_window(&w.since)?)?,
            };
            let mut tail = LogTail::new(c.clone(), q, start);
            // One Ctrl-C future for the whole loop, so a signal that arrives
            // while a request is in flight is not lost.
            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);
            loop {
                tokio::select! {
                    res = tail.poll() => match res {
                        Ok(v) => {
                            for r in &v {
                                if ctx.json {
                                    ctx.out.line(&json_safe(&serde_json::to_string(r)?))?;
                                } else {
                                    ctx.out.line(&log_line(r))?;
                                }
                            }
                        }
                        Err(e) => eprintln!("{} {}", alert("poll failed:"), describe(&e, c.base_url(), &ctx.hints)),
                    },
                    _ = &mut ctrl_c => return Ok(()),
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                    _ = &mut ctrl_c => return Ok(()),
                }
            }
        }
        Command::Metrics { w, service, query } => {
            let (from, to, since) = w.parts();
            let v = c.metrics(&MetricListQuery { from, to, since, service, q: query }).await?;
            print(ctx, &v, |v| metrics_text(v))
        }
        Command::Metric { name, w, service, agg, by, attr, step } => {
            let (from, to, since) = w.parts();
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
        Command::Sql { query, limit } => {
            let r = c.sql(&query, Some(limit)).await?;
            print(ctx, &r, sql_text)
        }
        Command::Tui { since } => tui::run_standalone(c.clone(), &since).await,
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
        let Command::Services { w, .. } = parse(&["services", "--since", "1h30m"]).unwrap() else { panic!() };
        assert_eq!(parse_window(&w.since).unwrap(), Duration::from_secs(5400));
        assert_eq!(w.parts().2.as_deref(), Some("1h30m"));
        let from = "2026-01-01T00:00:00Z".parse().unwrap();
        let w = Win { since: "1h".into(), from: Some(from), to: None };
        assert_eq!(w.parts(), (Some(from), None, None), "--from overrides --since");
    }
}
