//! Query API (`/api/v1/...`) with an OpenAPI document at `/openapi.json` and
//! interactive docs at `/docs`. All paths are relative to wherever the router
//! is mounted, so an embedding application can `nest` it.

use std::{sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware,
};
use mallard_common::{
    auth::Tokens,
    http::{ApiErr, BearerAuth, J, Q, docs_routes, require_bearer},
};
use odl_api::*;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    query::{
        QueryError, Reader, Window, exemplars, logs, metrics, sql, storage,
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
    pub catalog_type: CatalogType,
    pub data_path: String,
    pub max_buffer_bytes: u64,
    pub enable_sql: bool,
    pub capabilities: Capabilities,
    pub started: Instant,
}

type S = State<Arc<ApiState>>;

const HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

#[derive(OpenApi)]
#[openapi(
    info(
        title = "otel-ducklake query API",
        description = "Query traces, logs and metrics stored in DuckLake by otel-ducklake.",
        license(name = "MIT OR Apache-2.0")
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

impl From<QueryError> for ApiErr {
    fn from(e: QueryError) -> Self {
        match e {
            QueryError::BadRequest(m) => ApiErr::bad_request(m),
            QueryError::NotFound(m) => ApiErr::not_found(m),
            e @ QueryError::Timeout(_) => ApiErr::new(StatusCode::GATEWAY_TIMEOUT, "timeout", e.to_string()),
            e @ QueryError::Busy => ApiErr::new(StatusCode::SERVICE_UNAVAILABLE, "busy", e.to_string()),
            QueryError::Internal(e) => {
                // Details (paths, bucket names, connection strings) stay in the log.
                tracing::error!(error = %crate::store::redact(&format!("{e:#}")), "query failed");
                ApiErr::internal()
            }
        }
    }
}

type R<T> = Result<Json<T>, ApiErr>;

fn window<Q: HasWindow>(st: &ApiState, q: &Q) -> Result<Window, ApiErr> {
    resolve(st, q, HOUR)
}

fn resolve<Q: HasWindow>(st: &ApiState, q: &Q, default: std::time::Duration) -> Result<Window, ApiErr> {
    let w = q.window();
    Ok(Window::resolve(w.from, w.to, w.since.as_deref(), default, st.reader.max_window)?)
}

/// Routes behind the bearer-token check.
fn protected_routes() -> OpenApiRouter<Arc<ApiState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
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
}

/// Routes without authentication.
fn public_routes() -> OpenApiRouter<Arc<ApiState>> {
    OpenApiRouter::new().routes(routes!(health))
}

/// Build the API router. The OpenAPI document is derived from the same
/// route list, so the two cannot drift apart.
pub fn router(state: Arc<ApiState>) -> Router {
    let (api, openapi) = protected_routes()
        .layer(middleware::from_fn_with_state(state.tokens.clone(), require_bearer))
        .merge(public_routes())
        .with_state(state)
        .split_for_parts();
    api.merge(docs_routes(&openapi, "otel-ducklake API"))
}

/// The OpenAPI document without building a server (for `otel-ducklake openapi`).
pub fn openapi() -> utoipa::openapi::OpenApi {
    protected_routes().merge(public_routes()).split_for_parts().1
}

#[utoipa::path(get, path = "/api/v1/health", tag = "system",
    responses((status = 200, description = "Server is up", body = Health)))]
async fn health() -> Json<Health> {
    Json(Health { status: "ok".into(), version: env!("CARGO_PKG_VERSION").into() })
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
            catalog_type: st.catalog_type,
            data_path: st.data_path.clone(),
            snapshot_id,
            tables,
            last_maintenance: st.stats.last_maintenance(),
        },
        capabilities: Some(st.capabilities.clone()),
    }))
}

#[utoipa::path(get, path = "/api/v1/services", tag = "traces", params(ServiceQuery), security(("bearer" = [])),
    responses((status = 200, description = "RED metrics per service", body = Vec<ServiceSummary>),
              (status = 400, body = ApiError)))]
async fn services(State(st): S, Q(q): Q<ServiceQuery>) -> R<Vec<ServiceSummary>> {
    let w = window(&st, &q)?;
    let entry = q.entry_only.unwrap_or(true);
    Ok(Json(st.reader.run(move |c| traces::services(c, &w, entry)).await?))
}

#[utoipa::path(get, path = "/api/v1/operations", tag = "traces", params(OperationQuery), security(("bearer" = [])),
    responses((status = 200, description = "RED metrics per operation", body = Vec<OperationSummary>),
              (status = 400, body = ApiError)))]
async fn operations(State(st): S, Q(q): Q<OperationQuery>) -> R<Vec<OperationSummary>> {
    let w = window(&st, &q)?;
    let entry = q.entry_only.unwrap_or(true);
    Ok(Json(st.reader.run(move |c| traces::operations(c, &w, q.service.as_deref(), entry)).await?))
}

#[utoipa::path(get, path = "/api/v1/spans/series", tag = "traces", params(SpanSeriesQuery), security(("bearer" = [])),
    responses((status = 200, description = "Request rate, errors and latency over time", body = SpanSeries),
              (status = 400, body = ApiError)))]
async fn span_series(State(st): S, Q(q): Q<SpanSeriesQuery>) -> R<SpanSeries> {
    let w = window(&st, &q)?;
    let step = w.step(q.step.as_deref())?;
    let entry = q.entry_only.unwrap_or(true);
    Ok(Json(
        st.reader
            .run(move |c| traces::span_series(c, &w, step, q.service.as_deref(), q.operation.as_deref(), entry))
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/traces", tag = "traces", params(TraceQuery), security(("bearer" = [])),
    responses((status = 200, description = "Matching traces, newest first", body = Vec<TraceSummary>),
              (status = 400, body = ApiError)))]
async fn search_traces(State(st): S, Q(q): Q<TraceQuery>) -> R<Vec<TraceSummary>> {
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
async fn get_trace(State(st): S, Path(trace_id): Path<String>, Q(q): Q<TraceGetQuery>) -> R<Trace> {
    // Without a window, look back a week.
    let w = resolve(&st, &q, std::time::Duration::from_secs(7 * 86400))?;
    Ok(Json(st.reader.run(move |c| traces::get(c, &w, &trace_id)).await?))
}

#[utoipa::path(get, path = "/api/v1/logs", tag = "logs", params(LogQuery), security(("bearer" = [])),
    responses((status = 200, body = Vec<LogRecord>), (status = 400, body = ApiError)))]
async fn search_logs(State(st): S, Q(q): Q<LogQuery>) -> R<Vec<LogRecord>> {
    let w = window(&st, &q)?;
    let limit = clamp_limit(q.limit, 100, 5000)?;
    let ascending = q.order == Some(LogOrder::Asc);
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
async fn log_series(State(st): S, Q(q): Q<LogSeriesQuery>) -> R<LogSeries> {
    let w = window(&st, &q)?;
    let step = w.step(q.step.as_deref())?;
    Ok(Json(st.reader.run(move |c| logs::series(c, &w, step, q.service.as_deref(), q.q.as_deref())).await?))
}

#[utoipa::path(get, path = "/api/v1/metrics", tag = "metrics", params(MetricListQuery), security(("bearer" = [])),
    responses((status = 200, body = Vec<MetricInfo>), (status = 400, body = ApiError)))]
async fn list_metrics(State(st): S, Q(q): Q<MetricListQuery>) -> R<Vec<MetricInfo>> {
    let w = window(&st, &q)?;
    Ok(Json(st.reader.run(move |c| metrics::list(c, &w, q.service.as_deref(), q.q.as_deref())).await?))
}

#[utoipa::path(get, path = "/api/v1/metrics/query", tag = "metrics", params(MetricQuery), security(("bearer" = [])),
    responses((status = 200, body = MetricQueryResult), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn query_metric(State(st): S, Q(q): Q<MetricQuery>) -> R<MetricQueryResult> {
    let w = window(&st, &q)?;
    let step = w.step(q.step.as_deref())?;
    Ok(Json(
        st.reader
            .run(move |c| {
                let mut r = metrics::query(
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
                )?;
                if q.exemplars == Some(true) {
                    r.exemplars = exemplars::query(
                        c,
                        &w,
                        &exemplars::ExemplarRequest {
                            name: &q.name,
                            service: q.service.as_deref(),
                            group_by: q.group_by.as_deref(),
                            attributes: q.attributes.as_deref(),
                            step,
                        },
                    )?;
                }
                Ok(r)
            })
            .await?,
    ))
}

#[utoipa::path(post, path = "/api/v1/sql", tag = "sql", request_body = SqlRequest, security(("bearer" = [])),
    responses((status = 200, body = SqlResponse), (status = 400, body = ApiError), (status = 403, body = ApiError)))]
async fn run_sql(State(st): S, J(req): J<SqlRequest>) -> R<SqlResponse> {
    if !st.enable_sql {
        return Err(ApiErr::new(
            StatusCode::FORBIDDEN,
            "disabled",
            "ad-hoc SQL is disabled; set query.enable_sql = true (requires auth.api_tokens)",
        ));
    }
    let limit = clamp_limit(req.limit, 1000, 100_000)?;
    let schema = st.schema.clone();
    Ok(Json(st.reader.run(move |c| sql::run(c, &schema, &req.query, limit)).await?))
}
