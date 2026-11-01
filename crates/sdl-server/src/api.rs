//! The `/v1` query and management API (OpenAPI documented), the
//! `/api/0/...` endpoints `sentry-cli` uses for sourcemap uploads, and the
//! meta routes (`/openapi.json`, `/docs`, `/healthz`, `/readyz`).

use crate::AppState;
use crate::db::read::{self, Window, parse_group_id};
use crate::db::{ProjectRow, WriteCmd, WriteError};
use crate::ingest::constant_time_eq;
use crate::sourcemaps::{self, AssembleRequest, AssembleResponse, SourcemapError};
use axum::extract::{Multipart, Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use flate2::read::GzDecoder;
use sdl_protocol::dsn::Dsn;
use sdl_types::*;
use serde_json::json;
use std::io::Read;
use std::sync::Arc;
use utoipa::OpenApi;
use utoipa_scalar::Servable;

type S = State<Arc<AppState>>;

pub struct ApiErr(StatusCode, String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
    }
}

impl From<anyhow::Error> for ApiErr {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = format!("{e:#}"), "api error");
        ApiErr(StatusCode::INTERNAL_SERVER_ERROR, "internal error (see server log)".into())
    }
}

impl From<WriteError> for ApiErr {
    fn from(e: WriteError) -> Self {
        match e {
            WriteError::SlugTaken(_) => ApiErr(StatusCode::CONFLICT, e.to_string()),
            WriteError::IssueNotFound => not_found("issue"),
            WriteError::Db(e) => e.into(),
        }
    }
}

impl From<read::WindowError> for ApiErr {
    fn from(e: read::WindowError) -> Self {
        bad(e.to_string())
    }
}

fn bad(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found(what: &str) -> ApiErr {
    ApiErr(StatusCode::NOT_FOUND, format!("{what} not found"))
}

type ApiResult<T> = Result<Json<T>, ApiErr>;

#[derive(OpenApi)]
#[openapi(
    info(title = "sentry-ducklake API", description = "Query and manage errors stored by sentry-ducklake. Ingest uses the Sentry protocol at /api/{project_id}/envelope/."),
    paths(info, events_by_trace, list_projects, create_project, get_project, list_issues, get_issue, update_issue, list_events, get_event, project_stats, list_sourcemaps, run_maintenance),
    components(schemas(
        ServerInfo, Project, CreateProject, IssueStatus, IssueSummary, IssueList, IssueDetail, IssueActivity,
        TagFacet, TagValueCount, UpdateIssue, EventSummary, EventList, EventDetail, ProjectStats, StatsBucket,
        ArtifactBundle, ApiError
    )),
    modifiers(&BearerAuth),
    security(("bearer" = []))
)]
struct ApiDoc;

struct BearerAuth;
impl utoipa::Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        let c = openapi.components.get_or_insert_with(Default::default);
        c.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

pub fn router(state: Arc<AppState>) -> Router {
    let v1 = Router::new()
        .route("/v1/info", get(info))
        .route("/v1/projects", get(list_projects).post(create_project))
        .route("/v1/events", get(events_by_trace))
        .route("/v1/projects/{project}", get(get_project))
        .route("/v1/projects/{project}/issues", get(list_issues))
        .route("/v1/projects/{project}/issues/{issue_id}", get(get_issue).patch(update_issue))
        .route("/v1/projects/{project}/events", get(list_events))
        .route("/v1/projects/{project}/events/{event_id}", get(get_event))
        .route("/v1/projects/{project}/stats", get(project_stats))
        .route("/v1/projects/{project}/sourcemaps", get(list_sourcemaps))
        .route("/v1/maintenance", post(run_maintenance));
    let compat = Router::new()
        .route("/api/0/organizations/{org}/chunk-upload/", get(chunk_options).post(chunk_upload))
        .route("/api/0/organizations/{org}/artifactbundle/assemble/", post(assemble))
        .route("/api/0/organizations/{org}/releases/", post(release_upsert))
        .route("/api/0/projects/{org}/{project}/releases/", post(release_upsert))
        .route("/api/0/organizations/{org}/releases/{version}/", get(release_get).put(release_upsert_path))
        .route("/api/0/organizations/{org}/releases/{version}/deploys/", post(release_deploy))
        .layer(axum::extract::DefaultBodyLimit::max(sourcemaps::MAX_REQUEST_SIZE + 1024 * 1024));
    v1.merge(compat).layer(middleware::from_fn_with_state(state.clone(), require_token)).with_state(state)
}

pub fn meta_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/openapi.json", get(|| async { Json(openapi()) }))
        .merge(utoipa_scalar::Scalar::with_url("/docs", openapi()))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .with_state(state)
}

async fn require_token(State(state): S, req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim);
    let ok = token.is_some_and(|t| state.tokens.iter().any(|known| constant_time_eq(known.as_bytes(), t.as_bytes())));
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "missing or invalid bearer token", "detail": "Invalid token"})),
        )
            .into_response();
    }
    next.run(req).await
}

async fn readyz(State(state): S) -> Response {
    if !state.db.writer_alive() {
        return (StatusCode::SERVICE_UNAVAILABLE, "writer stopped").into_response();
    }
    match state.db.read(|c, _| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i32>(0))?)).await {
        Ok(_) => (StatusCode::OK, "ready").into_response(),
        Err(e) => {
            // Details go to the log, not to unauthenticated callers.
            tracing::warn!(error = format!("{e:#}"), "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database not ready").into_response()
        }
    }
}

fn to_project(state: &AppState, p: ProjectRow) -> Project {
    Project {
        dsn: Dsn::format(&state.cfg.public_url, &p.public_key, p.id),
        id: p.id,
        slug: p.slug,
        name: p.name,
        platform: p.platform,
        public_key: p.public_key,
        created_at: p.created_at,
    }
}

fn project(state: &AppState, r: &str) -> Result<ProjectRow, ApiErr> {
    state.db.caches.project_by_ref(r).ok_or_else(|| not_found("project"))
}

fn issue_hash(id: &str) -> Result<u64, ApiErr> {
    parse_group_id(id).ok_or_else(|| bad("issue id must be 16 hex characters"))
}

#[utoipa::path(get, path = "/v1/info", responses((status = 200, body = ServerInfo)))]
async fn info(State(state): S) -> Json<ServerInfo> {
    Json(ServerInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        duckdb_version: state.db.duckdb_version.clone(),
        catalog: state.db.catalog.clone(),
        storage: state.storage.as_ref().map(|s| s.describe()).unwrap_or_else(|| "(external)".into()),
        queue_depth: state.db.queue_depth() as u64,
        queue_capacity: state.db.queue_capacity as u64,
    })
}

#[utoipa::path(get, path = "/v1/projects", responses((status = 200, body = Vec<Project>)))]
async fn list_projects(State(state): S) -> Json<Vec<Project>> {
    Json(state.db.caches.projects().into_iter().map(|p| to_project(&state, p)).collect())
}

fn slugify(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    s.trim_end_matches('-').to_string()
}

#[utoipa::path(post, path = "/v1/projects", request_body = CreateProject,
    responses((status = 201, body = Project), (status = 400, body = ApiError), (status = 409, body = ApiError)))]
async fn create_project(
    State(state): S,
    Json(req): Json<CreateProject>,
) -> Result<(StatusCode, Json<Project>), ApiErr> {
    let CreateProject { name, slug, platform } = req;
    let slug = slug.unwrap_or_else(|| slugify(&name));
    if slug.is_empty()
        || slug.len() > 64
        || !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(bad("slug must be 1-64 chars of a-z, 0-9, - or _"));
    }
    if slug.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad("slug must not be numeric (it would clash with project ids)"));
    }
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(bad("name is required"));
    }
    let row = state.db.call(|reply| WriteCmd::CreateProject { name, slug, platform, reply }).await?;
    Ok((StatusCode::CREATED, Json(to_project(&state, row))))
}

#[utoipa::path(get, path = "/v1/projects/{project}", params(("project" = String, Path, description = "Project id or slug")),
    responses((status = 200, body = Project), (status = 404, body = ApiError)))]
async fn get_project(State(state): S, Path(p): Path<String>) -> ApiResult<Project> {
    Ok(Json(to_project(&state, project(&state, &p)?)))
}

#[utoipa::path(get, path = "/v1/projects/{project}/issues",
    params(("project" = String, Path, description = "Project id or slug"), IssueQuery),
    responses((status = 200, body = IssueList), (status = 404, body = ApiError)))]
async fn list_issues(State(state): S, Path(p): Path<String>, Query(q): Query<IssueQuery>) -> ApiResult<IssueList> {
    let pid = project(&state, &p)?.id;
    let status = read::StatusFilter::parse(q.status.as_deref())
        .ok_or_else(|| bad("status must be unresolved, resolved, ignored or all"))?;
    let sort = read::IssueSort::parse(q.sort.as_deref())
        .ok_or_else(|| bad("sort must be last_seen, first_seen, count or users"))?;
    let w = Window::parse(q.since.as_deref(), "14d", Utc::now())?;
    Ok(Json(state.db.read(move |c, ns| read::list_issues(c, ns, pid, &q, &w, status, sort)).await?))
}

#[utoipa::path(get, path = "/v1/projects/{project}/issues/{issue_id}",
    params(("project" = String, Path), ("issue_id" = String, Path, description = "16 hex chars"), WindowQuery),
    responses((status = 200, body = IssueDetail), (status = 404, body = ApiError)))]
async fn get_issue(
    State(state): S,
    Path((p, id)): Path<(String, String)>,
    Query(q): Query<WindowQuery>,
) -> ApiResult<IssueDetail> {
    let pid = project(&state, &p)?.id;
    let g = issue_hash(&id)?;
    let w = Window::parse(q.since.as_deref(), "14d", Utc::now())?;
    state.db.read(move |c, ns| read::issue_detail(c, ns, pid, g, &w)).await?.map(Json).ok_or_else(|| not_found("issue"))
}

#[utoipa::path(patch, path = "/v1/projects/{project}/issues/{issue_id}", request_body = UpdateIssue,
    params(("project" = String, Path), ("issue_id" = String, Path)),
    responses((status = 200, body = IssueDetail), (status = 404, body = ApiError)))]
async fn update_issue(
    State(state): S,
    Path((p, id)): Path<(String, String)>,
    Json(req): Json<UpdateIssue>,
) -> ApiResult<IssueDetail> {
    let pid = project(&state, &p)?.id;
    let g = issue_hash(&id)?;
    state
        .db
        .call(|reply| WriteCmd::SetStatus { project_id: pid, group_hash: g, status: req.status, reason: "user", reply })
        .await?;
    let w = Window::parse(None, "14d", Utc::now())?;
    state.db.read(move |c, ns| read::issue_detail(c, ns, pid, g, &w)).await?.map(Json).ok_or_else(|| not_found("issue"))
}

#[utoipa::path(get, path = "/v1/projects/{project}/events", params(("project" = String, Path), EventQuery),
    responses((status = 200, body = EventList), (status = 404, body = ApiError)))]
async fn list_events(State(state): S, Path(p): Path<String>, Query(q): Query<EventQuery>) -> ApiResult<EventList> {
    let pid = project(&state, &p)?.id;
    let g = q.issue.as_deref().map(issue_hash).transpose()?;
    if q.trace_id.as_deref().is_some_and(|t| sdl_protocol::event::parse_trace_id(t).is_none()) {
        return Err(bad("trace_id must be 32 hex characters"));
    }
    let before =
        q.before.as_deref().map(|c| read::Cursor::parse(c).ok_or_else(|| bad("invalid cursor"))).transpose()?;
    Ok(Json(state.db.read(move |c, ns| read::list_events(c, ns, pid, g, &q, before)).await?))
}

#[utoipa::path(get, path = "/v1/events", params(TraceQuery),
    description = "Events of one trace across all projects (for linking errors to traces).",
    responses((status = 200, body = EventList), (status = 400, body = ApiError)))]
async fn events_by_trace(State(state): S, Query(q): Query<TraceQuery>) -> ApiResult<EventList> {
    let t =
        sdl_protocol::event::parse_trace_id(&q.trace_id).ok_or_else(|| bad("trace_id must be 32 hex characters"))?;
    let span = q
        .span_id
        .as_deref()
        .map(|s| sdl_protocol::event::parse_span_id(s).ok_or_else(|| bad("span_id must be 16 hex characters")))
        .transpose()?;
    let w = Window::parse(q.since.as_deref(), "30d", Utc::now())?;
    let limit = q.limit.unwrap_or(50);
    Ok(Json(state.db.read(move |c, ns| read::events_by_trace(c, ns, t, span, &w, limit)).await?))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
struct EventDetailQuery {
    /// Issue id; makes the lookup much faster on large projects.
    issue: Option<String>,
}

#[utoipa::path(get, path = "/v1/projects/{project}/events/{event_id}",
    params(("project" = String, Path), ("event_id" = String, Path, description = "32 hex chars"), EventDetailQuery),
    responses((status = 200, body = EventDetail), (status = 404, body = ApiError)))]
async fn get_event(
    State(state): S,
    Path((p, id)): Path<(String, String)>,
    Query(q): Query<EventDetailQuery>,
) -> ApiResult<EventDetail> {
    let pid = project(&state, &p)?.id;
    let eid = sdl_protocol::event::parse_uuid(&id).ok_or_else(|| bad("invalid event id"))?;
    let g = q.issue.as_deref().map(issue_hash).transpose()?;
    state
        .db
        .read(move |c, ns| read::event_detail(c, ns, pid, eid, g))
        .await?
        .map(Json)
        .ok_or_else(|| not_found("event"))
}

#[utoipa::path(get, path = "/v1/projects/{project}/stats", params(("project" = String, Path), WindowQuery),
    responses((status = 200, body = ProjectStats), (status = 404, body = ApiError)))]
async fn project_stats(
    State(state): S,
    Path(p): Path<String>,
    Query(q): Query<WindowQuery>,
) -> ApiResult<ProjectStats> {
    let pid = project(&state, &p)?.id;
    let w = Window::parse(q.since.as_deref(), "24h", Utc::now())?;
    let interval_us = q
        .interval
        .as_deref()
        .map(|i| {
            let d = humantime::parse_duration(i).map_err(|e| bad(format!("invalid interval: {e}")))?;
            i64::try_from(d.as_micros()).map_err(|_| bad("interval is too large"))
        })
        .transpose()?;
    Ok(Json(state.db.read(move |c, ns| read::project_stats(c, ns, pid, &w, interval_us)).await?))
}

#[utoipa::path(get, path = "/v1/projects/{project}/sourcemaps", params(("project" = String, Path)),
    responses((status = 200, body = Vec<ArtifactBundle>), (status = 404, body = ApiError)))]
async fn list_sourcemaps(State(state): S, Path(p): Path<String>) -> ApiResult<Vec<ArtifactBundle>> {
    let pid = project(&state, &p)?.id;
    Ok(Json(state.db.read(move |c, ns| read::list_bundles(c, ns, pid)).await?))
}

#[utoipa::path(post, path = "/v1/maintenance",
    description = "Flush inlined data to Parquet, apply retention, compact files and expire snapshots now.",
    responses((status = 204), (status = 500, body = ApiError)))]
async fn run_maintenance(State(state): S) -> Result<StatusCode, ApiErr> {
    state.db.call(|reply| WriteCmd::Maintenance { reply }).await.map_err(ApiErr::from)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// sentry-cli / bundler plugin compatibility (/api/0/...)
// ---------------------------------------------------------------------------

fn sentry_err(status: StatusCode, msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (status, Json(json!({ "error": msg, "detail": msg }))).into_response()
}

impl IntoResponse for SourcemapError {
    fn into_response(self) -> Response {
        match self {
            SourcemapError::NotConfigured => sentry_err(StatusCode::NOT_IMPLEMENTED, self.to_string()),
            SourcemapError::BadRequest(msg) => sentry_err(StatusCode::BAD_REQUEST, msg),
            SourcemapError::Internal(e) => {
                tracing::error!(error = format!("{e:#}"), "sourcemap request failed");
                sentry_err(StatusCode::INTERNAL_SERVER_ERROR, "internal error (see server log)")
            }
        }
    }
}

async fn chunk_options(State(state): S, Path(org): Path<String>) -> Response {
    if !state.sourcemaps.enabled() {
        return sentry_err(StatusCode::NOT_IMPLEMENTED, "sourcemap storage is not configured");
    }
    let base = state.cfg.public_url.trim_end_matches('/');
    Json(json!({
        "url": format!("{base}/api/0/organizations/{org}/chunk-upload/"),
        "chunkSize": sourcemaps::CHUNK_SIZE,
        "chunksPerRequest": sourcemaps::CHUNKS_PER_REQUEST,
        "maxFileSize": sourcemaps::MAX_BUNDLE_SIZE,
        "maxRequestSize": sourcemaps::MAX_REQUEST_SIZE,
        "concurrency": 8,
        "hashAlgorithm": "sha1",
        "compression": ["gzip"],
        "accept": ["release_files", "artifact_bundles", "artifact_bundles_v2"]
    }))
    .into_response()
}

async fn chunk_upload(State(state): S, mut form: Multipart) -> Response {
    let mut parts = 0usize;
    let mut total = 0usize;
    loop {
        let field = match form.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return sentry_err(StatusCode::BAD_REQUEST, format!("invalid multipart body: {e}")),
        };
        parts += 1;
        if parts > sourcemaps::CHUNKS_PER_REQUEST {
            return sentry_err(StatusCode::BAD_REQUEST, "Too many chunks");
        }
        let name = field.name().unwrap_or_default().to_string();
        let checksum = field.file_name().unwrap_or_default().to_string();
        let data = match field.bytes().await {
            Ok(d) => d,
            Err(e) => return sentry_err(StatusCode::BAD_REQUEST, format!("reading chunk failed: {e}")),
        };
        let data = match name.as_str() {
            "file" => data,
            "file_gzip" => {
                let mut out = Vec::new();
                let mut dec = GzDecoder::new(&data[..]).take(sourcemaps::CHUNK_SIZE as u64 + 1);
                if dec.read_to_end(&mut out).is_err() {
                    return sentry_err(StatusCode::BAD_REQUEST, "Invalid gzip payload");
                }
                bytes::Bytes::from(out)
            }
            other => return sentry_err(StatusCode::BAD_REQUEST, format!("unsupported chunk field {other:?}")),
        };
        total += data.len();
        if total > sourcemaps::MAX_REQUEST_SIZE {
            return sentry_err(StatusCode::BAD_REQUEST, "Request too large");
        }
        if let Err(e) = state.sourcemaps.store_chunk(&checksum, data).await {
            return e.into_response();
        }
    }
    StatusCode::OK.into_response()
}

async fn assemble(State(state): S, Json(req): Json<AssembleRequest>) -> Result<Json<AssembleResponse>, SourcemapError> {
    Ok(Json(state.sourcemaps.assemble(&state.db, req).await?))
}

/// Releases are not stored; we answer so bundler plugins don't log errors.
async fn release_upsert(Json(body): Json<serde_json::Value>) -> Response {
    let version = body.get("version").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    (StatusCode::CREATED, Json(release_info(&version))).into_response()
}

async fn release_upsert_path(Path((_org, version)): Path<(String, String)>) -> Response {
    Json(release_info(&version)).into_response()
}

async fn release_get(Path((_org, version)): Path<(String, String)>) -> Response {
    Json(release_info(&version)).into_response()
}

async fn release_deploy(
    Path((_org, _version)): Path<(String, String)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let env = body.get("environment").cloned().unwrap_or(json!("production"));
    (StatusCode::CREATED, Json(json!({"id": "1", "environment": env, "dateFinished": Utc::now()}))).into_response()
}

fn release_info(version: &str) -> serde_json::Value {
    json!({ "version": version, "dateCreated": Utc::now(), "projects": [], "newGroups": 0 })
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs() {
        assert_eq!(super::slugify("My Web App!"), "my-web-app");
        assert_eq!(super::slugify("  API  "), "api");
    }

    #[test]
    fn write_errors_map_by_variant_not_text() {
        use super::*;
        let status = |e: WriteError| ApiErr::from(e).0;
        assert_eq!(status(WriteError::SlugTaken("x".into())), StatusCode::CONFLICT);
        assert_eq!(status(WriteError::IssueNotFound), StatusCode::NOT_FOUND);
        // A database error whose text happens to say "not found" is a 500.
        assert_eq!(status(WriteError::Db(anyhow::anyhow!("table not found"))), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(status(WriteError::Db(anyhow::anyhow!("x already exists"))), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn sourcemap_errors_map_by_variant() {
        use super::*;
        let status = |e: SourcemapError| e.into_response().status();
        assert_eq!(status(SourcemapError::NotConfigured), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(status(SourcemapError::BadRequest("bad".into())), StatusCode::BAD_REQUEST);
        assert_eq!(status(SourcemapError::Internal(anyhow::anyhow!("disk"))), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn openapi_builds() {
        let doc = super::openapi();
        assert!(doc.paths.paths.contains_key("/v1/projects/{project}/issues"));
    }
}
