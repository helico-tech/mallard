//! Async client for the otel-ducklake query API, typed on [`odl_api`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), odl_client::Error> {
//! let c = odl_client::Client::new("http://localhost:8080", None)?;
//! let services = c.services(&odl_client::api::ServiceQuery { since: Some("1h".into()), ..Default::default() }).await?;
//! # Ok(()) }
//! ```

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
pub use odl_api as api;
use odl_api::*;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid base URL {0:?}")]
    BadUrl(String),
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{status}: {message}")]
    Api { status: u16, error: String, message: String },
}

impl Error {
    /// HTTP status for API errors.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            Error::Http(e) => e.status().map(|s| s.as_u16()),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Client {
    /// `base_url` is where the API router is mounted, e.g. `http://localhost:8080`
    /// or `https://obs.example.com/otel`.
    pub fn new(base_url: &str, token: Option<String>) -> Result<Self> {
        let base = base_url.trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::BadUrl(base_url.to_string()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("odl-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, base, token: token.filter(|t| !t.is_empty()) })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self.http.request(m, format!("{}{}", self.base, path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    async fn send<T: DeserializeOwned>(r: reqwest::RequestBuilder) -> Result<T> {
        let resp = r.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json().await?);
        }
        let text = resp.text().await.unwrap_or_default();
        let (error, message) = match serde_json::from_str::<ApiError>(&text) {
            Ok(e) => (e.error, e.message),
            Err(_) => ("http".to_string(), if text.is_empty() { status.to_string() } else { text }),
        };
        Err(Error::Api { status: status.as_u16(), error, message })
    }

    async fn get<T: DeserializeOwned, Q: Serialize + ?Sized>(&self, path: &str, q: &Q) -> Result<T> {
        Self::send(self.req(reqwest::Method::GET, path).query(q)).await
    }

    pub async fn health(&self) -> Result<Health> {
        self.get("/api/v1/health", &()).await
    }
    pub async fn stats(&self) -> Result<Stats> {
        self.get("/api/v1/stats", &()).await
    }
    pub async fn services(&self, q: &ServiceQuery) -> Result<Vec<ServiceSummary>> {
        self.get("/api/v1/services", q).await
    }
    pub async fn operations(&self, q: &OperationQuery) -> Result<Vec<OperationSummary>> {
        self.get("/api/v1/operations", q).await
    }
    pub async fn span_series(&self, q: &SpanSeriesQuery) -> Result<SpanSeries> {
        self.get("/api/v1/spans/series", q).await
    }
    pub async fn traces(&self, q: &TraceQuery) -> Result<Vec<TraceSummary>> {
        self.get("/api/v1/traces", q).await
    }
    pub async fn trace(&self, trace_id: &str, q: &TraceGetQuery) -> Result<Trace> {
        let id: String = trace_id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        self.get(&format!("/api/v1/traces/{id}"), q).await
    }
    pub async fn logs(&self, q: &LogQuery) -> Result<Vec<LogRecord>> {
        self.get("/api/v1/logs", q).await
    }
    pub async fn log_series(&self, q: &LogSeriesQuery) -> Result<LogSeries> {
        self.get("/api/v1/logs/series", q).await
    }
    pub async fn metrics(&self, q: &MetricListQuery) -> Result<Vec<MetricInfo>> {
        self.get("/api/v1/metrics", q).await
    }
    pub async fn metric_query(&self, q: &MetricQuery) -> Result<MetricQueryResult> {
        self.get("/api/v1/metrics/query", q).await
    }
    pub async fn sql(&self, query: &str, limit: Option<u32>) -> Result<SqlResponse> {
        let body = SqlRequest { query: query.to_string(), limit };
        Self::send(self.req(reqwest::Method::POST, "/api/v1/sql").json(&body)).await
    }
}

/// Follows new log records ("tail -f") by polling.
///
/// Records become visible only after the server's flush interval and may
/// arrive late, so each poll looks back a little (`lookback`, default 30 s)
/// and drops what it returned before. The window of each poll is relative to
/// the server's clock (`since`), so client clock skew does not matter and the
/// window stays small even when nothing matches for a long time.
///
/// When more than `limit` records arrive between two polls only the newest
/// `limit` are returned (a tail shows what is happening now);
/// [`LogTail::last_poll_truncated`] then reports that some may be missing.
pub struct LogTail {
    client: Client,
    query: LogQuery,
    lookback: TimeDelta,
    /// Start of the first poll's window.
    start: DateTime<Utc>,
    /// When the last successful poll started (client clock, monotonic).
    last_poll: Option<std::time::Instant>,
    /// Records returned so far, counted per identity, for recent times.
    seen: HashMap<(i64, u64), usize>,
    truncated: bool,
}

const MAX_SEEN: usize = 200_000;

impl LogTail {
    /// Start tailing from `start` (e.g. now minus a few minutes). Filters in
    /// `query` are kept (`limit` caps each poll, default 500); its window and
    /// order are managed by the tail.
    pub fn new(client: Client, query: LogQuery, start: DateTime<Utc>) -> Self {
        Self {
            client,
            query,
            lookback: TimeDelta::seconds(30),
            start,
            last_poll: None,
            seen: HashMap::new(),
            truncated: false,
        }
    }

    /// How far each poll looks back for late records (default 30 s).
    pub fn lookback(mut self, d: Duration) -> Self {
        self.lookback = TimeDelta::from_std(d).unwrap_or(self.lookback);
        self
    }

    /// Treat these records as already returned (e.g. an initial page the
    /// caller loaded itself), so `poll` does not repeat them.
    pub fn mark_seen(&mut self, records: &[LogRecord]) {
        let mut counts: HashMap<(i64, u64), usize> = HashMap::new();
        for r in records {
            *counts.entry(key(r)).or_default() += 1;
        }
        for (k, n) in counts {
            let e = self.seen.entry(k).or_default();
            *e = (*e).max(n);
        }
    }

    /// True when the last poll returned a full page: older new records may
    /// have been skipped.
    pub fn last_poll_truncated(&self) -> bool {
        self.truncated
    }

    /// Fetch records not returned before, oldest first.
    pub async fn poll(&mut self) -> Result<Vec<LogRecord>> {
        let started = std::time::Instant::now();
        let mut q = self.query.clone();
        q.to = None;
        match self.last_poll {
            // Relative to the server's now: everything since the previous
            // poll, plus the look-back for late records.
            Some(prev) => {
                let back = prev.elapsed() + self.lookback.to_std().unwrap_or_default() + Duration::from_secs(1);
                q.from = None;
                q.since = Some(format!("{}ms", back.as_millis()));
            }
            None => {
                q.from = Some(self.start - self.lookback);
                q.since = None;
            }
        }
        q.order = Some("desc".into());
        let limit = q.limit.unwrap_or(500).clamp(1, 5000);
        q.limit = Some(limit);
        let mut records = self.client.logs(&q).await?;
        self.truncated = records.len() as u32 >= limit;
        records.reverse();
        // Multiset difference: a record identical to one seen before is new
        // when it occurs more often in this response than seen so far.
        let mut in_response: HashMap<(i64, u64), usize> = HashMap::new();
        let mut out = Vec::new();
        for r in records {
            let k = key(&r);
            let n = in_response.entry(k).or_default();
            *n += 1;
            if *n > self.seen.get(&k).copied().unwrap_or(0) {
                out.push(r);
            }
        }
        for (k, n) in in_response {
            let e = self.seen.entry(k).or_default();
            *e = (*e).max(n);
        }
        self.last_poll = Some(started);
        // Forget identities far older than any future poll window.
        if let Some(newest) = self.seen.keys().map(|k| k.0).max() {
            let horizon = newest - (self.lookback * 2 + TimeDelta::minutes(5)).num_microseconds().unwrap_or(i64::MAX);
            self.seen.retain(|k, _| k.0 >= horizon);
        }
        if self.seen.len() > MAX_SEEN {
            let mut times: Vec<i64> = self.seen.keys().map(|k| k.0).collect();
            times.sort_unstable();
            let cut = times[times.len() - MAX_SEEN / 2];
            self.seen.retain(|k, _| k.0 >= cut);
        }
        Ok(out)
    }
}

/// Identity of a log record: time plus a hash of its content.
fn key(r: &LogRecord) -> (i64, u64) {
    (r.time.timestamp_micros(), fingerprint(r))
}

fn fingerprint(r: &LogRecord) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    r.observed_time.timestamp_micros().hash(&mut h);
    r.service_name.hash(&mut h);
    r.body.hash(&mut h);
    r.trace_id.hash(&mut h);
    r.span_id.hash(&mut h);
    r.severity_number.hash(&mut h);
    r.severity_text.hash(&mut h);
    r.event_name.hash(&mut h);
    r.attributes.hash(&mut h);
    r.resource_attributes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_validation() {
        assert!(Client::new("localhost:8080", None).is_err());
        let c = Client::new("http://x:8080/otel/", Some(String::new())).unwrap();
        assert_eq!(c.base_url(), "http://x:8080/otel");
        assert!(c.token.is_none());
    }
}
