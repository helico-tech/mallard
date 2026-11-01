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

/// The same variants and helpers as `sdl_client::Error` (plus
/// [`Error::InvalidTraceId`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid base url: {0}")]
    InvalidUrl(String),
    #[error("invalid trace id {0:?}: must be 32 hex characters")]
    InvalidTraceId(String),
    /// Connection, timeout or decoding error; the cause is in `source()`.
    #[error("request failed")]
    Http(#[from] reqwest::Error),
    /// The server answered with an error status. `code` is the machine
    /// readable code (e.g. `bad_request`), or `http` when the body was not
    /// an API error.
    #[error("{status}: {message}")]
    Api { status: u16, code: String, message: String },
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

    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }

    /// The server refused the time window because it is larger than its
    /// `query.max_window`.
    ///
    /// The server reports this as a generic `bad_request`, so this matches
    /// its message. It is the only place that does.
    pub fn is_window_too_large(&self) -> bool {
        matches!(self, Error::Api { status: 400, message, .. } if message.contains("max_window"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Requests time out after this (the same in `sdl-client`).
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// `base_url` is where the API router is mounted, e.g. `http://localhost:8080`
    /// or `https://obs.example.com/otel`. An empty `token` means no token.
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self> {
        let parsed = url::Url::parse(base_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::InvalidUrl(format!("scheme must be http or https, not {:?}", parsed.scheme())));
        }
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("odl-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let token = token.filter(|t| !t.is_empty()).map(str::to_string);
        Ok(Self { http, base: base_url.trim_end_matches('/').to_string(), token })
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
        let (code, message) = match serde_json::from_str::<ApiError>(&text) {
            Ok(e) => (e.error, e.message),
            Err(_) => ("http".to_string(), fallback_message(&text, status)),
        };
        Err(Error::Api { status: status.as_u16(), code, message })
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
    /// `trace_id`: 32 hex characters; dashes (UUID form) are ignored.
    pub async fn trace(&self, trace_id: &str, q: &TraceGetQuery) -> Result<Trace> {
        let id = trace_id.replace('-', "");
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::InvalidTraceId(trace_id.to_string()));
        }
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

/// Message for an error response that is not an [`ApiError`] (e.g. a
/// proxy's HTML page): its first line, at most 200 characters.
fn fallback_message(body: &str, status: reqwest::StatusCode) -> String {
    let line = body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    if line.is_empty() {
        return status.to_string();
    }
    let mut msg: String = line.chars().take(200).collect();
    if msg.len() < line.len() {
        msg.push('…');
    }
    msg
}

/// Follows new log records ("tail -f") by polling.
///
/// Records become visible only after the server's flush interval and may
/// arrive late, so each poll looks back a little (`lookback`, default 30 s)
/// and drops what it returned before. From the second poll on, the window is
/// relative to the server's clock (`since`), so client clock skew does not
/// matter and the window stays small even when nothing matches for a long
/// time. The first poll starts at the `start` given to [`LogTail::new`],
/// which is on the caller's clock.
///
/// When more than `limit` records arrive between two polls only the newest
/// `limit` are returned (a tail shows what is happening now);
/// [`LogTail::last_poll_truncated`] then reports that some may be missing.
/// The same happens after a long pause or outage: a poll never looks back
/// more than [`MAX_GAP`] (or the look-back, when that is longer).
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

impl std::fmt::Debug for LogTail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogTail")
            .field("query", &self.query)
            .field("lookback", &self.lookback)
            .field("start", &self.start)
            .field("seen", &self.seen.len())
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

const MAX_SEEN: usize = 200_000;

/// Longest gap a poll tries to fill after a pause or outage.
pub const MAX_GAP: Duration = Duration::from_secs(10 * 60);

/// The `since` window of a poll that follows `elapsed` after the previous
/// one, and whether it had to be capped (records may be missing).
fn poll_window(elapsed: Duration, lookback: Duration) -> (Duration, bool) {
    let want = elapsed + lookback + Duration::from_secs(1);
    let cap = MAX_GAP.max(lookback + Duration::from_secs(1));
    if want > cap { (cap, true) } else { (want, false) }
}

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

    /// True when records between the last two polls may be missing: the
    /// page was full and did not reach back to records returned before, or
    /// the gap since the previous poll was longer than [`MAX_GAP`].
    pub fn last_poll_truncated(&self) -> bool {
        self.truncated
    }

    /// Fetch records not returned before, oldest first.
    pub async fn poll(&mut self) -> Result<Vec<LogRecord>> {
        let started = std::time::Instant::now();
        let mut q = self.query.clone();
        q.to = None;
        let mut capped = false;
        match self.last_poll {
            // Relative to the server's now: everything since the previous
            // poll, plus the look-back for late records.
            Some(prev) => {
                let (back, c) = poll_window(prev.elapsed(), self.lookback.to_std().unwrap_or_default());
                capped = c;
                q.from = None;
                q.since = Some(format!("{}ms", back.as_millis()));
            }
            None => {
                q.from = Some(self.start - self.lookback);
                q.since = None;
            }
        }
        q.order = Some(LogOrder::Desc);
        let limit = q.limit.unwrap_or(500).clamp(1, 5000);
        q.limit = Some(limit);
        let records = self.client.logs(&q).await?;
        // No await below this point: a cancelled poll changes nothing.
        let out = self.accept(records, limit);
        self.truncated |= capped;
        self.last_poll = Some(started);
        Ok(out)
    }

    /// Take one response (newest first, at most `limit` records): return the
    /// records not returned before, oldest first, and update `seen` and the
    /// truncation flag.
    fn accept(&mut self, mut records: Vec<LogRecord>, limit: u32) -> Vec<LogRecord> {
        let full = records.len() >= limit as usize;
        records.reverse();
        // A full page that still reaches back to a record returned before
        // overlaps the previous poll: nothing was skipped.
        self.truncated = full && records.first().is_some_and(|r| !self.seen.contains_key(&key(r)));
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
        out
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
        assert!(matches!(Client::new("localhost:8080", None), Err(Error::InvalidUrl(_))));
        assert!(matches!(Client::new("not a url", None), Err(Error::InvalidUrl(_))));
        let c = Client::new("http://x:8080/otel/", Some("")).unwrap();
        assert_eq!(c.base_url(), "http://x:8080/otel");
        assert!(c.token.is_none());
    }

    #[test]
    fn debug_hides_token() {
        let c = Client::new("http://x", Some("secret")).unwrap();
        let d = format!("{c:?}");
        assert!(!d.contains("secret") && d.contains("***"), "{d}");
    }

    #[tokio::test]
    async fn trace_id_is_validated() {
        let c = Client::new("http://127.0.0.1:9", None).unwrap();
        let q = TraceGetQuery::default();
        assert!(matches!(c.trace("xyz", &q).await, Err(Error::InvalidTraceId(_))));
        assert!(matches!(c.trace("0af7651916cd43dd8448eb211c80319c/x", &q).await, Err(Error::InvalidTraceId(_))));
        // Dashes are fine; this one fails only because nothing listens.
        assert!(matches!(c.trace("0af76519-16cd-43dd-8448-eb211c80319c", &q).await, Err(Error::Http(_))));
    }

    #[test]
    fn error_messages() {
        let html = "<html>\n<body>502 Bad Gateway</body></html>";
        assert_eq!(fallback_message(html, reqwest::StatusCode::BAD_GATEWAY), "<html>");
        let long = "x".repeat(500);
        assert_eq!(fallback_message(&long, reqwest::StatusCode::BAD_GATEWAY).chars().count(), 201);
        assert_eq!(fallback_message("", reqwest::StatusCode::BAD_GATEWAY), "502 Bad Gateway");
        let e = Error::Api {
            status: 400,
            code: "bad_request".into(),
            message: "time window larger than query.max_window (1day)".into(),
        };
        assert!(e.is_window_too_large());
        let e = Error::Api { status: 400, code: "bad_request".into(), message: "invalid since".into() };
        assert!(!e.is_window_too_large());
        assert!(!e.is_not_found());
        let e = Error::Api { status: 404, code: "not_found".into(), message: "no trace".into() };
        assert!(e.is_not_found() && e.status() == Some(404));
    }

    fn rec(micros: i64, body: &str) -> LogRecord {
        let t = DateTime::from_timestamp_micros(1_700_000_000_000_000 + micros).unwrap();
        LogRecord {
            time: t,
            observed_time: t,
            service_name: "api".into(),
            severity_number: 9,
            severity_text: "INFO".into(),
            body: body.into(),
            trace_id: None,
            span_id: None,
            event_name: String::new(),
            scope_name: String::new(),
            attributes: Default::default(),
            resource_attributes: Default::default(),
        }
    }

    fn tail() -> LogTail {
        LogTail::new(Client::new("http://127.0.0.1:9", None).unwrap(), LogQuery::default(), Utc::now())
    }

    /// Newest first, like the server returns them.
    fn page(range: std::ops::Range<i64>) -> Vec<LogRecord> {
        range.rev().map(|i| rec(i, &format!("r{i}"))).collect()
    }

    // Finding 1: a full page that overlaps what was returned before is not
    // a gap; only a full page with nothing seen before is.
    #[test]
    fn truncated_only_when_page_does_not_overlap() {
        let mut t = tail();
        assert_eq!(t.accept(page(0..10), 10).len(), 10);
        // Busy stream: the full page re-reads 5 old records (look-back).
        assert_eq!(t.accept(page(5..15), 10).len(), 5);
        assert!(!t.last_poll_truncated(), "overlapping full page reported as truncated");
        // A burst: the full page starts after everything seen.
        assert_eq!(t.accept(page(100..110), 10).len(), 10);
        assert!(t.last_poll_truncated());
        // A page that is not full is never truncated.
        t.accept(page(200..203), 10);
        assert!(!t.last_poll_truncated());
    }

    #[test]
    fn identical_records_and_mark_seen() {
        let mut t = tail();
        t.mark_seen(&[rec(1, "same")]);
        let out = t.accept(vec![rec(1, "same"), rec(1, "same")], 10);
        assert_eq!(out.len(), 1, "one more identical record");
        assert!(t.accept(vec![rec(1, "same"), rec(1, "same")], 10).is_empty());
    }

    #[test]
    fn seen_is_pruned_by_age() {
        let mut t = tail();
        t.accept(vec![rec(0, "old")], 10);
        // 10 minutes later: far beyond 2 x look-back + 5 min.
        t.accept(vec![rec(600_000_000, "new")], 10);
        assert_eq!(t.seen.len(), 1);
        // The old record would be returned again, but it is outside any
        // future window.
        assert!(t.seen.keys().all(|k| k.0 == key(&rec(600_000_000, "new")).0));
    }

    // Finding 8: after a long pause the window is capped (and reported as a
    // gap) instead of growing past the server's max_window.
    #[test]
    fn poll_window_is_capped() {
        let lb = Duration::from_secs(30);
        assert_eq!(poll_window(Duration::from_secs(2), lb), (Duration::from_secs(33), false));
        assert_eq!(poll_window(Duration::from_secs(48 * 3600), lb), (MAX_GAP, true));
        // A look-back longer than the cap still fits.
        let long = Duration::from_secs(3600);
        assert!(!poll_window(Duration::ZERO, long).1);
    }
}
