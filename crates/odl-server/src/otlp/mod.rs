//! OTLP receivers (gRPC and HTTP) feeding the write buffer.

pub mod convert;
pub mod grpc;
pub mod http;

use std::sync::Arc;

use opentelemetry_proto::tonic::collector::{
    logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
    trace::v1::ExportTraceServiceRequest,
};

use crate::{
    auth::Tokens,
    stats::Stats,
    store::{schema::Signal, writer::WriteBuffer},
};

/// Shared ingest state for both receivers.
pub struct Ingest {
    pub buffer: Arc<WriteBuffer>,
    pub stats: Arc<Stats>,
    pub tokens: Tokens,
    pub max_request_bytes: usize,
}

/// Outcome of an accepted request.
pub struct Accepted {
    pub rejected: u64,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("{0}")]
    Backpressure(String),
    #[error("{0}")]
    BadRequest(String),
}

/// A decoded OTLP request of any signal.
pub enum Request {
    Traces(ExportTraceServiceRequest),
    Logs(ExportLogsServiceRequest),
    Metrics(ExportMetricsServiceRequest),
}

impl Request {
    pub fn signal(&self) -> Signal {
        match self {
            Request::Traces(_) => Signal::Traces,
            Request::Logs(_) => Signal::Logs,
            Request::Metrics(_) => Signal::Metrics,
        }
    }
}

impl Ingest {
    /// Convert and buffer a request. Runs on a blocking thread because
    /// conversion is CPU work proportional to the payload.
    pub async fn handle(self: &Arc<Self>, req: Request) -> Result<Accepted, IngestError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.handle_blocking(req))
            .await
            .map_err(|e| IngestError::BadRequest(format!("ingest task failed: {e}")))?
    }

    pub fn handle_blocking(&self, req: Request) -> Result<Accepted, IngestError> {
        let signal = req.signal();
        let c = match &req {
            Request::Traces(r) => convert::convert_traces(r),
            Request::Logs(r) => convert::convert_logs(r),
            Request::Metrics(r) => convert::convert_metrics(r),
        };
        drop(req);
        let (accepted, rejected) = (c.accepted, c.rejected);
        if let Err(e) = self.buffer.push(c.batches) {
            self.stats.record_request(signal, 0, accepted + rejected);
            return Err(IngestError::Backpressure(e.to_string()));
        }
        self.stats.record_request(signal, accepted, rejected);
        Ok(Accepted {
            rejected,
            message: c.reject_reason.unwrap_or_default(),
        })
    }
}
