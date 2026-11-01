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
use tonic::{Request as TReq, Response, Status, codec::CompressionEncoding, service::Routes};

use super::{Accepted, Ingest, IngestError, Request};

#[derive(Clone)]
pub struct Otlp(pub Arc<Ingest>);

impl Otlp {
    fn check_auth<T>(&self, req: &TReq<T>) -> Result<(), Status> {
        let header = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok());
        if self.0.tokens.allows(header) {
            Ok(())
        } else {
            Err(Status::unauthenticated("invalid or missing bearer token"))
        }
    }

    async fn export(&self, req: Request) -> Result<Accepted, Status> {
        self.0.handle(req).await.map_err(|e| match e {
            // RESOURCE_EXHAUSTED is retryable for OTLP exporters.
            IngestError::Backpressure(m) => Status::resource_exhausted(m),
            IngestError::BadRequest(m) => Status::invalid_argument(m),
        })
    }
}

#[tonic::async_trait]
impl TraceService for Otlp {
    async fn export(
        &self,
        request: TReq<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        self.check_auth(&request)?;
        let a = Otlp::export(self, Request::Traces(request.into_inner())).await?;
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: (a.rejected > 0).then(|| ExportTracePartialSuccess {
                rejected_spans: a.rejected as i64,
                error_message: a.message,
            }),
        }))
    }
}

#[tonic::async_trait]
impl LogsService for Otlp {
    async fn export(
        &self,
        request: TReq<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        self.check_auth(&request)?;
        let a = Otlp::export(self, Request::Logs(request.into_inner())).await?;
        Ok(Response::new(ExportLogsServiceResponse {
            partial_success: (a.rejected > 0).then(|| ExportLogsPartialSuccess {
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
        self.check_auth(&request)?;
        let a = Otlp::export(self, Request::Metrics(request.into_inner())).await?;
        Ok(Response::new(ExportMetricsServiceResponse {
            partial_success: (a.rejected > 0).then(|| ExportMetricsPartialSuccess {
                rejected_data_points: a.rejected as i64,
                error_message: a.message,
            }),
        }))
    }
}

/// The three OTLP services, ready to serve or to merge into another tonic server.
pub fn routes(ingest: Arc<Ingest>) -> Routes {
    let max = ingest.max_request_bytes;
    let svc = Otlp(ingest);
    let mut b = Routes::builder();
    b.add_service(
        TraceServiceServer::new(svc.clone())
            .accept_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Zstd)
            .max_decoding_message_size(max),
    );
    b.add_service(
        LogsServiceServer::new(svc.clone())
            .accept_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Zstd)
            .max_decoding_message_size(max),
    );
    b.add_service(
        MetricsServiceServer::new(svc)
            .accept_compressed(CompressionEncoding::Gzip)
            .accept_compressed(CompressionEncoding::Zstd)
            .max_decoding_message_size(max),
    );
    b.routes()
}
