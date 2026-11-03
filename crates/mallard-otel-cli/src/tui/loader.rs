//! Background loading. Requests run on the tokio runtime; results come back
//! over a channel and are applied in `Embedded::tick`. Each request kind has a
//! generation number so stale responses (e.g. after a filter change) are
//! dropped, and a superseded request is aborted.
//!
//! Every response is cleaned here, once, on its background task (ADR 0006,
//! [`Data::cleaned`]), so a view draws only clean text and cannot forget it. What goes
//! back to the server must stay as the server has it, so a row that can be drilled into
//! keeps its original name ([`Named`]).
//!
//! Periodic refreshes use [`Loader::load`] with `force = false`: while a
//! request of that kind is still running nothing new is started, so a server
//! slower than the refresh interval still gets to deliver its answer and is
//! not flooded with duplicate queries.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    ops::Deref,
    sync::mpsc::{Receiver, Sender, channel},
};

use mallard_otel_client::{LogTail, api::*};
use mallard_term::text::{clean, sanitize};
use serde::{Serialize, de::DeserializeOwned};

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
    /// Deploys for the charts' markers (the host's error lookup).
    Deploys,
    /// Error events of the listed traces (the host's error lookup).
    TraceErrors,
    /// Error events of the open trace.
    SpanErrors,
    /// Keys or values for the filter bar. Quiet: it fails as data and shows no spinner.
    Suggest,
}

/// A row of a list, cleaned for display, with the name it has on the server. Views read the
/// row's fields as usual. A filter or query sends `raw`: a cleaned name matches nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Named<T> {
    pub raw: String,
    row: T,
}

impl<T> Deref for Named<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.row
    }
}

impl<T: HasName> From<T> for Named<T> {
    fn from(row: T) -> Self {
        Named { raw: row.name().to_string(), row }
    }
}

/// A row whose name goes back to the server.
pub trait HasName {
    fn name(&self) -> &str;
}

impl HasName for ServiceSummary {
    fn name(&self) -> &str {
        &self.name
    }
}

impl HasName for OperationSummary {
    fn name(&self) -> &str {
        &self.name
    }
}

impl HasName for MetricInfo {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Rows as they came, each with its name.
pub fn named<T: HasName>(rows: Vec<T>) -> Vec<Named<T>> {
    rows.into_iter().map(Named::from).collect()
}

fn cleaned_rows<T: Serialize + DeserializeOwned>(rows: Vec<Named<T>>) -> Result<Vec<Named<T>>, serde_json::Error> {
    rows.into_iter().map(|Named { raw, row }| Ok(Named { raw, row: sanitize(row, false)? })).collect()
}

/// A loaded value.
pub enum Data {
    Stats(Box<Stats>),
    Services(Vec<Named<ServiceSummary>>),
    Operations(Vec<Named<OperationSummary>>),
    SpanSeries(SpanSeries),
    LogSeries(LogSeries),
    Traces(Vec<TraceSummary>),
    Trace(Box<Trace>),
    /// The logs of the open trace.
    Logs(Vec<LogRecord>),
    /// The first page of the log list, and the tail that goes on from it.
    LogPage {
        records: Vec<LogRecord>,
        tail: Box<LogTail>,
    },
    /// New tail records; `truncated`: some before them may have been skipped.
    Tail {
        records: Vec<LogRecord>,
        truncated: bool,
    },
    MetricList(Vec<Named<MetricInfo>>),
    MetricQuery(MetricQueryResult),
    /// From the host's error lookup, which fails in its own way.
    Errors(Result<crate::ErrorMap, String>),
    Deploys(Result<Vec<f64>, String>),
    Suggest(Box<super::suggestions::Answer>),
}

impl Data {
    /// All server text cleaned for display. No wildcard here: a new kind of data has to say
    /// how it is cleaned. Text with views that split it into lines keeps its line breaks:
    /// log bodies and stack traces.
    fn cleaned(self) -> Result<Self, serde_json::Error> {
        Ok(match self {
            Data::Stats(v) => Data::Stats(sanitize(v, false)?),
            Data::Services(v) => Data::Services(cleaned_rows(v)?),
            Data::Operations(v) => Data::Operations(cleaned_rows(v)?),
            Data::SpanSeries(v) => Data::SpanSeries(sanitize(v, false)?),
            Data::LogSeries(v) => Data::LogSeries(sanitize(v, false)?),
            Data::Traces(v) => Data::Traces(sanitize(v, false)?),
            Data::Trace(v) => Data::Trace(sanitize(v, true)?),
            Data::Logs(v) => Data::Logs(sanitize(v, true)?),
            Data::LogPage { records, tail } => Data::LogPage { records: sanitize(records, true)?, tail },
            Data::Tail { records, truncated } => Data::Tail { records: sanitize(records, true)?, truncated },
            Data::MetricList(v) => Data::MetricList(cleaned_rows(v)?),
            Data::MetricQuery(v) => Data::MetricQuery(sanitize(v, false)?),
            // The event and span ids are compared and handed to the host; the view cleans them.
            // Numbers: nothing to clean.
            d @ Data::Deploys(_) => d,
            Data::Errors(r) => Data::Errors(r.map(|mut map| {
                for e in map.values_mut().flat_map(|t| &mut t.events) {
                    e.title = clean(&e.title).into_owned();
                    e.issue_id = clean(&e.issue_id).into_owned();
                }
                map
            })),
            // `Suggest` keeps it raw, to insert it, and cleans what it shows.
            Data::Suggest(a) => Data::Suggest(a),
        })
    }
}

/// A response that cannot be cleaned is never shown raw. JSON from a server cannot get
/// here, because it has nothing `sanitize` refuses.
fn unshowable(e: serde_json::Error) -> mallard_otel_client::Error {
    mallard_otel_client::Error::Api {
        status: 0,
        code: "unshowable".into(),
        message: format!("cannot show the response: {e}"),
    }
}

pub struct Loaded {
    pub kind: Kind,
    pub result: Result<Data, mallard_otel_client::Error>,
}

type Msg = (Kind, u64, Result<Data, mallard_otel_client::Error>);

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
        F: Future<Output = Result<T, mallard_otel_client::Error>> + Send + 'static,
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
            let res = fut.await.map(wrap).and_then(|d| d.cleaned().map_err(unshowable));
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
        F: Future<Output = Result<T, mallard_otel_client::Error>> + Send + 'static,
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
        self.pending.iter().any(|k| *k != Kind::Suggest)
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

/// One-line text for an error, for flashes and panels. A server's message is server text.
pub fn short_error(e: &mallard_otel_client::Error) -> String {
    let text = match e {
        mallard_otel_client::Error::Http(e) if e.is_connect() => "cannot connect to the server".to_string(),
        mallard_otel_client::Error::Http(e) if e.is_timeout() => "request timed out".to_string(),
        mallard_otel_client::Error::Http(e) => format!("request failed: {e}"),
        other => other.to_string(),
    };
    clean(&text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn ok(n: usize) -> Result<Vec<Named<MetricInfo>>, mallard_otel_client::Error> {
        Ok(named(Vec::with_capacity(n)))
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
        l.spawn(Kind::Stats, async { Err(mallard_otel_client::Error::InvalidUrl(String::new())) }, |s| {
            Data::Stats(Box::new(s))
        });
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
