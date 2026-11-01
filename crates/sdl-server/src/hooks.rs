//! What an embedding application can plug into ingest, and the in-process
//! entry points it can call. mallard uses them to turn Sentry transactions
//! and span items into traces and OTel exceptions into issues
//! (docs/adr/0009-bridge.md, 0013).

use bytes::Bytes;
use serde_json::{Map, Value};
use uuid::Uuid;

/// A Sentry transaction item as received (envelope `transaction` item or a
/// legacy `/store/` transaction). Already authenticated.
#[derive(Debug, Clone)]
pub struct Transaction {
    pub project_id: u32,
    pub project_slug: String,
    /// The transaction's JSON, not parsed or checked.
    pub payload: Bytes,
}

/// Receives transactions instead of dropping them. Called on the request's
/// async task: return quickly and never block (hand heavy work to another
/// thread and drop what does not fit).
pub trait TransactionSink: Send + Sync + 'static {
    fn transaction(&self, t: Transaction);
}

/// An envelope item of type `span` as received: span streaming
/// (`content_type` `application/vnd.sentry.items.span.v2+json`, a container
/// with several spans) or a single standalone span from older SDKs.
/// Already authenticated.
#[derive(Debug, Clone)]
pub struct Spans<'a> {
    pub project_id: u32,
    pub project_slug: String,
    /// The item's `content_type` header, if any.
    pub content_type: Option<&'a str>,
    /// The envelope's headers (`trace` holds the release and environment
    /// of the trace, `sdk` the SDK), not checked. Borrowed: one envelope can
    /// hold many items, and its headers can be large.
    pub envelope_headers: &'a Map<String, Value>,
    /// The item's JSON, not parsed or checked.
    pub payload: &'a [u8],
}

/// Receives span items instead of dropping them. Same rules as
/// [`TransactionSink`]: called on the request's task, never block.
pub trait SpanSink: Send + Sync + 'static {
    fn spans(&self, s: Spans<'_>);
}

/// Told about every event from a Sentry SDK once it is stored, with the
/// trace and span it belongs to (`contexts.trace`). Events stored through
/// [`crate::Server::ingest_event`] are not reported. Must return quickly.
pub trait EventObserver: Send + Sync + 'static {
    fn event_stored(&self, project_id: u32, trace_id: Uuid, span_id: u64);
}

/// Why an event could not be stored. Ingest answers 429/503/500 for these.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IngestError {
    #[error("write queue full, retry later")]
    QueueFull,
    #[error("server is shutting down")]
    ShuttingDown,
    #[error("storing the event failed")]
    StoreFailed,
    #[error("event processing failed")]
    Internal,
    #[error("project not found")]
    UnknownProject,
}
