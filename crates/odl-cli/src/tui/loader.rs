//! Background loading. Requests run on the tokio runtime; results come back
//! over a channel and are applied in `Embedded::tick`. Each request kind has a
//! generation number so stale responses (e.g. after a filter change) are
//! dropped, and a superseded request is aborted.
//!
//! Periodic refreshes use [`Loader::load`] with `force = false`: while a
//! request of that kind is still running nothing new is started, so a server
//! slower than the refresh interval still gets to deliver its answer and is
//! not flooded with duplicate queries.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc::{Receiver, Sender, channel},
};

use odl_client::api::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Stats,
    Services,
    Operations,
    SpanSeries,
    LogSeries,
    /// Log volume for the Logs view (filtered).
    LogVolume,
    Traces,
    Trace,
    TraceLogs,
    Logs,
    LogTail,
    MetricList,
    MetricQuery,
}

/// A loaded value.
pub enum Data {
    Stats(Stats),
    Services(Vec<ServiceSummary>),
    Operations(Vec<OperationSummary>),
    SpanSeries(SpanSeries),
    LogSeries(LogSeries),
    Traces(Vec<TraceSummary>),
    Trace(Box<Trace>),
    Logs(Vec<LogRecord>),
    /// New tail records and whether some may have been skipped.
    Tail((Vec<LogRecord>, bool)),
    MetricList(Vec<MetricInfo>),
    MetricQuery(MetricQueryResult),
}

pub struct Loaded {
    pub kind: Kind,
    pub result: Result<Data, String>,
}

pub struct Loader {
    tx: Sender<(Kind, u64, Result<Data, String>)>,
    rx: Receiver<(Kind, u64, Result<Data, String>)>,
    gens: HashMap<Kind, u64>,
    pending: HashSet<Kind>,
    tasks: HashMap<Kind, tokio::task::JoinHandle<()>>,
    /// Set when a load could not start because there is no tokio runtime.
    pub no_runtime: bool,
}

/// Requests started, for tests.
pub static STARTED: AtomicU64 = AtomicU64::new(0);

impl Default for Loader {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            gens: HashMap::new(),
            pending: HashSet::new(),
            tasks: HashMap::new(),
            no_runtime: false,
        }
    }
}

impl Loader {
    /// Start a request of `kind`, superseding (aborting) any earlier one of
    /// the same kind. Returns false when there is no tokio runtime to run it on.
    pub fn spawn<T, F>(&mut self, kind: Kind, fut: F, wrap: fn(T) -> Data) -> bool
    where
        T: Send + 'static,
        F: Future<Output = Result<T, odl_client::Error>> + Send + 'static,
    {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.no_runtime = true;
            return false;
        };
        self.no_runtime = false;
        let g = self.gens.entry(kind).or_default();
        *g += 1;
        let generation = *g;
        self.pending.insert(kind);
        let tx = self.tx.clone();
        STARTED.fetch_add(1, Ordering::Relaxed);
        let task = handle.spawn(async move {
            let res = fut.await.map(wrap).map_err(|e| short_error(&e));
            let _ = tx.send((kind, generation, res));
        });
        if let Some(old) = self.tasks.insert(kind, task) {
            old.abort();
        }
        true
    }

    /// Like [`Loader::spawn`], but when `force` is false and a request of this
    /// kind is still running, do nothing and report success: the running
    /// request will deliver fresh data.
    pub fn load<T, F>(&mut self, kind: Kind, force: bool, fut: F, wrap: fn(T) -> Data) -> bool
    where
        T: Send + 'static,
        F: Future<Output = Result<T, odl_client::Error>> + Send + 'static,
    {
        if !force && self.is_pending(kind) {
            return true;
        }
        self.spawn(kind, fut, wrap)
    }

    /// Abort in-flight requests of this kind (late results are ignored too).
    pub fn cancel(&mut self, kind: Kind) {
        *self.gens.entry(kind).or_default() += 1;
        self.pending.remove(&kind);
        if let Some(t) = self.tasks.remove(&kind) {
            t.abort();
        }
    }

    pub fn is_pending(&self, kind: Kind) -> bool {
        self.pending.contains(&kind)
    }

    pub fn any_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Results of the latest request per kind that arrived since last call.
    pub fn drain(&mut self) -> Vec<Loaded> {
        let mut out = Vec::new();
        while let Ok((kind, generation, result)) = self.rx.try_recv() {
            if self.gens.get(&kind).copied() == Some(generation) {
                self.pending.remove(&kind);
                self.tasks.remove(&kind);
                out.push(Loaded { kind, result });
            }
        }
        out
    }
}

fn short_error(e: &odl_client::Error) -> String {
    match e {
        odl_client::Error::Http(e) if e.is_connect() => "cannot connect to the server".to_string(),
        odl_client::Error::Http(e) if e.is_timeout() => "request timed out".to_string(),
        other => other.to_string(),
    }
}
