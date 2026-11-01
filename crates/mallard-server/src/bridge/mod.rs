//! The bridge between the two sides (docs/adr/0009-bridge.md), in process:
//!
//! - Sentry transactions and span items (span streaming, ADR 0013) → OTel
//!   spans: sdl-server hands each `transaction` and `span` item to
//!   [`Transactions`], which queues it for `dedupe_window`. The
//!   [`TransactionWorker`] then converts it on a blocking thread, drops the
//!   spans the app also sent over OTLP ([`OtlpSpans`], ADR 0010) and
//!   buffers the rest with `odl_server::Server::ingest_traces`. A full
//!   queue or OTel buffer drops it (counted); Sentry ingest never waits.
//! - OTel exceptions → Sentry events: odl-server shows every buffered
//!   OTLP request to [`OtlpHook`], which remembers its span ids and queues
//!   the exceptions in it. The [`ExceptionWorker`] holds each one for
//!   `dedupe_window`, drops it when a Sentry SDK sent an event for the same
//!   trace + span, and otherwise stores it with
//!   `sdl_server::Server::ingest_event` in the project of its service.

pub mod exception;
pub mod span;
mod stacktrace;
pub mod transaction;

use std::collections::HashSet;
use std::future::Future;
use std::sync::{
    Arc, Mutex, PoisonError,
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

/// Sentry transactions and span items waiting for their dedupe window: at
/// most this many and this many payload bytes; more are dropped.
const TRANSACTION_QUEUE: usize = 50_000;
const TRANSACTION_QUEUE_BYTES: usize = 256 << 20;
/// Transactions and span items converted and stored at once (blocking threads).
const TRANSACTION_WORKERS: usize = 32;
/// OTLP span ids are remembered in two generations; a new one starts when
/// the current one is this old or holds this many ids (ADR 0010).
const OTLP_SPAN_GENERATION: Duration = Duration::from_secs(300);
const OTLP_SPANS_PER_GENERATION: usize = 500_000;
/// OTel exceptions waiting for their dedupe window; more are dropped.
const EXCEPTION_QUEUE: usize = 10_000;
/// OTel exceptions stored at once (each waits for a Sentry write batch).
const EXCEPTION_WORKERS: usize = 64;
/// How long a Sentry SDK event's span is remembered for dedupe.
const SEEN_SPAN_TTL: Duration = Duration::from_secs(3600);
const SEEN_SPANS: u64 = 200_000;

/// The bridge's background workers; each is `None` when its half is off.
#[derive(Default)]
pub struct Workers {
    /// Stores Sentry transactions and span items as spans (`bridge.transactions`).
    pub transactions: Option<TransactionWorker>,
    /// Stores OTel exceptions as Sentry events (`bridge.exceptions`).
    pub exceptions: Option<ExceptionWorker>,
}

/// Install the hooks `cfg` turns on. Returns the workers to run (with
/// their `run`) for the halves that are on.
pub fn install(cfg: &BridgeConfig, sentry: &sdl_server::Server, otel: &Arc<odl_server::Server>) -> Result<Workers> {
    let mut workers = Workers::default();
    let mut hook = OtlpHook::default();
    if cfg.transactions {
        let otlp_spans = Arc::new(OtlpSpans::new(OTLP_SPAN_GENERATION, OTLP_SPANS_PER_GENERATION));
        let held = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel(TRANSACTION_QUEUE);
        let sink = Arc::new(Transactions { tx, window: cfg.dedupe_window, held });
        sentry.set_transaction_sink(sink.clone())?;
        sentry.set_span_sink(sink)?;
        hook.spans = Some(otlp_spans.clone());
        workers.transactions =
            Some(TransactionWorker { rx, store: Arc::new(SpanStore { otel: otel.clone(), otlp_spans }) });
    }
    if cfg.exceptions {
        let seen = Arc::new(SeenSpans(
            moka::sync::Cache::builder().max_capacity(SEEN_SPANS).time_to_live(SEEN_SPAN_TTL).build(),
        ));
        sentry.set_event_observer(seen.clone())?;
        let (tx, rx) = mpsc::channel(EXCEPTION_QUEUE);
        hook.exceptions = Some(Exceptions { tx, window: cfg.dedupe_window });
        workers.exceptions = Some(ExceptionWorker {
            rx,
            ctx: Arc::new(Ctx { sentry: sentry.clone(), seen, default_project: cfg.default_project.clone() }),
        });
    }
    if cfg.transactions || cfg.exceptions {
        otel.set_ingest_hook(Arc::new(hook))?;
    }
    Ok(workers)
}

/// An item waiting until `due`, the end of its dedupe window.
struct Delayed<T> {
    due: tokio::time::Instant,
    item: T,
}

/// Hand each queued item to `handle` once it is due, at most `workers` at
/// once. When `stop` is cancelled, handle what is queued without waiting
/// (the receivers are down, so no copy from the other side can arrive any
/// more) and return when all of it is done.
async fn run_delayed<T, F, Fut>(mut rx: mpsc::Receiver<Delayed<T>>, stop: CancellationToken, workers: usize, handle: F)
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    loop {
        let next = tokio::select! {
            p = rx.recv() => p,
            () = stop.cancelled(), if !rx.is_closed() => {
                rx.close();
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
        while tasks.len() >= workers {
            tasks.join_next().await;
        }
        tasks.spawn(handle(p.item));
    }
    while tasks.join_next().await.is_some() {}
}

/// Queues Sentry transactions and span items for their dedupe window.
struct Transactions {
    tx: mpsc::Sender<Delayed<Held>>,
    window: Duration,
    /// Payload bytes queued.
    held: Arc<AtomicUsize>,
}

/// A queued transaction or span item.
struct Held {
    kind: Kind,
    project_slug: String,
    payload: Vec<u8>,
    _bytes: Release,
}

/// What a queued payload is.
enum Kind {
    Transaction,
    /// A v2 `span` item and what its envelope's headers say.
    Spans(span::Defaults),
    /// A standalone `span` item of an SDK before 11, same.
    Standalone(span::Defaults),
}

impl Kind {
    /// The metric that counts outcomes of this kind of item.
    fn metric(&self) -> &'static str {
        match self {
            Kind::Transaction => "mallard_bridge_transactions_total",
            Kind::Spans(_) | Kind::Standalone(_) => "mallard_bridge_span_items_total",
        }
    }

    fn count(&self, result: &'static str) {
        count(self.metric(), result);
    }
}

impl Transactions {
    fn queue(&self, kind: Kind, project_slug: String, payload: &[u8]) {
        let n = payload.len();
        if self.held.fetch_add(n, Ordering::Relaxed) + n > TRANSACTION_QUEUE_BYTES {
            self.held.fetch_sub(n, Ordering::Relaxed);
            kind.count("busy");
            return;
        }
        // A copy: the payload may be a slice of a much larger envelope body.
        let item = Held { kind, project_slug, payload: payload.to_vec(), _bytes: Release(self.held.clone(), n) };
        if let Err(e) = self.tx.try_send(Delayed { due: tokio::time::Instant::now() + self.window, item }) {
            let (mpsc::error::TrySendError::Full(d) | mpsc::error::TrySendError::Closed(d)) = e;
            d.item.kind.count("busy");
        }
    }
}

impl sdl_server::TransactionSink for Transactions {
    fn transaction(&self, t: sdl_server::Transaction) {
        self.queue(Kind::Transaction, t.project_slug, &t.payload);
    }
}

impl sdl_server::SpanSink for Transactions {
    fn spans(&self, s: sdl_server::Spans<'_>) {
        let kind: fn(span::Defaults) -> Kind = match s.content_type {
            Some(span::V2_CONTENT_TYPE) => Kind::Spans,
            None => Kind::Standalone,
            Some(_) => {
                count("mallard_bridge_span_items_total", "unsupported");
                return;
            }
        };
        self.queue(kind(span::Defaults::from_headers(s.envelope_headers)), s.project_slug, s.payload);
    }
}

/// Gives `n` back to the counter when dropped (also after a panic).
struct Release(Arc<AtomicUsize>, usize);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Ordering::Relaxed);
    }
}

/// Converts due Sentry transactions and span items and stores them as spans.
pub struct TransactionWorker {
    rx: mpsc::Receiver<Delayed<Held>>,
    store: Arc<SpanStore>,
}

impl TransactionWorker {
    /// Run until `stop` is cancelled; then store what is queued without
    /// waiting and return. Run it before otel's final flush.
    pub async fn run(self, stop: CancellationToken) {
        let store = self.store;
        run_delayed(self.rx, stop, TRANSACTION_WORKERS, move |t: Held| {
            let store = store.clone();
            async move {
                let metric = t.kind.metric();
                let result = tokio::task::spawn_blocking(move || store.store(&t)).await;
                count(metric, result.unwrap_or("error"));
            }
        })
        .await;
    }
}

struct SpanStore {
    otel: Arc<odl_server::Server>,
    otlp_spans: Arc<OtlpSpans>,
}

impl SpanStore {
    /// Convert one transaction or span item, drop the spans that came over
    /// OTLP too and buffer the rest; the outcome for the metric. Blocking.
    fn store(&self, t: &Held) -> &'static str {
        let Ok(payload) = serde_json::from_slice(&t.payload) else { return "invalid" };
        let req = match &t.kind {
            Kind::Transaction => transaction::to_otlp(&payload, &t.project_slug),
            Kind::Spans(defaults) => span::to_otlp(&payload, defaults, &t.project_slug),
            Kind::Standalone(defaults) => span::standalone_to_otlp(&payload, defaults, &t.project_slug),
        };
        let Some(mut req) = req else { return "invalid" };
        if req.resource_spans.is_empty() {
            return "invalid"; // a span item without one valid span
        }
        let dropped = self.otlp_spans.remove_seen(&mut req);
        if dropped > 0 {
            metrics::counter!("mallard_bridge_duplicate_spans_total").increment(dropped);
        }
        if req.resource_spans.iter().flat_map(|r| &r.scope_spans).all(|s| s.spans.is_empty()) {
            return "duplicate";
        }
        match self.otel.ingest_traces(req) {
            Ok(n) => {
                metrics::counter!("mallard_bridge_spans_total").increment(n);
                "stored"
            }
            Err(OtelError::Backpressure(_)) => "otel_full",
            Err(OtelError::TooLarge(_)) => "too_large",
            Err(e) => {
                tracing::warn!(error = %e, "storing Sentry spans failed");
                "error"
            }
        }
    }
}

/// Trace id then span id, packed (24 bytes, no padding).
type SpanKey = [u8; 24];

fn span_key(trace_id: &[u8], span_id: &[u8]) -> Option<SpanKey> {
    if trace_id.len() != 16 || span_id.len() != 8 {
        return None;
    }
    let mut k = [0; 24];
    k[..16].copy_from_slice(trace_id);
    k[16..].copy_from_slice(span_id);
    Some(k)
}

/// Ids (trace + span) of spans received over OTLP recently, to drop the
/// Sentry copy of a span that an app sends both ways (ADR 0010). Two
/// generations: when the current one is `generation` old or holds `cap`
/// ids, it becomes the previous one and the one before is dropped. So an
/// id is kept for at least `generation` or `cap` spans, whichever ends
/// first, and memory stays bounded (2 × `cap` ids).
struct OtlpSpans {
    generation: Duration,
    cap: usize,
    ids: Mutex<Generations>,
}

struct Generations {
    current: HashSet<SpanKey>,
    previous: HashSet<SpanKey>,
    started: Instant,
}

impl Generations {
    fn rotate(&mut self) {
        self.previous = std::mem::take(&mut self.current);
        self.started = Instant::now();
    }
}

impl OtlpSpans {
    fn new(generation: Duration, cap: usize) -> Self {
        let ids = Generations { current: HashSet::new(), previous: HashSet::new(), started: Instant::now() };
        OtlpSpans { generation, cap, ids: Mutex::new(ids) }
    }

    fn record(&self, req: &ExportTraceServiceRequest) {
        let mut g = self.ids.lock().unwrap_or_else(PoisonError::into_inner);
        if g.started.elapsed() >= self.generation {
            g.rotate();
        }
        for s in req.resource_spans.iter().flat_map(|r| &r.scope_spans).flat_map(|s| &s.spans) {
            if let Some(k) = span_key(&s.trace_id, &s.span_id) {
                if g.current.len() >= self.cap {
                    g.rotate();
                }
                g.current.insert(k);
            }
        }
    }

    /// Remove the spans whose ids were seen; returns how many.
    fn remove_seen(&self, req: &mut ExportTraceServiceRequest) -> u64 {
        let g = self.ids.lock().unwrap_or_else(PoisonError::into_inner);
        let mut removed = 0;
        for scope in req.resource_spans.iter_mut().flat_map(|r| &mut r.scope_spans) {
            scope.spans.retain(|s| {
                let seen = span_key(&s.trace_id, &s.span_id)
                    .is_some_and(|k| g.current.contains(&k) || g.previous.contains(&k));
                removed += u64::from(seen);
                !seen
            });
        }
        removed
    }
}

fn count(metric: &'static str, result: &'static str) {
    metrics::counter!(metric, "result" => result).increment(1);
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

/// Sees every buffered OTLP request: remembers its span ids (for
/// transactions) and queues its exceptions.
#[derive(Default)]
struct OtlpHook {
    spans: Option<Arc<OtlpSpans>>,
    exceptions: Option<Exceptions>,
}

impl odl_server::IngestHook for OtlpHook {
    fn traces(&self, req: &ExportTraceServiceRequest) {
        if let Some(spans) = &self.spans {
            spans.record(req);
        }
        if let Some(e) = &self.exceptions {
            e.queue(exception::from_traces(req));
        }
    }
    fn logs(&self, req: &ExportLogsServiceRequest) {
        if let Some(e) = &self.exceptions {
            e.queue(exception::from_logs(req));
        }
    }
}

/// Queues OTel exceptions for their dedupe window.
struct Exceptions {
    tx: mpsc::Sender<Delayed<OtelException>>,
    window: Duration,
}

impl Exceptions {
    fn queue(&self, found: Vec<OtelException>) {
        let due = tokio::time::Instant::now() + self.window;
        for item in found {
            if self.tx.try_send(Delayed { due, item }).is_err() {
                count_exception("queue_full");
            }
        }
    }
}

struct Ctx {
    sentry: sdl_server::Server,
    seen: Arc<SeenSpans>,
    default_project: String,
}

/// Stores queued OTel exceptions as Sentry events.
pub struct ExceptionWorker {
    rx: mpsc::Receiver<Delayed<OtelException>>,
    ctx: Arc<Ctx>,
}

impl ExceptionWorker {
    /// Run until `stop` is cancelled; then store what is queued without
    /// waiting and return. Run it before Sentry's final flush.
    pub async fn run(self, stop: CancellationToken) {
        let ctx = self.ctx;
        run_delayed(self.rx, stop, EXCEPTION_WORKERS, move |e| {
            let ctx = ctx.clone();
            async move { count_exception(ctx.store(e).await) }
        })
        .await;
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

    fn request(ids: &[(u8, u8)]) -> ExportTraceServiceRequest {
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
        let spans = ids
            .iter()
            .map(|&(t, s)| Span { trace_id: vec![t; 16], span_id: vec![s; 8], ..Default::default() })
            .collect();
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans { spans, ..Default::default() }],
                ..Default::default()
            }],
        }
    }

    fn left(req: &ExportTraceServiceRequest) -> Vec<u8> {
        req.resource_spans[0].scope_spans[0].spans.iter().map(|s| s.span_id[0]).collect()
    }

    #[test]
    fn otlp_spans_drop_only_seen_ids() {
        let seen = OtlpSpans::new(Duration::from_secs(60), 100);
        seen.record(&request(&[(1, 1), (1, 2)]));
        let mut req = request(&[(1, 1), (1, 2), (1, 3), (2, 1)]);
        assert_eq!(seen.remove_seen(&mut req), 2);
        assert_eq!(left(&req), [3, 1], "the same span id in another trace stays");
    }

    #[test]
    fn otlp_spans_keep_two_generations() {
        let seen = OtlpSpans::new(Duration::from_secs(60), 2);
        seen.record(&request(&[(1, 1), (1, 2)]));
        seen.record(&request(&[(1, 3), (1, 4)])); // full: 1, 2 become the previous generation
        let mut req = request(&[(1, 1), (1, 4)]);
        assert_eq!(seen.remove_seen(&mut req), 2);
        seen.record(&request(&[(1, 5)])); // full again: 1, 2 are forgotten
        let mut req = request(&[(1, 1), (1, 3), (1, 5)]);
        assert_eq!(seen.remove_seen(&mut req), 2);
        assert_eq!(left(&req), [1]);

        let seen = OtlpSpans::new(Duration::ZERO, 100);
        for id in 1..=3 {
            seen.record(&request(&[(1, id)])); // too old: each request starts a generation
        }
        let mut req = request(&[(1, 1), (1, 2), (1, 3)]);
        assert_eq!(seen.remove_seen(&mut req), 2);
        assert_eq!(left(&req), [1]);
    }
}
