//! Client for mallard. One base URL and token reach both libraries:
//! sentry-ducklake under `<url>/sentry` and otel-ducklake under `<url>/otel`.
//! It also correlates them by trace id.
//!
//! ```no_run
//! # async fn demo() -> Result<(), mallard_client::Error> {
//! let c = mallard_client::Client::new("http://localhost:8080", Some("token"))?;
//! let t = c.trace_overview("0af7651916cd43dd8448eb211c80319c").await?;
//! println!("{} spans, {} errors, {} logs", t.spans.len(), t.errors.len(), t.logs.len());
//! # Ok(()) }
//! ```

pub use odl_client;
pub use sdl_client;

use odl_client::api::{LogQuery, LogRecord, Span, TraceGetQuery};
use sdl_client::types::EventSummary;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sentry: {0}")]
    Sentry(#[from] sdl_client::Error),
    #[error("otel: {0}")]
    Otel(#[from] odl_client::Error),
}

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    pub sentry: sdl_client::Client,
    pub otel: odl_client::Client,
}

/// Everything known about one trace.
#[derive(Debug, Clone, Serialize)]
pub struct TraceOverview {
    pub trace_id: String,
    /// Empty when the trace has no spans (yet) in the window.
    pub spans: Vec<Span>,
    /// Sentry events captured in this trace.
    pub errors: Vec<EventSummary>,
    /// OTel log records with this trace id, oldest first.
    pub logs: Vec<LogRecord>,
}

impl Client {
    /// `base_url` is the server's `http_addr`, e.g. `http://localhost:8080`.
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self, Error> {
        let base = base_url.trim_end_matches('/').to_string();
        let token = token.filter(|t| !t.is_empty()).map(str::to_string);
        let sentry = sdl_client::Client::new(&format!("{base}/sentry"), token.as_deref().unwrap_or(""))?;
        let otel = odl_client::Client::new(&format!("{base}/otel"), token.clone())?;
        Ok(Client { base, token, sentry, otel })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Spans, Sentry errors and logs of one trace. `since` bounds the search
    /// window (e.g. `24h`, `7d`); default: each library's default.
    pub async fn trace_overview_since(&self, trace_id: &str, since: Option<&str>) -> Result<TraceOverview, Error> {
        let id: String = trace_id.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_lowercase();
        let since = since.map(str::to_string);
        let spans = async {
            let q = TraceGetQuery { since: since.clone(), ..Default::default() };
            match self.otel.trace(&id, &q).await {
                Ok(t) => Ok(t.spans),
                Err(e) if e.status() == Some(404) => Ok(vec![]),
                Err(e) => Err(Error::from(e)),
            }
        };
        let errors = async {
            let q = sdl_client::types::TraceQuery { trace_id: id.clone(), since: since.clone(), ..Default::default() };
            Ok::<_, Error>(self.sentry.events_by_trace_query(&q).await?.events)
        };
        let logs = async {
            let q = LogQuery {
                trace_id: Some(id.clone()),
                since: Some(since.clone().unwrap_or_else(|| "7d".into())),
                order: Some("asc".into()),
                limit: Some(1000),
                ..Default::default()
            };
            Ok::<_, Error>(self.otel.logs(&q).await?)
        };
        let (spans, errors, logs) = tokio::try_join!(spans, errors, logs)?;
        Ok(TraceOverview { trace_id: id, spans, errors, logs })
    }

    pub async fn trace_overview(&self, trace_id: &str) -> Result<TraceOverview, Error> {
        self.trace_overview_since(trace_id, None).await
    }
}
