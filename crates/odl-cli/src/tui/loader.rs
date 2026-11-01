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
    /// New tail records; `truncated`: some before them may have been skipped.
    Tail {
        records: Vec<LogRecord>,
        truncated: bool,
    },
    MetricList(Vec<MetricInfo>),
    MetricQuery(MetricQueryResult),
}

pub struct Loaded {
    pub kind: Kind,
    pub result: Result<Data, odl_client::Error>,
}

type Msg = (Kind, u64, Result<Data, odl_client::Error>);

pub struct Loader {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    gens: HashMap<Kind, u64>,
    pending: HashSet<Kind>,
    tasks: HashMap<Kind, tokio::task::JoinHandle<()>>,
    /// Set when a load could not start because there is no tokio runtime.
    pub no_runtime: bool,
}

impl Default for Loader {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, gens: HashMap::new(), pending: HashSet::new(), tasks: HashMap::new(), no_runtime: false }
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
        let task = handle.spawn(async move {
            let res = fut.await.map(wrap);
            let _ = tx.send((kind, generation, res));
        });
        if let Some(old) = self.tasks.insert(kind, task) {
            old.abort();
        }
        true
    }

    /// Like [`Loader::spawn`], but when `force` is false and a request of this
    /// kind is still running, do nothing and report success: the running
    /// request will deliver fresh data. Callers must [`Loader::cancel`] the
    /// kind when its query parameters change, or that running request is a
    /// stale one.
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

    /// Cancel every kind except `keep`, also kinds added later.
    pub fn cancel_all_except(&mut self, keep: Kind) {
        let kinds: Vec<Kind> = self.gens.keys().copied().filter(|k| *k != keep).collect();
        for k in kinds {
            self.cancel(k);
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

/// One-line text for an error, for flashes and panels.
pub fn short_error(e: &odl_client::Error) -> String {
    match e {
        odl_client::Error::Http(e) if e.is_connect() => "cannot connect to the server".to_string(),
        odl_client::Error::Http(e) if e.is_timeout() => "request timed out".to_string(),
        odl_client::Error::Http(e) => format!("request failed: {e}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn ok(n: usize) -> Result<Vec<MetricInfo>, odl_client::Error> {
        Ok(Vec::with_capacity(n))
    }

    async fn drain_all(l: &mut Loader) -> Vec<Loaded> {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        l.drain()
    }

    #[tokio::test]
    async fn newest_generation_wins_and_cancel_drops_results() {
        let mut l = Loader::default();
        assert!(l.spawn(Kind::MetricList, ok(1), Data::MetricList));
        assert!(l.spawn(Kind::MetricList, ok(2), Data::MetricList));
        let got = drain_all(&mut l).await;
        assert_eq!(got.len(), 1, "the superseded request is aborted or dropped");
        assert!(!l.is_pending(Kind::MetricList));
        l.spawn(Kind::MetricList, ok(3), Data::MetricList);
        l.cancel(Kind::MetricList);
        assert!(drain_all(&mut l).await.is_empty(), "a cancelled result is ignored");
        assert!(!l.any_pending());
    }

    #[tokio::test]
    async fn load_skips_while_pending_unless_forced() {
        let mut l = Loader::default();
        let slow = async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(vec![])
        };
        l.spawn(Kind::MetricList, slow, Data::MetricList);
        let g = l.gens[&Kind::MetricList];
        assert!(l.load(Kind::MetricList, false, ok(1), Data::MetricList));
        assert_eq!(l.gens[&Kind::MetricList], g, "nothing new started while pending");
        assert!(l.load(Kind::MetricList, true, ok(1), Data::MetricList));
        assert_eq!(l.gens[&Kind::MetricList], g + 1);
        assert_eq!(drain_all(&mut l).await.len(), 1);
    }

    #[tokio::test]
    async fn cancel_all_except_keeps_one_kind() {
        let mut l = Loader::default();
        l.spawn(Kind::Stats, async { Err(odl_client::Error::InvalidUrl(String::new())) }, Data::Stats);
        l.spawn(Kind::MetricList, ok(1), Data::MetricList);
        l.cancel_all_except(Kind::Stats);
        let got = drain_all(&mut l).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, Kind::Stats);
    }

    #[test]
    fn no_runtime_is_reported() {
        let mut l = Loader::default();
        assert!(!l.spawn(Kind::MetricList, ok(1), Data::MetricList));
        assert!(l.no_runtime);
    }
}
