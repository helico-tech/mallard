//! The telemetry side's error lookup ([`mallard_otel_cli::ErrorLookup`]), answered
//! by the Sentry side: one `GET /sentry/v1/events/by-trace` per trace list.

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use mallard_client::Client;
use mallard_client::mallard_sentry_client::types::DeployQuery;
use mallard_client::mallard_sentry_client::types::WindowRange;
use mallard_otel_cli::{ErrorEvent, ErrorMap, TraceErrors};

#[derive(Debug, Clone)]
pub(crate) struct SentryErrors(pub Client);

impl mallard_otel_cli::ErrorLookup for SentryErrors {
    fn deploys(&self, project: Option<String>, since: String) -> BoxFuture<'static, anyhow::Result<Vec<f64>>> {
        let c = self.0.clone();
        Box::pin(async move {
            let q = DeployQuery { project, since: Some(since), limit: None };
            let r = c
                .sentry
                .deploys(&q)
                .await
                .map_err(|e| crate::described(mallard_client::Error::Sentry(e), c.base_url()))?;
            Ok(r.deploys.iter().map(|d| d.date_finished.timestamp() as f64).collect())
        })
    }

    fn errors(
        &self,
        trace_ids: Vec<String>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> BoxFuture<'static, anyhow::Result<ErrorMap>> {
        let c = self.0.clone();
        Box::pin(async move {
            let w = WindowRange { from: Some(from), to: Some(to), step: None };
            let r = c
                .sentry
                .errors_by_trace(&trace_ids, &w)
                .await
                .map_err(|e| crate::described(mallard_client::Error::Sentry(e), c.base_url()))?;
            Ok(r.traces
                .into_iter()
                .map(|(id, t)| {
                    let events = t
                        .events
                        .into_iter()
                        .map(|e| ErrorEvent {
                            event_id: e.event_id,
                            issue_id: e.issue_id,
                            span_id: e.span_id,
                            title: e.title,
                        })
                        .collect();
                    (id, TraceErrors { count: t.count, events })
                })
                .collect())
        })
    }
}
