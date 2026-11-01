//! The bridge between the two sides (docs/adr/0009-bridge.md), in process:
//!
//! - Sentry transactions → OTel spans: sdl-server hands each transaction
//!   item to [`Transactions`], which converts it on a blocking thread and
//!   buffers it with `odl_server::Server::ingest_traces`. A full buffer
//!   drops it (counted); Sentry ingest never waits.
//! - OTel exceptions → Sentry events: odl-server shows every buffered
//!   OTLP request to [`Exceptions`], which queues the exceptions in it. The
//!   [`Worker`] holds each one for `dedupe_window`, drops it when a Sentry
//!   SDK sent an event for the same trace + span, and otherwise stores it
//!   with `sdl_server::Server::ingest_event` in the project of its service.

pub mod exception;
mod stacktrace;
pub mod transaction;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::Result;
use odl_server::IngestError as OtelError;
use opentelemetry_proto::tonic::collector::{logs::v1::ExportLogsServiceRequest, trace::v1::ExportTraceServiceRequest};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::BridgeConfig;
use exception::OtelException;

/// Transactions being converted at once; more are dropped.
const MAX_PENDING_TRANSACTIONS: usize = 1024;
/// OTel exceptions waiting for their dedupe window; more are dropped.
const EXCEPTION_QUEUE: usize = 10_000;
/// OTel exceptions stored at once (each waits for a Sentry write batch).
const EXCEPTION_WORKERS: usize = 64;
/// How long a Sentry SDK event's span is remembered for dedupe.
const SEEN_SPAN_TTL: Duration = Duration::from_secs(3600);
const SEEN_SPANS: u64 = 200_000;

/// Install the hooks `cfg` turns on. Returns the worker that stores OTel
/// exceptions (to run with [`Worker::run`]) when that half is on.
pub fn install(
    cfg: &BridgeConfig,
    sentry: &sdl_server::Server,
    otel: &Arc<odl_server::Server>,
) -> Result<Option<Worker>> {
    if cfg.transactions {
        let sink = Transactions {
            otel: otel.clone(),
            runtime: tokio::runtime::Handle::current(),
            pending: Arc::new(AtomicUsize::new(0)),
        };
        sentry.set_transaction_sink(Arc::new(sink))?;
    }
    if !cfg.exceptions {
        return Ok(None);
    }
    let seen =
        Arc::new(SeenSpans(moka::sync::Cache::builder().max_capacity(SEEN_SPANS).time_to_live(SEEN_SPAN_TTL).build()));
    sentry.set_event_observer(seen.clone())?;
    let (tx, rx) = mpsc::channel(EXCEPTION_QUEUE);
    otel.set_ingest_hook(Arc::new(Exceptions { tx, window: cfg.dedupe_window }))?;
    Ok(Some(Worker {
        rx,
        ctx: Arc::new(Ctx { sentry: sentry.clone(), seen, default_project: cfg.default_project.clone() }),
    }))
}

/// Converts Sentry transactions and buffers them as OTel spans.
struct Transactions {
    otel: Arc<odl_server::Server>,
    runtime: tokio::runtime::Handle,
    pending: Arc<AtomicUsize>,
}

impl sdl_server::TransactionSink for Transactions {
    fn transaction(&self, t: sdl_server::Transaction) {
        if self.pending.fetch_add(1, Ordering::Relaxed) >= MAX_PENDING_TRANSACTIONS {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            count_transaction("busy");
            return;
        }
        let (otel, pending) = (self.otel.clone(), self.pending.clone());
        self.runtime.spawn_blocking(move || {
            let _done = Decrement(pending);
            count_transaction(store_transaction(&otel, &t));
        });
    }
}

/// Lowers the pending count when dropped (also after a panic).
struct Decrement(Arc<AtomicUsize>);

impl Drop for Decrement {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Convert and buffer one transaction; the outcome for the metric.
fn store_transaction(otel: &odl_server::Server, t: &sdl_server::Transaction) -> &'static str {
    let Ok(tx) = serde_json::from_slice(&t.payload) else { return "invalid" };
    let Some(req) = transaction::to_otlp(&tx, &t.project_slug) else { return "invalid" };
    match otel.ingest_traces(req) {
        Ok(n) => {
            metrics::counter!("mallard_bridge_spans_total").increment(n);
            "stored"
        }
        Err(OtelError::Backpressure(_)) => "otel_full",
        Err(OtelError::TooLarge(_)) => "too_large",
        Err(e) => {
            tracing::warn!(error = %e, "storing a Sentry transaction as spans failed");
            "error"
        }
    }
}

fn count_transaction(result: &'static str) {
    metrics::counter!("mallard_bridge_transactions_total", "result" => result).increment(1);
}

fn count_exception(result: &'static str) {
    metrics::counter!("mallard_bridge_exceptions_total", "result" => result).increment(1);
}

/// Spans that a Sentry SDK event was stored for, recently.
struct SeenSpans(moka::sync::Cache<(Uuid, u64), ()>);

impl sdl_server::EventObserver for SeenSpans {
    fn event_stored(&self, _project_id: u32, trace_id: Uuid, span_id: u64) {
        self.0.insert((trace_id, span_id), ());
    }
}

/// An OTel exception waiting until `due` (its dedupe window).
struct Pending {
    due: tokio::time::Instant,
    exception: OtelException,
}

/// Finds exceptions in buffered OTLP requests and queues them.
struct Exceptions {
    tx: mpsc::Sender<Pending>,
    window: Duration,
}

impl Exceptions {
    fn queue(&self, found: Vec<OtelException>) {
        let due = tokio::time::Instant::from_std(Instant::now() + self.window);
        for exception in found {
            if self.tx.try_send(Pending { due, exception }).is_err() {
                count_exception("queue_full");
            }
        }
    }
}

impl odl_server::IngestHook for Exceptions {
    fn traces(&self, req: &ExportTraceServiceRequest) {
        self.queue(exception::from_traces(req));
    }
    fn logs(&self, req: &ExportLogsServiceRequest) {
        self.queue(exception::from_logs(req));
    }
}

struct Ctx {
    sentry: sdl_server::Server,
    seen: Arc<SeenSpans>,
    default_project: String,
}

/// Stores queued OTel exceptions as Sentry events.
pub struct Worker {
    rx: mpsc::Receiver<Pending>,
    ctx: Arc<Ctx>,
}

impl Worker {
    /// Run until `stop` is cancelled; then store what is queued without
    /// waiting for the dedupe window (the receivers are down, so no Sentry
    /// event can arrive any more) and return.
    pub async fn run(mut self, stop: CancellationToken) {
        let mut tasks = JoinSet::new();
        loop {
            let next = tokio::select! {
                p = self.rx.recv() => p,
                () = stop.cancelled(), if !self.rx.is_closed() => {
                    self.rx.close();
                    continue;
                }
            };
            let Some(p) = next else { break };
            if !stop.is_cancelled() {
                tokio::select! {
                    () = tokio::time::sleep_until(p.due) => {}
                    () = stop.cancelled() => {}
                }
            }
            while tasks.len() >= EXCEPTION_WORKERS {
                tasks.join_next().await;
            }
            let ctx = self.ctx.clone();
            tasks.spawn(async move { count_exception(ctx.store(p.exception).await) });
        }
        while tasks.join_next().await.is_some() {}
    }
}

impl Ctx {
    /// Store one exception unless a Sentry SDK already reported its span.
    async fn store(&self, e: OtelException) -> &'static str {
        if let (Some(t), Some(s)) = (e.trace_id, e.span_id)
            && self.seen.0.contains_key(&(t, s))
        {
            return "duplicate";
        }
        let Some(project) = self.project(&e).await else { return "no_project" };
        match self.sentry.ingest_event(project, exception::to_sentry_event(&e)).await {
            Ok(_) => "stored",
            Err(sdl_server::IngestError::QueueFull) => "sentry_full",
            Err(err) => {
                tracing::warn!(error = %err, "storing an OTel exception failed");
                "error"
            }
        }
    }

    /// The project whose slug is the service name; else the default
    /// project; else a new project for the service. Missing ones are created.
    async fn project(&self, e: &OtelException) -> Option<u32> {
        let slug = service_slug(&e.service);
        if let Some(id) = self.sentry.project_by_slug(&slug) {
            return Some(id);
        }
        let (name, slug) = match self.default_project.as_str() {
            "" => (e.service.as_str(), slug),
            p => (p, p.to_string()),
        };
        if let Some(id) = self.sentry.project_by_slug(&slug) {
            return Some(id);
        }
        match self.sentry.create_project(name, &slug, Some(e.platform())).await {
            Ok(id) => {
                tracing::info!(project = %slug, "created a project for OTel exceptions");
                Some(id)
            }
            Err(err) => {
                tracing::warn!(project = %slug, error = %format!("{err:#}"), "no project for an OTel exception");
                None
            }
        }
    }
}

/// A valid project slug for a service name (`unknown_service:node` →
/// `unknown-service-node`).
fn service_slug(service: &str) -> String {
    let mut s = sdl_types::slugify(service);
    s.truncate(64);
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "unknown-service".into()
    } else if s.bytes().all(|b| b.is_ascii_digit()) {
        format!("service-{s}")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_become_valid_slugs() {
        assert_eq!(service_slug("checkout"), "checkout");
        assert_eq!(service_slug("Checkout API"), "checkout-api");
        assert_eq!(service_slug("unknown_service:node"), "unknown-service-node");
        assert_eq!(service_slug("42"), "service-42");
        assert_eq!(service_slug("::"), "unknown-service");
        assert_eq!(service_slug(&"a".repeat(80)).len(), 64);
    }
}
