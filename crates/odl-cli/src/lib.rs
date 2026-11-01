//! `odl` command line and terminal UI, usable as a library.
//!
//! - [`Cmd`] + [`run`]: mount the commands in another CLI.
//! - [`tui::Embedded`]: the TUI as a component without terminal setup.

pub mod fmt;
pub mod tui;

use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Subcommand, ValueEnum};
use odl_client::{Client, LogTail, api::*};
use owo_colors::OwoColorize;

use crate::fmt::*;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Output {
    Table,
    Json,
}

#[derive(clap::Args, Clone)]
pub struct Win {
    /// Relative window ending now, e.g. 15m, 6h, 2d.
    #[arg(short, long, default_value = "1h")]
    pub since: String,
    /// Absolute start (RFC 3339). Overrides --since.
    #[arg(long)]
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    /// Absolute end (RFC 3339). Default: now.
    #[arg(long)]
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

/// All `odl` commands. Embedders can mount this as a subcommand.
#[derive(Subcommand)]
pub enum Cmd {
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
        #[arg(short, long, default_value_t = 30)]
        limit: u32,
    },
    /// Show one trace as a waterfall.
    Trace {
        trace_id: String,
        /// Look-back window for the lookup.
        #[arg(short, long, default_value = "7d")]
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
        /// Keep printing new records.
        #[arg(short, long)]
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
        #[arg(short, long, default_value = "1h")]
        since: String,
    },
}

impl Win {
    fn parts(
        &self,
    ) -> (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    ) {
        (
            self.from,
            self.to,
            if self.from.is_some() {
                None
            } else {
                Some(self.since.clone())
            },
        )
    }
}

fn attrs(a: &[String]) -> Option<String> {
    (!a.is_empty()).then(|| a.join(","))
}

fn severity(level: &str) -> Result<u8> {
    Ok(match level.to_ascii_lowercase().as_str() {
        "trace" => 1,
        "debug" => 5,
        "info" => 9,
        "warn" | "warning" => 13,
        "error" => 17,
        "fatal" => 21,
        n => match n.parse::<u8>() {
            Ok(v) if v <= 24 => v,
            _ => bail!("unknown level {level:?}"),
        },
    })
}

fn json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Shared context for commands.
pub struct Ctx {
    pub client: Client,
    pub output: Output,
}

/// Run one command.
pub async fn run(cmd: Cmd, ctx: &Ctx) -> Result<()> {
    let c = ctx.client.clone();
    let out = ctx.output;
    match cmd {
        Cmd::Health => {
            let h = c.health().await?;
            if out == Output::Json {
                json(&h)
            } else {
                println!("{} {}", h.status, h.version);
                Ok(())
            }
        }
        Cmd::Status => {
            let s = c.stats().await?;
            if out == Output::Json {
                json(&s)
            } else {
                print_stats(&s, c.base_url());
                Ok(())
            }
        }
        Cmd::Services { w, all_spans } => {
            let (from, to, since) = w.parts();
            let v = c
                .services(&ServiceQuery {
                    from,
                    to,
                    since,
                    entry_only: Some(!all_spans),
                })
                .await?;
            if out == Output::Json {
                json(&v)
            } else {
                print_services(&v);
                Ok(())
            }
        }
        Cmd::Ops {
            w,
            service,
            all_spans,
        } => {
            let (from, to, since) = w.parts();
            let v = c
                .operations(&OperationQuery {
                    from,
                    to,
                    since,
                    service,
                    entry_only: Some(!all_spans),
                })
                .await?;
            if out == Output::Json {
                json(&v)
            } else {
                print_operations(&v);
                Ok(())
            }
        }
        Cmd::Traces {
            w,
            service,
            operation,
            errors,
            min_ms,
            max_ms,
            attr,
            limit,
        } => {
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
            if out == Output::Json {
                json(&v)
            } else {
                print_traces(&v);
                Ok(())
            }
        }
        Cmd::Trace {
            trace_id,
            since,
            attributes,
        } => {
            let t = c
                .trace(
                    &trace_id,
                    &TraceGetQuery {
                        since: Some(since),
                        ..Default::default()
                    },
                )
                .await?;
            if out == Output::Json {
                json(&t)
            } else {
                print_trace(&t, attributes);
                Ok(())
            }
        }
        Cmd::Logs {
            w,
            service,
            level,
            query,
            trace,
            attr,
            limit,
            follow,
        } => {
            let min_severity = level.as_deref().map(severity).transpose()?;
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
                order: Some("desc".into()),
            };
            if !follow {
                let mut v = c.logs(&q).await?;
                v.reverse(); // print oldest first, like a log file
                return if out == Output::Json {
                    json(&v)
                } else {
                    v.iter().for_each(print_log);
                    Ok(())
                };
            }
            let start = chrono::Utc::now() - chrono::TimeDelta::minutes(1);
            let mut tail = LogTail::new(c.clone(), q, start);
            loop {
                match tail.poll().await {
                    Ok(v) => {
                        for r in &v {
                            if out == Output::Json {
                                println!("{}", serde_json::to_string(r)?)
                            } else {
                                print_log(r)
                            }
                        }
                    }
                    Err(e) => eprintln!("{} {e}", "poll failed:".yellow()),
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                }
            }
        }
        Cmd::Metrics { w, service, query } => {
            let (from, to, since) = w.parts();
            let v = c
                .metrics(&MetricListQuery {
                    from,
                    to,
                    since,
                    service,
                    q: query,
                })
                .await?;
            if out == Output::Json {
                json(&v)
            } else {
                print_metrics(&v);
                Ok(())
            }
        }
        Cmd::Metric {
            name,
            w,
            service,
            agg,
            by,
            attr,
            step,
        } => {
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
            if out == Output::Json {
                json(&r)
            } else {
                print_metric(&r);
                Ok(())
            }
        }
        Cmd::Sql { query, limit } => {
            let r = c.sql(&query, Some(limit)).await?;
            if out == Output::Json {
                json(&r)
            } else {
                print_sql(&r);
                Ok(())
            }
        }
        Cmd::Tui { since } => tui::run_standalone(c, since).await,
    }
}
