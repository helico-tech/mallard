//! Async client for the otel-ducklake query API, typed on [`odl_api`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), odl_client::Error> {
//! let c = odl_client::Client::new("http://localhost:8080", None)?;
//! let services = c.services(&odl_client::api::ServiceQuery { since: Some("1h".into()), ..Default::default() }).await?;
//! # Ok(()) }
//! ```

use std::{collections::HashSet, time::Duration};

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
    Api {
        status: u16,
        error: String,
        message: String,
    },
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
        Ok(Self {
            http,
            base,
            token: token.filter(|t| !t.is_empty()),
        })
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
            Err(_) => (
                "http".to_string(),
                if text.is_empty() {
                    status.to_string()
                } else {
                    text
                },
            ),
        };
        Err(Error::Api {
            status: status.as_u16(),
            error,
            message,
        })
    }

    async fn get<T: DeserializeOwned, Q: Serialize + ?Sized>(
        &self,
        path: &str,
        q: &Q,
    ) -> Result<T> {
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
        let body = SqlRequest {
            query: query.to_string(),
            limit,
        };
        Self::send(self.req(reqwest::Method::POST, "/api/v1/sql").json(&body)).await
    }
}

/// Follows new log records ("tail -f") by polling.
///
/// Records become visible after the server's flush interval and may arrive
/// late, so each poll looks back `lookback` and drops records already seen.
pub struct LogTail {
    client: Client,
    query: LogQuery,
    lookback: TimeDelta,
    cursor: DateTime<Utc>,
    seen: HashSet<(i64, u64)>,
}

impl LogTail {
    /// Start tailing from `start` (e.g. now minus a few minutes). Filters in
    /// `query` are kept; its window and order are managed by the tail.
    pub fn new(client: Client, query: LogQuery, start: DateTime<Utc>) -> Self {
        Self {
            client,
            query,
            lookback: TimeDelta::seconds(30),
            cursor: start,
            seen: HashSet::new(),
        }
    }

    /// How far each poll looks back for late records (default 30 s).
    pub fn lookback(mut self, d: Duration) -> Self {
        self.lookback = TimeDelta::from_std(d).unwrap_or(self.lookback);
        self
    }

    /// Fetch records not returned before, oldest first.
    pub async fn poll(&mut self) -> Result<Vec<LogRecord>> {
        let mut q = self.query.clone();
        q.from = Some(self.cursor - self.lookback);
        q.to = None;
        q.since = None;
        q.order = Some("asc".into());
        q.limit = Some(q.limit.unwrap_or(1000).min(5000));
        let records = self.client.logs(&q).await?;
        let mut out = Vec::new();
        for r in records {
            let key = (r.time.timestamp_micros(), fingerprint(&r));
            if self.seen.insert(key) {
                if r.time > self.cursor {
                    self.cursor = r.time;
                }
                out.push(r);
            }
        }
        // Forget keys older than the look-back window.
        let horizon = (self.cursor - self.lookback - TimeDelta::seconds(1)).timestamp_micros();
        self.seen.retain(|(t, _)| *t >= horizon);
        Ok(out)
    }
}

fn fingerprint(r: &LogRecord) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    r.service_name.hash(&mut h);
    r.body.hash(&mut h);
    r.span_id.hash(&mut h);
    r.severity_number.hash(&mut h);
    r.attributes.hash(&mut h);
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
