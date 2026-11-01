//! OTLP/HTTP receiver (port 4318 by default): `POST /v1/traces`, `/v1/logs`,
//! `/v1/metrics` with `application/x-protobuf` or `application/json` bodies,
//! optionally `Content-Encoding: gzip` or `zstd`.

use std::{io::Read, sync::Arc};

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Request as HttpRequest, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use opentelemetry_proto::tonic::collector::{
    logs::v1::{ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse},
    metrics::v1::{ExportMetricsPartialSuccess, ExportMetricsServiceRequest, ExportMetricsServiceResponse},
    trace::v1::{ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse},
};
use prost::Message;

use super::{Ingest, IngestError, Request};
use crate::store::schema::Signal;

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Proto,
    Json,
}

pub fn router(ingest: Arc<Ingest>) -> Router {
    // Compressed bodies are limited here, decompressed ones in `decode_body`.
    let limit = ingest.max_request_bytes;
    Router::new()
        .route("/v1/traces", post(traces))
        .route("/v1/logs", post(logs))
        .route("/v1/metrics", post(metrics))
        .layer(DefaultBodyLimit::max(limit))
        .layer(middleware::from_fn_with_state(ingest.clone(), admit))
        .with_state(ingest)
}

/// Runs before the body is read: check the token, then reserve memory for
/// the body so concurrent requests cannot exhaust it.
async fn admit(State(ingest): State<Arc<Ingest>>, req: HttpRequest, next: Next) -> Response {
    let headers = req.headers();
    let fmt = format(headers).unwrap_or(Format::Json);
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !ingest.tokens.allows(auth) {
        return error(StatusCode::UNAUTHORIZED, fmt, "invalid or missing bearer token".into());
    }
    let identity = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|e| e.trim().is_empty() || e.trim().eq_ignore_ascii_case("identity"));
    let len = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse().ok());
    let Some(_inflight) = ingest.admit(len.filter(|_| identity)) else {
        return error(StatusCode::TOO_MANY_REQUESTS, fmt, "too many requests in flight; retry later".into());
    };
    next.run(req).await
}

fn error(status: StatusCode, fmt: Format, msg: String) -> Response {
    // OTLP: error bodies are google.rpc.Status; a plain message is accepted by exporters.
    let mut resp = match fmt {
        Format::Json => {
            (status, [(header::CONTENT_TYPE, "application/json")], serde_json::json!({ "message": msg }).to_string())
                .into_response()
        }
        Format::Proto => (status, [(header::CONTENT_TYPE, "text/plain")], msg).into_response(),
    };
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
    }
    resp
}

fn format(headers: &HeaderMap) -> Result<Format, String> {
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/x-protobuf");
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "application/x-protobuf" | "application/protobuf" => Ok(Format::Proto),
        "application/json" => Ok(Format::Json),
        _ => Err(format!("unsupported content type {ct}")),
    }
}

/// Decompress (gzip/zstd/identity) with a size cap.
fn decode_body(headers: &HeaderMap, body: Bytes, max: usize) -> Result<Vec<u8>, String> {
    let enc = headers.get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()).unwrap_or("identity");
    let mut out = Vec::new();
    let reader: Box<dyn Read> = match enc.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => return Ok(body.to_vec()),
        "gzip" => Box::new(flate2::read::MultiGzDecoder::new(&body[..])),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(&body[..])),
        "zstd" => Box::new(zstd::stream::read::Decoder::new(&body[..]).map_err(|e| e.to_string())?),
        other => return Err(format!("unsupported content encoding {other}")),
    };
    reader.take(max as u64 + 1).read_to_end(&mut out).map_err(|e| format!("decompressing body: {e}"))?;
    if out.len() > max {
        return Err(format!("decompressed body larger than {max} bytes"));
    }
    Ok(out)
}

fn ok_response<T: Message + serde::Serialize>(fmt: Format, msg: &T) -> Response {
    match fmt {
        Format::Proto => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/x-protobuf")], msg.encode_to_vec()).into_response()
        }
        Format::Json => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(msg).unwrap_or_else(|_| "{}".into()),
        )
            .into_response(),
    }
}

async fn handle(ingest: Arc<Ingest>, signal: Signal, headers: HeaderMap, body: Bytes) -> Response {
    let fmt = match format(&headers) {
        Ok(f) => f,
        Err(m) => return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, Format::Json, m),
    };
    let max = ingest.max_request_bytes;
    // Decompress and decode off the async runtime.
    let decoded = tokio::task::spawn_blocking(move || -> Result<Request, String> {
        let raw = decode_body(&headers, body, max)?;
        let decode_err = |e: &dyn std::fmt::Display| format!("invalid OTLP {} payload: {e}", signal.as_str());
        Ok(match (signal, fmt) {
            (Signal::Traces, Format::Proto) => {
                Request::Traces(ExportTraceServiceRequest::decode(&raw[..]).map_err(|e| decode_err(&e))?)
            }
            (Signal::Traces, Format::Json) => {
                Request::Traces(serde_json::from_slice(&raw).map_err(|e| decode_err(&e))?)
            }
            (Signal::Logs, Format::Proto) => {
                Request::Logs(ExportLogsServiceRequest::decode(&raw[..]).map_err(|e| decode_err(&e))?)
            }
            (Signal::Logs, Format::Json) => Request::Logs(serde_json::from_slice(&raw).map_err(|e| decode_err(&e))?),
            (Signal::Metrics, Format::Proto) => {
                Request::Metrics(ExportMetricsServiceRequest::decode(&raw[..]).map_err(|e| decode_err(&e))?)
            }
            (Signal::Metrics, Format::Json) => {
                Request::Metrics(serde_json::from_slice(&raw).map_err(|e| decode_err(&e))?)
            }
        })
    })
    .await;
    let req = match decoded {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            ingest.stats.record_request(signal, 0, 0);
            return error(StatusCode::BAD_REQUEST, fmt, e);
        }
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, fmt, e.to_string()),
    };
    match ingest.handle(req).await {
        Ok(a) => match signal {
            Signal::Traces => ok_response(
                fmt,
                &ExportTraceServiceResponse {
                    partial_success: (a.rejected > 0).then_some(ExportTracePartialSuccess {
                        rejected_spans: a.rejected as i64,
                        error_message: a.message,
                    }),
                },
            ),
            Signal::Logs => ok_response(
                fmt,
                &ExportLogsServiceResponse {
                    partial_success: (a.rejected > 0).then_some(ExportLogsPartialSuccess {
                        rejected_log_records: a.rejected as i64,
                        error_message: a.message,
                    }),
                },
            ),
            Signal::Metrics => ok_response(
                fmt,
                &ExportMetricsServiceResponse {
                    partial_success: (a.rejected > 0).then_some(ExportMetricsPartialSuccess {
                        rejected_data_points: a.rejected as i64,
                        error_message: a.message,
                    }),
                },
            ),
        },
        Err(IngestError::Backpressure(m)) => error(StatusCode::TOO_MANY_REQUESTS, fmt, m),
        Err(IngestError::TooLarge(m)) => error(StatusCode::PAYLOAD_TOO_LARGE, fmt, m),
        Err(IngestError::Internal(m)) => error(StatusCode::INTERNAL_SERVER_ERROR, fmt, m),
    }
}

async fn traces(State(i): State<Arc<Ingest>>, h: HeaderMap, b: Bytes) -> Response {
    handle(i, Signal::Traces, h, b).await
}
async fn logs(State(i): State<Arc<Ingest>>, h: HeaderMap, b: Bytes) -> Response {
    handle(i, Signal::Logs, h, b).await
}
async fn metrics(State(i): State<Arc<Ingest>>, h: HeaderMap, b: Bytes) -> Response {
    handle(i, Signal::Metrics, h, b).await
}
