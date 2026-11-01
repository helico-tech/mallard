//! Client for mallard. One base URL and token reach both libraries:
//! sentry-ducklake under `<url>/sentry` and otel-ducklake under `<url>/otel`.
//! It also correlates them by trace id.
//!
//! ```no_run
//! # async fn demo() -> Result<(), mallard_client::Error> {
//! let c = mallard_client::Client::new("http://localhost:8080", Some("token"))?;
//! let t = c.trace_overview("0af7651916cd43dd8448eb211c80319c", None).await?;
//! println!("{} spans, {} errors, {} logs", t.spans.len(), t.errors.len(), t.logs.len());
//! # Ok(()) }
//! ```

pub use mallard_otel_client;
pub use mallard_sentry_client;

use chrono::{DateTime, Utc};
use mallard_otel_client::api::{LogQuery, LogRecord, Span, TraceGetQuery};
use mallard_sentry_client::types::EventSummary;
use serde::Serialize;

/// The message already includes the inner error, so it is not also
/// exposed as `source()` (that would print it twice in error chains).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("sentry: {0}")]
    Sentry(mallard_sentry_client::Error),
    #[error("otel: {0}")]
    Otel(mallard_otel_client::Error),
    #[error("invalid trace id {0:?}: expected 32 hex characters")]
    InvalidTraceId(String),
}

impl From<mallard_sentry_client::Error> for Error {
    fn from(e: mallard_sentry_client::Error) -> Self {
        Error::Sentry(e)
    }
}

impl From<mallard_otel_client::Error> for Error {
    fn from(e: mallard_otel_client::Error) -> Self {
        Error::Otel(e)
    }
}

/// Default search window of [`Client::trace_overview`] for all three sources.
pub const DEFAULT_TRACE_WINDOW: &str = "7d";
/// Max log records fetched per trace.
pub const TRACE_LOG_LIMIT: u32 = 1000;

/// A W3C/OTel trace id: 32 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TraceId(String);

impl TraceId {
    /// Accepts 32 hex characters, or a UUID with dashes; any case.
    pub fn parse(s: &str) -> Result<TraceId, Error> {
        let hex: String = s.trim().chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase();
        if hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(TraceId(hex))
        } else {
            Err(Error::InvalidTraceId(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TraceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    pub sentry: mallard_sentry_client::Client,
    pub otel: mallard_otel_client::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).field("token", &self.token.as_ref().map(|_| "***")).finish()
    }
}

/// Everything known about one trace. A source that failed contributes an
/// empty list and a line in `warnings`.
#[derive(Debug, Clone, Serialize)]
pub struct TraceOverview {
    pub trace_id: TraceId,
    /// Empty when the trace has no spans (yet) in the window.
    pub spans: Vec<Span>,
    /// Sentry events captured in this trace.
    pub errors: Vec<EventSummary>,
    /// OTel log records with this trace id, oldest first.
    pub logs: Vec<LogRecord>,
    /// True when more than [`TRACE_LOG_LIMIT`] log records exist.
    pub logs_truncated: bool,
    /// Sources that could not be read, e.g. `"spans: otel: 400: window too large"`.
    pub warnings: Vec<String>,
}

impl Client {
    /// `base_url` is the server's `http_addr`, e.g. `http://localhost:8080`.
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self, Error> {
        let base = base_url.trim_end_matches('/').to_string();
        let token = token.filter(|t| !t.is_empty()).map(str::to_string);
        let sentry = mallard_sentry_client::Client::new(&format!("{base}/sentry"), token.as_deref())?;
        let otel = mallard_otel_client::Client::new(&format!("{base}/otel"), token.as_deref())?;
        Ok(Client { base, token, sentry, otel })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Spans, Sentry errors and logs of one trace, all searched over the
    /// same window (`since`, e.g. `24h`; default [`DEFAULT_TRACE_WINDOW`]).
    /// Fails only when the id is invalid or every source fails.
    pub async fn trace_overview(&self, trace_id: &str, since: Option<&str>) -> Result<TraceOverview, Error> {
        let since = since.unwrap_or(DEFAULT_TRACE_WINDOW).to_string();
        self.overview(trace_id, Some(since), None, None).await
    }

    /// [`Client::trace_overview`] over an absolute window (`from` inclusive,
    /// `to` exclusive).
    pub async fn trace_overview_in(
        &self,
        trace_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<TraceOverview, Error> {
        self.overview(trace_id, None, Some(from), Some(to)).await
    }

    async fn overview(
        &self,
        trace_id: &str,
        since: Option<String>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<TraceOverview, Error> {
        let id = TraceId::parse(trace_id)?;
        let spans = async {
            let q = TraceGetQuery { since: since.clone(), from, to };
            match self.otel.trace(id.as_str(), &q).await {
                Ok(t) => Ok(t.spans),
                Err(e) if e.status() == Some(404) => Ok(vec![]),
                Err(e) => Err(Error::from(e)),
            }
        };
        let errors = async {
            let q = mallard_sentry_client::types::TraceQuery {
                trace_id: id.to_string(),
                since: since.clone(),
                ..Default::default()
            };
            let w = mallard_sentry_client::types::WindowRange { from, to, step: None };
            Ok::<_, Error>(self.sentry.events_by_trace_in(&q, &w).await?.events)
        };
        let logs = async {
            let q = LogQuery {
                trace_id: Some(id.to_string()),
                since: since.clone(),
                from,
                to,
                order: Some(mallard_otel_client::api::LogOrder::Asc),
                limit: Some(TRACE_LOG_LIMIT + 1),
                ..Default::default()
            };
            Ok::<_, Error>(self.otel.logs(&q).await?)
        };
        let (spans, errors, logs) = tokio::join!(spans, errors, logs);
        let (spans, errors, logs) = match (spans, errors, logs) {
            (Err(e), Err(_), Err(_)) => return Err(e),
            all => all,
        };
        let mut warnings = Vec::new();
        let spans = or_warn(spans, "spans", &mut warnings);
        let errors = or_warn(errors, "errors", &mut warnings);
        let mut logs = or_warn(logs, "logs", &mut warnings);
        let logs_truncated = logs.len() > TRACE_LOG_LIMIT as usize;
        logs.truncate(TRACE_LOG_LIMIT as usize);
        Ok(TraceOverview { trace_id: id, spans, errors, logs, logs_truncated, warnings })
    }
}

/// The list, or an empty one plus a warning naming the failed source.
fn or_warn<T>(r: Result<Vec<T>, Error>, source: &str, warnings: &mut Vec<String>) -> Vec<T> {
    r.unwrap_or_else(|e| {
        warnings.push(format!("{source}: {e}"));
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_ids_are_validated_not_mangled() {
        let id = TraceId::parse("0AF7651916CD43DD8448EB211C80319C").unwrap();
        assert_eq!(id.as_str(), "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(TraceId::parse("0af76519-16cd-43dd-8448-eb211c80319c").unwrap(), id);
        assert!(TraceId::parse("abc-xyz").is_err());
        assert!(TraceId::parse("0af7651916cd43dd8448eb211c80319").is_err(), "31 chars");
        assert!(TraceId::parse("0af7651916cd43dd8448eb211c80319g").is_err(), "non-hex");
    }

    #[test]
    fn debug_hides_the_token() {
        let c = Client::new("http://localhost:8080", Some("s3cret")).unwrap();
        assert!(!format!("{c:?}").contains("s3cret"));
    }
}
