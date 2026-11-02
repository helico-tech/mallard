//! `mallard series spans|logs|errors`: counts per time bucket from the
//! existing endpoints (otel `spans/series`, `logs/series`; Sentry project
//! `stats`). JSON is the list envelope with one item per bucket; `time` is
//! the start of the bucket (UTC). Default window 1h for all three.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Subcommand;
use mallard_client::mallard_otel_client::api::{LogSeriesQuery, SpanSeriesQuery};
use mallard_client::mallard_sentry_client::types::{WindowQuery, WindowRange};
use mallard_term::fmt::ms;
use mallard_term::output::List;
use mallard_term::table::{right, table};
use mallard_term::window::{Window, WindowArgs};
use serde::Serialize;

use crate::Run;

const WINDOW: Duration = Duration::from_secs(3600);

#[derive(Subcommand, Debug)]
pub(crate) enum SeriesCmd {
    /// Spans per bucket: count, errors and p50/p95/p99 (entry spans unless --all-spans).
    Spans(SpanArgs),
    /// Log records per bucket: total and per severity band.
    Logs(LogArgs),
    /// Sentry events of a project per bucket: errors, warnings, other.
    Errors(ErrorArgs),
}

#[derive(clap::Args, Debug)]
pub(crate) struct SpanArgs {
    #[command(flatten)]
    w: WindowArgs,
    #[arg(short = 'S', long)]
    service: Option<String>,
    /// Span name.
    #[arg(short = 'O', long)]
    operation: Option<String>,
    /// Bucket width, e.g. 5m (default: about 60 buckets).
    #[arg(long)]
    step: Option<String>,
    /// Count all spans, not only entry spans.
    #[arg(long)]
    all_spans: bool,
}

#[derive(clap::Args, Debug)]
pub(crate) struct LogArgs {
    #[command(flatten)]
    w: WindowArgs,
    #[arg(short = 'S', long)]
    service: Option<String>,
    /// Substring to find in the body (case-insensitive).
    #[arg(short, long)]
    query: Option<String>,
    /// Bucket width, e.g. 5m (default: about 60 buckets).
    #[arg(long)]
    step: Option<String>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct ErrorArgs {
    #[command(flatten)]
    w: WindowArgs,
    /// Bucket width, e.g. 5m (default: window / 24).
    #[arg(long)]
    step: Option<String>,
}

impl SeriesCmd {
    /// The window and `-S` of a telemetry series; `None` for the errors series.
    pub(crate) fn scope(&self) -> Option<(&WindowArgs, Option<&String>)> {
        match self {
            SeriesCmd::Spans(a) => Some((&a.w, a.service.as_ref())),
            SeriesCmd::Logs(a) => Some((&a.w, a.service.as_ref())),
            SeriesCmd::Errors(_) => None,
        }
    }
}

/// One bucket of Sentry events.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct ErrorPoint {
    time: DateTime<Utc>,
    events: u64,
    errors: u64,
    warnings: u64,
    other: u64,
}

pub(crate) async fn run(r: &Run, cmd: SeriesCmd) -> Result<()> {
    let c = &r.client;
    let otel = |e| crate::described(mallard_client::Error::Otel(e), c.base_url());
    match cmd {
        SeriesCmd::Spans(a) => {
            let w = a.w.resolve(WINDOW)?;
            let q = SpanSeriesQuery {
                from: Some(w.from),
                to: Some(w.to),
                since: None,
                service: r.services(a.service),
                operation: a.operation,
                step: a.step,
                entry_only: Some(!a.all_spans),
            };
            let s = c.otel.span_series(&q).await.map_err(otel)?;
            print(r, w, s.points, &["time", "count", "errors", "p50", "p95", "p99"], |p| {
                vec![
                    right(p.time.format("%Y-%m-%d %H:%M:%S")),
                    right(p.count),
                    right(p.errors),
                    right(ms(p.p50_ms)),
                    right(ms(p.p95_ms)),
                    right(ms(p.p99_ms)),
                ]
            })
        }
        SeriesCmd::Logs(a) => {
            let w = a.w.resolve(WINDOW)?;
            let q = LogSeriesQuery {
                from: Some(w.from),
                to: Some(w.to),
                since: None,
                service: r.services(a.service),
                q: a.query,
                step: a.step,
            };
            let s = c.otel.log_series(&q).await.map_err(otel)?;
            print(r, w, s.points, &["time", "total", "debug", "info", "warn", "error"], |p| {
                vec![
                    right(p.time.format("%Y-%m-%d %H:%M:%S")),
                    right(p.total),
                    right(p.debug),
                    right(p.info),
                    right(p.warn),
                    right(p.error),
                ]
            })
        }
        SeriesCmd::Errors(a) => {
            let w = a.w.resolve(WINDOW)?;
            let ctx = r.sentry();
            let project = ctx.resolve_project().await?;
            let range = WindowRange { from: Some(w.from), to: Some(w.to), step: None };
            let s = c
                .sentry
                .stats_in(&project, &WindowQuery { since: None, interval: a.step }, &range)
                .await
                .map_err(|e| crate::described(mallard_client::Error::Sentry(e), c.base_url()))?;
            let points = s
                .buckets
                .into_iter()
                .map(|b| ErrorPoint {
                    time: b.start,
                    events: b.events,
                    errors: b.errors,
                    warnings: b.warnings,
                    other: b.other,
                })
                .collect();
            print(r, w, points, &["time", "events", "errors", "warnings", "other"], |p| {
                vec![
                    right(p.time.format("%Y-%m-%d %H:%M:%S")),
                    right(p.events),
                    right(p.errors),
                    right(p.warnings),
                    right(p.other),
                ]
            })
        }
    }
}

/// The envelope (JSON), or a table with one row per bucket.
fn print<T: Serialize>(
    r: &Run,
    w: Window,
    points: Vec<T>,
    head: &[&str],
    row: impl Fn(&T) -> Vec<comfy_table::Cell>,
) -> Result<()> {
    let list = List::new(points).window(w);
    if !r.output.text() {
        return r.output.list(&list);
    }
    let mut t = table(head);
    for p in &list.items {
        t.add_row(row(p));
    }
    r.output.out.line(&t.to_string())?;
    Ok(())
}
