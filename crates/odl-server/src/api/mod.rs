//! Query API (`/api/v1/...`) with an OpenAPI document at `/openapi.json` and
//! interactive docs at `/docs`. All paths are relative to wherever the router
//! is mounted, so an embedding application can `nest` it.

use std::{sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use odl_api::*;
use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::Tokens,
    query::{
        QueryError, Reader, Window, logs, metrics, sql, storage,
        traces::{self, clamp_limit},
    },
    stats::Stats,
};

/// State shared by API handlers.
pub struct ApiState {
    pub reader: Arc<Reader>,
    pub stats: Arc<Stats>,
    pub tokens: Tokens,
    pub schema: String,
    pub catalog_type: String,
    pub data_path: String,
    pub max_buffer_bytes: u64,
    pub enable_sql: bool,
    pub started: Instant,
}

type S = State<Arc<ApiState>>;

const HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

#[derive(OpenApi)]
#[openapi(
    info(
        title = "otel-ducklake query API",
        description = "Query traces, logs and metrics stored in DuckLake by otel-ducklake.",
        license(name = "Apache-2.0")
    ),
    modifiers(&BearerAuth),
    tags(
        (name = "system", description = "Health and statistics"),
        (name = "traces", description = "Services, operations and traces"),
        (name = "logs", description = "Log search and volume"),
        (name = "metrics", description = "Metric catalog and time series"),
        (name = "sql", description = "Read-only ad-hoc SQL (disabled by default)")
    )
)]
struct ApiDoc;

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

/// Error type for handlers.
pub struct ApiErr(StatusCode, String, String);

impl From<QueryError> for ApiErr {
    fn from(e: QueryError) -> Self {
        match e {
            QueryError::BadRequest(m) => ApiErr(StatusCode::BAD_REQUEST, "bad_request".into(), m),
            QueryError::NotFound(m) => ApiErr(StatusCode::NOT_FOUND, "not_found".into(), m),
            e @ QueryError::Timeout(_) => {
                ApiErr(StatusCode::GATEWAY_TIMEOUT, "timeout".into(), e.to_string())
            }
            e @ QueryError::Busy => ApiErr(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy".into(),
                e.to_string(),
            ),
            QueryError::Internal(e) => {
                tracing::error!(error = %format!("{e:#}"), "query failed");
                ApiErr(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal".into(),
                    format!("{e:#}"),
                )
            }
        }
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(ApiError {
                error: self.1,
                message: self.2,
            }),
        )
            .into_response()
    }
}

type R<T> = Result<Json<T>, ApiErr>;

fn window<Q: HasWindow>(st: &ApiState, q: &Q) -> Result<Window, ApiErr> {
    Ok(Window::resolve(&q.window(), HOUR, st.reader.max_window)?)
}

/// Build the API router and its OpenAPI document.
pub fn router(state: Arc<ApiState>) -> (Router, utoipa::openapi::OpenApi) {
    let (api, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(stats))
        .routes(routes!(services))
        .routes(routes!(operations))
        .routes(routes!(span_series))
        .routes(routes!(search_traces))
        .routes(routes!(get_trace))
        .routes(routes!(search_logs))
        .routes(routes!(log_series))
        .routes(routes!(list_metrics))
        .routes(routes!(query_metric))
        .routes(routes!(run_sql))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .routes(routes!(health))
        .with_state(state)
        .split_for_parts();
    let spec_json = openapi.to_json().unwrap_or_default();
    let docs_html = scalar_html();
    let router = api
        .route(
            "/openapi.json",
            get(move || async move { ([(header::CONTENT_TYPE, "application/json")], spec_json) }),
        )
        .route("/docs", get(move || async move { Html(docs_html) }));
    (router, openapi)
}

/// The OpenAPI document without building a server (for `otel-ducklake openapi`).
pub fn openapi() -> utoipa::openapi::OpenApi {
    OpenApiRouter::<Arc<ApiState>>::with_openapi(ApiDoc::openapi())
        .routes(routes!(health))
        .routes(routes!(stats))
        .routes(routes!(services))
        .routes(routes!(operations))
        .routes(routes!(span_series))
        .routes(routes!(search_traces))
        .routes(routes!(get_trace))
        .routes(routes!(search_logs))
        .routes(routes!(log_series))
        .routes(routes!(list_metrics))
        .routes(routes!(query_metric))
        .routes(routes!(run_sql))
        .split_for_parts()
        .1
}

/// API reference page. Loads the spec from a relative URL so it works when nested.
fn scalar_html() -> String {
    r#"<!doctype html>
<html><head><title>otel-ducklake API</title><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/></head>
<body><script id="api-reference" data-url="openapi.json"></script>
<script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.25.0"></script></body></html>"#
        .to_string()
}

async fn auth(State(st): S, req: Request, next: Next) -> Response {
    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if st.tokens.allows(header) {
        next.run(req).await
    } else {
        ApiErr(
            StatusCode::UNAUTHORIZED,
            "unauthorized".into(),
            "invalid or missing bearer token".into(),
        )
        .into_response()
    }
}

#[utoipa::path(get, path = "/api/v1/health", tag = "system",
    responses((status = 200, description = "Server is up", body = Health)))]
async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

#[utoipa::path(get, path = "/api/v1/stats", tag = "system", security(("bearer" = [])),
    responses((status = 200, body = odl_api::Stats), (status = 401, body = ApiError)))]
async fn stats(State(st): S) -> R<odl_api::Stats> {
    let schema = st.schema.clone();
    let (snapshot_id, tables) = st.reader.run(move |c| storage::tables(c, &schema)).await?;
    Ok(Json(odl_api::Stats {
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_secs: st.started.elapsed().as_secs(),
        ingest: st.stats.ingest(st.max_buffer_bytes),
        storage: StorageStats {
            catalog_type: st.catalog_type.clone(),
            data_path: st.data_path.clone(),
            snapshot_id,
            tables,
            last_maintenance: st.stats.last_maintenance(),
        },
    }))
}

#[utoipa::path(get, path = "/api/v1/services", tag = "traces", params(ServiceQuery), security(("bearer" = [])),
    responses((status = 200, description = "RED metrics per service", body = Vec<ServiceSummary>),
              (status = 400, body = ApiError)))]
async fn services(State(st): S, Query(q): Query<ServiceQuery>) -> R<Vec<ServiceSummary>> {
    let w = window(&st, &q)?;
    let entry = q.entry_only.unwrap_or(true);
    Ok(Json(
        st.reader
            .run(move |c| traces::services(c, &w, entry))
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/operations", tag = "traces", params(OperationQuery), security(("bearer" = [])),
    responses((status = 200, description = "RED metrics per operation", body = Vec<OperationSummary>),
              (status = 400, body = ApiError)))]
async fn operations(State(st): S, Query(q): Query<OperationQuery>) -> R<Vec<OperationSummary>> {
    let w = window(&st, &q)?;
    let entry = q.entry_only.unwrap_or(true);
    Ok(Json(
        st.reader
            .run(move |c| traces::operations(c, &w, q.service.as_deref(), entry))
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/spans/series", tag = "traces", params(SpanSeriesQuery), security(("bearer" = [])),
    responses((status = 200, description = "Request rate, errors and latency over time", body = SpanSeries),
              (status = 400, body = ApiError)))]
async fn span_series(State(st): S, Query(q): Query<SpanSeriesQuery>) -> R<SpanSeries> {
    let w = window(&st, &q)?;
    let step = w.step(q.step.as_deref())?;
    let entry = q.entry_only.unwrap_or(true);
    Ok(Json(
        st.reader
            .run(move |c| {
                traces::span_series(
                    c,
                    &w,
                    step,
                    q.service.as_deref(),
                    q.operation.as_deref(),
                    entry,
                )
            })
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/traces", tag = "traces", params(TraceQuery), security(("bearer" = [])),
    responses((status = 200, description = "Matching traces, newest first", body = Vec<TraceSummary>),
              (status = 400, body = ApiError)))]
async fn search_traces(State(st): S, Query(q): Query<TraceQuery>) -> R<Vec<TraceSummary>> {
    let w = window(&st, &q)?;
    let limit = clamp_limit(q.limit, 50, 1000)?;
    Ok(Json(
        st.reader
            .run(move |c| {
                traces::search(
                    c,
                    &w,
                    &traces::TraceSearch {
                        service: q.service.as_deref(),
                        operation: q.operation.as_deref(),
                        errors_only: q.errors_only.unwrap_or(false),
                        min_duration_ms: q.min_duration_ms,
                        max_duration_ms: q.max_duration_ms,
                        attributes: q.attributes.as_deref(),
                        limit,
                    },
                )
            })
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/traces/{trace_id}", tag = "traces",
    params(("trace_id" = String, Path, description = "32 hex chars"), TraceGetQuery), security(("bearer" = [])),
    responses((status = 200, body = Trace), (status = 404, body = ApiError)))]
async fn get_trace(
    State(st): S,
    Path(trace_id): Path<String>,
    Query(q): Query<TraceGetQuery>,
) -> R<Trace> {
    // Without a window, look back a week.
    let w = Window::resolve(
        &q.window(),
        std::time::Duration::from_secs(7 * 86400),
        st.reader.max_window,
    )?;
    Ok(Json(
        st.reader
            .run(move |c| traces::get(c, &w, &trace_id))
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/logs", tag = "logs", params(LogQuery), security(("bearer" = [])),
    responses((status = 200, body = Vec<LogRecord>), (status = 400, body = ApiError)))]
async fn search_logs(State(st): S, Query(q): Query<LogQuery>) -> R<Vec<LogRecord>> {
    let w = window(&st, &q)?;
    let limit = clamp_limit(q.limit, 100, 5000)?;
    let ascending = match q.order.as_deref() {
        None | Some("desc") => false,
        Some("asc") => true,
        Some(o) => {
            return Err(
                QueryError::BadRequest(format!("order must be asc or desc, got {o:?}")).into(),
            );
        }
    };
    Ok(Json(
        st.reader
            .run(move |c| {
                logs::search(
                    c,
                    &w,
                    &logs::LogSearch {
                        service: q.service.as_deref(),
                        min_severity: q.min_severity,
                        q: q.q.as_deref(),
                        trace_id: q.trace_id.as_deref(),
                        attributes: q.attributes.as_deref(),
                        limit,
                        ascending,
                    },
                )
            })
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/logs/series", tag = "logs", params(LogSeriesQuery), security(("bearer" = [])),
    responses((status = 200, body = LogSeries), (status = 400, body = ApiError)))]
async fn log_series(State(st): S, Query(q): Query<LogSeriesQuery>) -> R<LogSeries> {
    let w = window(&st, &q)?;
    let step = w.step(q.step.as_deref())?;
    Ok(Json(
        st.reader
            .run(move |c| logs::series(c, &w, step, q.service.as_deref(), q.q.as_deref()))
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/metrics", tag = "metrics", params(MetricListQuery), security(("bearer" = [])),
    responses((status = 200, body = Vec<MetricInfo>), (status = 400, body = ApiError)))]
async fn list_metrics(State(st): S, Query(q): Query<MetricListQuery>) -> R<Vec<MetricInfo>> {
    let w = window(&st, &q)?;
    Ok(Json(
        st.reader
            .run(move |c| metrics::list(c, &w, q.service.as_deref(), q.q.as_deref()))
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/metrics/query", tag = "metrics", params(MetricQuery), security(("bearer" = [])),
    responses((status = 200, body = MetricQueryResult), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn query_metric(State(st): S, Query(q): Query<MetricQuery>) -> R<MetricQueryResult> {
    let w = window(&st, &q)?;
    let step = w.step(q.step.as_deref())?;
    Ok(Json(
        st.reader
            .run(move |c| {
                metrics::query(
                    c,
                    &w,
                    &metrics::MetricRequest {
                        name: &q.name,
                        service: q.service.as_deref(),
                        agg: q.agg.as_deref(),
                        group_by: q.group_by.as_deref(),
                        attributes: q.attributes.as_deref(),
                        step,
                    },
                )
            })
            .await?,
    ))
}

#[utoipa::path(post, path = "/api/v1/sql", tag = "sql", request_body = SqlRequest, security(("bearer" = [])),
    responses((status = 200, body = SqlResponse), (status = 400, body = ApiError), (status = 403, body = ApiError)))]
async fn run_sql(State(st): S, Json(req): Json<SqlRequest>) -> R<SqlResponse> {
    if !st.enable_sql {
        return Err(ApiErr(
            StatusCode::FORBIDDEN,
            "disabled".into(),
            "ad-hoc SQL is disabled; set query.enable_sql = true (requires auth.api_tokens)".into(),
        ));
    }
    let limit = clamp_limit(req.limit, 1000, 100_000)?;
    let schema = st.schema.clone();
    Ok(Json(
        st.reader
            .run(move |c| sql::run(c, &schema, &req.query, limit))
            .await?,
    ))
}
