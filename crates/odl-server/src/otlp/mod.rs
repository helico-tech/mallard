//! OTLP receivers (gRPC and HTTP) feeding the write buffer.

pub mod convert;
pub mod grpc;
pub mod http;

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use opentelemetry_proto::tonic::collector::{
    logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest, trace::v1::ExportTraceServiceRequest,
};

use crate::{
    auth::Tokens,
    stats::Stats,
    store::{
        schema::Signal,
        writer::{Backpressure, WriteBuffer},
    },
};

/// Upper estimate of the Arrow size of a request: per row, the resource
/// attributes (copied to every row) plus a fixed overhead; plus a multiple of
/// the encoded payload for everything else.
pub fn estimate_expanded_bytes(req: &Request) -> u64 {
    use prost::Message;
    fn attr_bytes(r: Option<&opentelemetry_proto::tonic::resource::v1::Resource>) -> u64 {
        r.map(|r| r.attributes.iter().map(|kv| kv.encoded_len() as u64 + 16).sum()).unwrap_or(0)
    }
    const ROW: u64 = 96;
    let (per_resource, encoded): (u64, usize) = match req {
        Request::Traces(r) => (
            r.resource_spans
                .iter()
                .map(|rs| {
                    let n: u64 = rs.scope_spans.iter().map(|s| s.spans.len() as u64).sum();
                    n * (attr_bytes(rs.resource.as_ref()) + ROW)
                })
                .sum(),
            r.encoded_len(),
        ),
        Request::Logs(r) => (
            r.resource_logs
                .iter()
                .map(|rl| {
                    let n: u64 = rl.scope_logs.iter().map(|s| s.log_records.len() as u64).sum();
                    n * (attr_bytes(rl.resource.as_ref()) + ROW)
                })
                .sum(),
            r.encoded_len(),
        ),
        Request::Metrics(r) => {
            use opentelemetry_proto::tonic::metrics::v1::metric::Data;
            (
                r.resource_metrics
                    .iter()
                    .map(|rm| {
                        let n: u64 = rm
                            .scope_metrics
                            .iter()
                            .flat_map(|s| s.metrics.iter())
                            .map(|m| match &m.data {
                                Some(Data::Gauge(g)) => g.data_points.len(),
                                Some(Data::Sum(x)) => x.data_points.len(),
                                Some(Data::Histogram(x)) => x.data_points.len(),
                                Some(Data::ExponentialHistogram(x)) => x.data_points.len(),
                                Some(Data::Summary(x)) => x.data_points.len(),
                                None => 0,
                            } as u64)
                            .sum();
                        n * (attr_bytes(rm.resource.as_ref()) + ROW)
                    })
                    .sum(),
                r.encoded_len(),
            )
        }
    };
    per_resource + encoded as u64 * 3
}

/// Shared ingest state for both receivers.
pub struct Ingest {
    pub buffer: Arc<WriteBuffer>,
    pub stats: Arc<Stats>,
    pub tokens: Tokens,
    pub max_request_bytes: usize,
    /// Bounds the memory of requests being received and decoded, in KiB.
    inflight: Arc<Semaphore>,
    inflight_kib: u32,
}

/// A request's share of the in-flight budget; released when dropped.
#[derive(Clone)]
pub struct InFlight(#[allow(dead_code)] Arc<OwnedSemaphorePermit>);

/// Outcome of an accepted request.
pub struct Accepted {
    pub rejected: u64,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// Temporary overload; clients should retry.
    #[error("{0}")]
    Backpressure(String),
    /// The request is too large to ever be accepted.
    #[error("{0}")]
    TooLarge(String),
    #[error("{0}")]
    Internal(String),
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
    /// `max_inflight_bytes` bounds the bodies of requests being received
    /// and decoded at the same time (on top of the write buffer).
    pub fn new(
        buffer: Arc<WriteBuffer>,
        stats: Arc<Stats>,
        tokens: Tokens,
        max_request_bytes: usize,
        max_inflight_bytes: u64,
    ) -> Self {
        let kib = (max_inflight_bytes / 1024).clamp(1, u32::MAX as u64) as u32;
        Self {
            buffer,
            stats,
            tokens,
            max_request_bytes,
            inflight: Arc::new(Semaphore::new(kib as usize)),
            inflight_kib: kib,
        }
    }

    /// Reserve memory for a request about to be read, or `None` when too
    /// much is in flight already (the client should retry). `body_len` is
    /// the exact size of an uncompressed body, if known; otherwise the
    /// maximum request size is reserved.
    pub fn admit(&self, body_len: Option<u64>) -> Option<InFlight> {
        let max = self.max_request_bytes as u64;
        let bytes = body_len.map_or(max, |n| n.min(max));
        let kib = bytes.div_ceil(1024).clamp(1, self.inflight_kib as u64) as u32;
        self.inflight.clone().try_acquire_many_owned(kib).ok().map(|p| InFlight(Arc::new(p)))
    }

    /// Convert and buffer a request. Runs on a blocking thread because
    /// conversion is CPU work proportional to the payload.
    pub async fn handle(self: &Arc<Self>, req: Request) -> Result<Accepted, IngestError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.handle_blocking(req)).await.map_err(|e| {
            tracing::error!(error = %e, "ingest task failed");
            IngestError::Internal("internal error while converting the request".into())
        })?
    }

    pub fn handle_blocking(&self, req: Request) -> Result<Accepted, IngestError> {
        let signal = req.signal();
        // Resource attributes are repeated on every row, so a small request
        // can expand a lot. Refuse it before allocating.
        let estimate = estimate_expanded_bytes(&req);
        if estimate > self.buffer.max_bytes() {
            self.stats.record_request(signal, 0, 0);
            return Err(IngestError::TooLarge(format!(
                "request would expand to about {estimate} bytes in memory, more than \
                 ingest.max_buffer_bytes ({}); send smaller batches or fewer resource attributes",
                self.buffer.max_bytes()
            )));
        }
        let c = match &req {
            Request::Traces(r) => convert::convert_traces(r),
            Request::Logs(r) => convert::convert_logs(r),
            Request::Metrics(r) => convert::convert_metrics(r),
        };
        drop(req);
        let (accepted, rejected) = (c.accepted, c.rejected);
        if let Err(e) = self.buffer.push(c.batches) {
            self.stats.record_request(signal, 0, accepted + rejected);
            return Err(match e {
                Backpressure::Full { .. } => IngestError::Backpressure(e.to_string()),
                Backpressure::TooLarge { .. } => IngestError::TooLarge(e.to_string()),
            });
        }
        self.stats.record_request(signal, accepted, rejected);
        Ok(Accepted { rejected, message: c.reject_reason.unwrap_or_default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflight_budget_bounds_concurrent_requests() {
        let stats = Arc::new(Stats::default());
        let buffer = Arc::new(WriteBuffer::new(1 << 20, 1000, stats.clone()));
        let ingest = Ingest::new(buffer, stats, Tokens::default(), 32 * 1024, 64 * 1024);
        let a = ingest.admit(None).expect("first request fits");
        let _b = ingest.admit(Some(40 * 1024)).expect("sizes are capped at max_request_bytes");
        assert!(ingest.admit(Some(1)).is_none(), "budget used up");
        drop(a);
        let _small = ingest.admit(Some(100)).expect("released when dropped");
        let _small2 = ingest.admit(Some(100)).expect("small requests take little");
    }
}
