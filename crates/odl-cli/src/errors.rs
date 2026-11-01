//! Error events of traces from an error tracker the host knows about.
//!
//! odl itself has none. A host that has one (`mallard`: the Sentry side)
//! gives the CLI ([`crate::Ctx::with_error_lookup`]) and the TUI
//! ([`crate::Embedded::set_error_lookup`]) an [`ErrorLookup`]; trace lists
//! then show an error count per trace and waterfalls mark the spans.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;

/// One error event of a trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEvent {
    pub event_id: String,
    pub issue_id: String,
    /// Span id (16 hex) the event was captured in.
    pub span_id: Option<String>,
    pub title: String,
}

/// The error events of one trace: `count` counts all of them, `events`
/// may hold only the newest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceErrors {
    pub count: u64,
    pub events: Vec<ErrorEvent>,
}

/// Error events per trace id. Traces without errors may be missing.
pub type ErrorMap = HashMap<String, TraceErrors>;

/// Looks up the error events of many traces in one go.
pub trait ErrorLookup: Send + Sync + std::fmt::Debug {
    /// The error events of `trace_ids` received between `from` and `to`.
    fn errors(
        &self,
        trace_ids: Vec<String>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> BoxFuture<'static, anyhow::Result<ErrorMap>>;
}
