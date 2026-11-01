//! OTLP/gRPC services (port 4317 by default).

use std::sync::Arc;

use opentelemetry_proto::tonic::collector::{
    logs::v1::{
        ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse,
        logs_service_server::{LogsService, LogsServiceServer},
    },
    metrics::v1::{
        ExportMetricsPartialSuccess, ExportMetricsServiceRequest, ExportMetricsServiceResponse,
        metrics_service_server::{MetricsService, MetricsServiceServer},
    },
    trace::v1::{
        ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
        trace_service_server::{TraceService, TraceServiceServer},
    },
};
use tonic::{
    Request as TReq, Response, Status,
    codec::CompressionEncoding,
    service::{Routes, interceptor::InterceptedService},
};

use super::{Accepted, InFlight, Ingest, IngestError, Request};

#[derive(Clone)]
struct Otlp(Arc<Ingest>);

/// Runs on the request metadata, before the message is decompressed and
/// decoded: check the token, then reserve memory for the message. The
/// reservation travels in the request extensions.
fn admit(ingest: &Ingest, mut req: TReq<()>) -> Result<TReq<()>, Status> {
    let header = req.metadata().get("authorization").and_then(|v| v.to_str().ok());
    if !ingest.tokens.allows(header) {
        return Err(Status::unauthenticated("invalid or missing bearer token"));
    }
    // The message size is not known yet: reserve the maximum.
    let inflight = ingest.admit(None).ok_or_else(|| Status::unavailable("too many requests in flight; retry later"))?;
    req.extensions_mut().insert(inflight);
    Ok(req)
}

/// The in-flight reservation made by [`admit`]; keep it until the request is handled.
fn inflight<T>(req: &TReq<T>) -> Option<InFlight> {
    req.extensions().get::<InFlight>().cloned()
}

impl Otlp {
    async fn export(&self, req: Request) -> Result<Accepted, Status> {
        self.0.handle(req).await.map_err(|e| match e {
            // UNAVAILABLE is retried by every OTLP exporter. RESOURCE_EXHAUSTED
            // is only retried when it carries RetryInfo, which the Collector
            // and the Go SDK require.
            IngestError::Backpressure(m) => Status::unavailable(m),
            // Not retryable: the request will never fit.
            IngestError::TooLarge(m) => Status::invalid_argument(m),
            IngestError::Internal(m) => Status::internal(m),
        })
    }
}

#[tonic::async_trait]
impl TraceService for Otlp {
    async fn export(
        &self,
        request: TReq<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let _inflight = inflight(&request);
        let a = Otlp::export(self, Request::Traces(request.into_inner())).await?;
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: (a.rejected > 0)
                .then_some(ExportTracePartialSuccess { rejected_spans: a.rejected as i64, error_message: a.message }),
        }))
    }
}

#[tonic::async_trait]
impl LogsService for Otlp {
    async fn export(
        &self,
        request: TReq<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        let _inflight = inflight(&request);
        let a = Otlp::export(self, Request::Logs(request.into_inner())).await?;
        Ok(Response::new(ExportLogsServiceResponse {
            partial_success: (a.rejected > 0).then_some(ExportLogsPartialSuccess {
                rejected_log_records: a.rejected as i64,
                error_message: a.message,
            }),
        }))
    }
}

#[tonic::async_trait]
impl MetricsService for Otlp {
    async fn export(
        &self,
        request: TReq<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        let _inflight = inflight(&request);
        let a = Otlp::export(self, Request::Metrics(request.into_inner())).await?;
        Ok(Response::new(ExportMetricsServiceResponse {
            partial_success: (a.rejected > 0).then_some(ExportMetricsPartialSuccess {
                rejected_data_points: a.rejected as i64,
                error_message: a.message,
            }),
        }))
    }
}

/// The three OTLP services, ready to serve or to merge into another tonic server.
pub fn routes(ingest: Arc<Ingest>) -> Routes {
    let max = ingest.max_request_bytes;
    let i = ingest.clone();
    let check = move |req| admit(&i, req);
    let svc = Otlp(ingest);
    let mut b = Routes::builder();
    b.add_service(InterceptedService::new(
        TraceServiceServer::new(svc.clone())
            .accept_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Zstd)
            .max_decoding_message_size(max),
        check.clone(),
    ));
    b.add_service(InterceptedService::new(
        LogsServiceServer::new(svc.clone())
            .accept_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Zstd)
            .max_decoding_message_size(max),
        check.clone(),
    ));
    b.add_service(InterceptedService::new(
        MetricsServiceServer::new(svc)
            .accept_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Zstd)
            .max_decoding_message_size(max),
        check,
    ));
    b.routes()
}
