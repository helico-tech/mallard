//! The `/v1` query and management API (OpenAPI documented), the
//! `/api/0/...` endpoints `sentry-cli` uses for sourcemap uploads, and the
//! meta routes (`/openapi.json`, `/docs`, `/healthz`, `/readyz`).

use crate::AppState;
use crate::db::read::{self, Window, parse_group_id};
use crate::db::{ProjectRow, WriteCmd, WriteError};
use crate::sourcemaps::{self, AssembleRequest, AssembleResponse, SourcemapError};
use axum::extract::{Multipart, Path, RawQuery, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use mallard_common::http::{ApiErr, BearerAuth, J, Q, docs_routes, readiness, require_bearer};
use mallard_sentry_api::*;
use mallard_sentry_protocol::dsn::Dsn;
use serde_json::json;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

type S = State<Arc<AppState>>;

impl From<WriteError> for ApiErr {
    fn from(e: WriteError) -> Self {
        match e {
            WriteError::SlugTaken(_) | WriteError::SameApp(_) => {
                ApiErr::new(StatusCode::CONFLICT, "conflict", e.to_string())
            }
            WriteError::IssueNotFound => not_found("issue"),
            WriteError::Db(e) => e.into(),
        }
    }
}

fn bad(msg: impl Into<String>) -> ApiErr {
    ApiErr::bad_request(msg)
}

fn not_found(what: &str) -> ApiErr {
    ApiErr::not_found(format!("{what} not found"))
}

type ApiResult<T> = Result<Json<T>, ApiErr>;

const DAY: Duration = Duration::from_secs(86_400);

/// Resolve `since`/`from`/`to` against `default` and `sentry.query.max_window`.
fn window(state: &AppState, since: Option<&str>, r: &WindowRange, default: Duration) -> Result<Window, ApiErr> {
    Ok(Window::resolve(r.from, r.to, since, default, state.cfg.query.max_window)?)
}

/// The repeated `tag=key:value` parameters of a query string (the struct
/// extractors can't read repeated keys).
fn tag_filters(raw: Option<&str>) -> Result<Vec<(String, String)>, ApiErr> {
    let tags: Vec<String> = url::form_urlencoded::parse(raw.unwrap_or("").as_bytes())
        .filter(|(k, _)| k == "tag")
        .map(|(_, v)| v.into_owned())
        .collect();
    if tags.len() > MAX_TAG_FILTERS {
        return Err(bad(format!("at most {MAX_TAG_FILTERS} tag filters")));
    }
    tags.iter()
        .map(|t| {
            parse_tag_filter(t)
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| bad(format!("tag must be key:value, got {t:?}")))
        })
        .collect()
}

#[derive(OpenApi)]
#[openapi(
    info(title = "mallard errors API", description = "Query and manage errors stored by mallard. Ingest uses the Sentry protocol at /api/{project_id}/envelope/."),
    components(schemas(
        ServerInfo, Project, CreateProject, IssueStatus, IssueSummary, IssueList, IssueDetail, IssueActivity,
        TagFacet, TagValueCount, UpdateIssue, EventSummary, EventList, EventDetail, ProjectStats, StatsBucket,
        ArtifactBundle, ApiError, EventsByTrace, TraceEvents, TraceEvent, Release, ReleaseList,
        SqlRequest, SqlResponse, SqlColumn, Schema, SchemaTable, TagStats, TagCount
    )),
    modifiers(&BearerAuth),
    security(("bearer" = []))
)]
struct ApiDoc;

/// The `/v1` routes. The OpenAPI document is derived from the same list.
fn v1_routes() -> OpenApiRouter<Arc<AppState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(info))
        .routes(routes!(list_projects, create_project))
        .routes(routes!(events_by_trace))
        .routes(routes!(events_by_traces))
        .routes(routes!(find_event))
        .routes(routes!(get_project))
        .routes(routes!(list_issues))
        .routes(routes!(get_issue, update_issue))
        .routes(routes!(list_events))
        .routes(routes!(get_event))
        .routes(routes!(project_stats))
        .routes(routes!(list_releases))
        .routes(routes!(tag_stats))
        .routes(routes!(list_sourcemaps))
        .routes(routes!(run_maintenance))
        .routes(routes!(run_sql))
        .routes(routes!(get_schema))
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    v1_routes().split_for_parts().1
}

pub fn router(state: Arc<AppState>) -> Router {
    let (v1, _) = v1_routes()
        .layer(middleware::from_fn_with_state(state.tokens.clone(), require_bearer))
        .with_state(state.clone())
        .split_for_parts();
    let compat = Router::new()
        .route("/api/0/organizations/{org}/chunk-upload/", get(chunk_options).post(chunk_upload))
        .route("/api/0/organizations/{org}/artifactbundle/assemble/", post(assemble))
        .route("/api/0/organizations/{org}/releases/", post(release_upsert))
        .route("/api/0/projects/{org}/{project}/releases/", post(release_upsert))
        .route("/api/0/organizations/{org}/releases/{version}/", get(release_get).put(release_upsert_path))
        .route("/api/0/organizations/{org}/releases/{version}/deploys/", post(release_deploy))
        .layer(axum::extract::DefaultBodyLimit::max(sourcemaps::MAX_REQUEST_SIZE + 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), require_token_sentry))
        .with_state(state);
    v1.merge(compat)
}

pub fn meta_router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(docs_routes(&openapi(), "mallard errors API"))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(|State(state): S| async move { readiness(state.ready().await) }))
        .with_state(state)
}

/// Bearer check for the sentry-cli routes, answering in Sentry's shape
/// (`detail` is what sentry-cli prints).
async fn require_token_sentry(State(state): S, req: Request, next: Next) -> Response {
    let header = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !state.tokens.allows(header) {
        return sentry_err(StatusCode::UNAUTHORIZED, "Invalid token");
    }
    next.run(req).await
}

/// Longest a lookup for `mallard-server` may take: one event id over the whole
/// lake is the slowest read there is.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Reads for `mallard-server`, which sees both sides. It knows the answers, not
/// the tables.
impl AppState {
    /// All projects, read now. Unlike the list behind `/v1/projects`, this fails
    /// when the catalog cannot be read.
    pub(crate) async fn all_projects(&self) -> Result<Vec<Project>, ApiErr> {
        self.db.load_projects(LOOKUP_TIMEOUT).await?.ok_or_else(|| timeout(LOOKUP_TIMEOUT))?;
        self.db.caches.projects().into_iter().map(|p| to_project(self, p)).collect()
    }

    pub(crate) async fn project_ref(&self, r: &str) -> Option<Project> {
        self.db.project_by_ref(r).await.and_then(|p| to_project(self, p).ok())
    }

    /// The events of one trace in `w`, newest first (the default page of `/v1/events`).
    pub(crate) async fn trace_events(&self, w: Window, trace_id: uuid::Uuid) -> Result<Vec<EventSummary>, ApiErr> {
        let run = self.db.read_for(LOOKUP_TIMEOUT, move |c, ns| read::events_by_trace(c, ns, trace_id, None, &w, 50));
        Ok(run.await?.ok_or_else(|| timeout(LOOKUP_TIMEOUT))?.events)
    }

    /// One event by id, in any project.
    pub(crate) async fn event_by_id(&self, id: uuid::Uuid) -> Result<Option<EventDetail>, ApiErr> {
        let run = self.db.read_for(LOOKUP_TIMEOUT, move |c, ns| read::event_detail(c, ns, None, id, None));
        run.await?.ok_or_else(|| timeout(LOOKUP_TIMEOUT))
    }

    /// One issue id in `project`, or in every project that has it. Counts as
    /// `GET /v1/projects/{p}/issues/{id}` does, with its default window.
    pub(crate) async fn issues_by_id(
        &self,
        group: u64,
        project: Option<u32>,
    ) -> Result<Vec<(u32, IssueDetail)>, ApiErr> {
        let w = window(self, None, &WindowRange::default(), 14 * DAY)?;
        let run = self.db.read_for(LOOKUP_TIMEOUT, move |c, ns| {
            let mut out = Vec::new();
            for pid in read::projects_with_issue(c, ns, group)? {
                if project.is_none_or(|p| p == pid)
                    && let Some(d) = read::issue_detail(c, ns, pid, group, &w)?
                {
                    out.push((pid, d));
                }
            }
            Ok(out)
        });
        run.await?.ok_or_else(|| timeout(LOOKUP_TIMEOUT))
    }
}

impl AppState {
    /// Ready = the writer thread runs and a reader can query the catalog.
    /// Details go to the log, not to unauthenticated callers.
    pub(crate) async fn ready(&self) -> Result<(), String> {
        if !self.db.writer_alive() {
            return Err("sentry writer stopped".into());
        }
        match self.db.read(|c, _| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i32>(0))?)).await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "readiness check failed");
                Err("sentry database not ready".into())
            }
        }
    }
}

fn to_project(state: &AppState, p: ProjectRow) -> Result<Project, ApiErr> {
    // Config::validate guarantees a public_url that can carry the key.
    let dsn = Dsn::build(&state.public_url, &p.public_key, p.id).map_err(anyhow::Error::from)?;
    Ok(Project {
        dsn,
        id: p.id,
        slug: p.slug,
        name: p.name,
        platform: p.platform,
        public_key: p.public_key,
        created_at: p.created_at,
    })
}

/// Slug rules for new projects.
pub(crate) fn check_slug(slug: &str) -> Result<(), &'static str> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("slug must be 1-64 chars of a-z, 0-9, - or _");
    }
    if slug.chars().all(|c| c.is_ascii_digit()) {
        return Err("slug must not be numeric (it would clash with project ids)");
    }
    Ok(())
}

async fn project(state: &AppState, r: &str) -> Result<ProjectRow, ApiErr> {
    state.db.project_by_ref(r).await.ok_or_else(|| not_found("project"))
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
        public_url: Some(state.public_url.clone()),
        retention_secs: Some(state.cfg.retention.events.as_secs()),
        max_window_secs: Some(state.cfg.query.max_window.as_secs()),
        sql: Some(state.cfg.query.enable_sql),
    })
}

#[utoipa::path(get, path = "/v1/projects", responses((status = 200, body = Vec<Project>)))]
async fn list_projects(State(state): S) -> ApiResult<Vec<Project>> {
    state.db.refresh_projects().await;
    Ok(Json(state.db.caches.projects().into_iter().map(|p| to_project(&state, p)).collect::<Result<_, _>>()?))
}

#[utoipa::path(post, path = "/v1/projects", request_body = CreateProject,
    responses((status = 201, body = Project), (status = 400, body = ApiError), (status = 409, body = ApiError)))]
async fn create_project(State(state): S, J(req): J<CreateProject>) -> Result<(StatusCode, Json<Project>), ApiErr> {
    let CreateProject { name, slug, platform } = req;
    let slug = slug.unwrap_or_else(|| slugify(&name));
    check_slug(&slug).map_err(bad)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(bad("name is required"));
    }
    let row = state.db.call(|reply| WriteCmd::CreateProject { name, slug, platform, reply }).await?;
    Ok((StatusCode::CREATED, Json(to_project(&state, row)?)))
}

#[utoipa::path(get, path = "/v1/projects/{project}", params(("project" = String, Path, description = "Project id or slug")),
    responses((status = 200, body = Project), (status = 404, body = ApiError)))]
async fn get_project(State(state): S, Path(p): Path<String>) -> ApiResult<Project> {
    Ok(Json(to_project(&state, project(&state, &p).await?)?))
}

#[utoipa::path(get, path = "/v1/projects/{project}/issues",
    params(("project" = String, Path, description = "Project id or slug"), IssueQuery, WindowRange,
        ("tag" = Option<Vec<String>>, Query, description = "Only events with this tag, `key:value`; repeat for more (all must match, max 10)")),
    description = "Issues with events in the window. Pages: pass `next_cursor` as `cursor` with the same filters; \
        the cursor carries the window and the sort.",
    responses((status = 200, body = IssueList), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn list_issues(
    State(state): S,
    Path(p): Path<String>,
    Q(q): Q<IssueQuery>,
    Q(r): Q<WindowRange>,
    RawQuery(raw): RawQuery,
) -> ApiResult<IssueList> {
    let pid = project(&state, &p).await?.id;
    let status = read::StatusFilter::parse(q.status.as_deref())
        .ok_or_else(|| bad("status must be unresolved, resolved, ignored or all"))?;
    let sort = read::IssueSort::parse(q.sort.as_deref())
        .ok_or_else(|| bad("sort must be last_seen, first_seen, count or users"))?;
    let tags = tag_filters(raw.as_deref())?;
    let after =
        q.cursor.as_deref().map(|c| read::IssueCursor::parse(c).ok_or_else(|| bad("invalid cursor"))).transpose()?;
    let w = match after {
        Some(c) if c.sort != sort => return Err(bad(format!("cursor is for sort={}", c.sort.as_str()))),
        // The cursor's window, so every page ranks the same events.
        Some(c) => {
            let at = |us| DateTime::from_timestamp_micros(us).ok_or_else(|| bad("invalid cursor"));
            let r = WindowRange { from: Some(at(c.from_us)?), to: Some(at(c.to_us)?), step: None };
            window(&state, None, &r, 14 * DAY)?
        }
        None => window(&state, q.since.as_deref(), &r, 14 * DAY)?,
    };
    Ok(Json(state.db.read(move |c, ns| read::list_issues(c, ns, pid, &q, &tags, &w, status, sort, after)).await?))
}

#[utoipa::path(get, path = "/v1/projects/{project}/issues/{issue_id}",
    params(("project" = String, Path), ("issue_id" = String, Path, description = "16 hex chars"), WindowQuery, WindowRange),
    responses((status = 200, body = IssueDetail), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn get_issue(
    State(state): S,
    Path((p, id)): Path<(String, String)>,
    Q(q): Q<WindowQuery>,
    Q(r): Q<WindowRange>,
) -> ApiResult<IssueDetail> {
    let pid = project(&state, &p).await?.id;
    let g = issue_hash(&id)?;
    let w = window(&state, q.since.as_deref(), &r, 14 * DAY)?;
    state.db.read(move |c, ns| read::issue_detail(c, ns, pid, g, &w)).await?.map(Json).ok_or_else(|| not_found("issue"))
}

/// Longest accepted `reason` of an issue update, in characters.
const MAX_REASON_CHARS: usize = 500;

#[utoipa::path(patch, path = "/v1/projects/{project}/issues/{issue_id}", request_body = UpdateIssue,
    params(("project" = String, Path), ("issue_id" = String, Path)),
    description = "Set the status. Idempotent: when the issue already has that status nothing is written and `changed` is false.",
    responses((status = 200, body = IssueDetail), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn update_issue(
    State(state): S,
    Path((p, id)): Path<(String, String)>,
    J(req): J<UpdateIssue>,
) -> ApiResult<IssueDetail> {
    let pid = project(&state, &p).await?.id;
    let g = issue_hash(&id)?;
    let reason = match req.reason.as_deref().map(str::trim) {
        None | Some("") => "user".to_string(),
        Some(r) if r.chars().count() > MAX_REASON_CHARS => {
            return Err(bad(format!("reason must be at most {MAX_REASON_CHARS} characters")));
        }
        Some(r) => r.to_string(),
    };
    let changed = state
        .db
        .call(|reply| WriteCmd::SetStatus { project_id: pid, group_hash: g, status: req.status, reason, reply })
        .await?;
    let w = window(&state, None, &WindowRange::default(), 14 * DAY)?;
    let mut d =
        state.db.read(move |c, ns| read::issue_detail(c, ns, pid, g, &w)).await?.ok_or_else(|| not_found("issue"))?;
    d.changed = Some(changed);
    Ok(Json(d))
}

#[utoipa::path(get, path = "/v1/projects/{project}/events", params(("project" = String, Path), EventQuery, WindowRange,
        ("tag" = Option<Vec<String>>, Query, description = "Only events with this tag, `key:value`; repeat for more (all must match, max 10)")),
    responses((status = 200, body = EventList), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn list_events(
    State(state): S,
    Path(p): Path<String>,
    Q(q): Q<EventQuery>,
    Q(r): Q<WindowRange>,
    RawQuery(raw): RawQuery,
) -> ApiResult<EventList> {
    let pid = project(&state, &p).await?.id;
    // No window unless one is asked for (all events, newest first).
    let w = if q.since.is_some() || r.from.is_some() || r.to.is_some() {
        Some(window(&state, q.since.as_deref(), &r, 14 * DAY)?)
    } else {
        None
    };
    let g = q.issue.as_deref().map(issue_hash).transpose()?;
    if q.trace_id.as_deref().is_some_and(|t| mallard_sentry_protocol::event::parse_trace_id(t).is_none()) {
        return Err(bad("trace_id must be 32 hex characters"));
    }
    let before =
        q.before.as_deref().map(|c| read::Cursor::parse(c).ok_or_else(|| bad("invalid cursor"))).transpose()?;
    let tags = tag_filters(raw.as_deref())?;
    Ok(Json(state.db.read(move |c, ns| read::list_events(c, ns, pid, g, &q, &tags, w.as_ref(), before)).await?))
}

#[utoipa::path(get, path = "/v1/events", params(TraceQuery, WindowRange),
    description = "Events of one trace across all projects (for linking errors to traces).",
    responses((status = 200, body = EventList), (status = 400, body = ApiError)))]
async fn events_by_trace(State(state): S, Q(q): Q<TraceQuery>, Q(r): Q<WindowRange>) -> ApiResult<EventList> {
    let t = mallard_sentry_protocol::event::parse_trace_id(&q.trace_id)
        .ok_or_else(|| bad("trace_id must be 32 hex characters"))?;
    let span = q
        .span_id
        .as_deref()
        .map(|s| {
            mallard_sentry_protocol::event::parse_span_id(s).ok_or_else(|| bad("span_id must be 16 hex characters"))
        })
        .transpose()?;
    let w = window(&state, q.since.as_deref(), &r, 30 * DAY)?;
    let limit = q.limit.unwrap_or(50);
    Ok(Json(state.db.read(move |c, ns| read::events_by_trace(c, ns, t, span, &w, limit)).await?))
}

#[utoipa::path(get, path = "/v1/events/by-trace", params(TracesQuery, WindowRange),
    description = "Error events of up to 200 traces at once, across all projects (for marking trace lists): \
        per trace the event count and the newest 50 events. Traces without events are left out.",
    responses((status = 200, body = EventsByTrace), (status = 400, body = ApiError)))]
async fn events_by_traces(State(state): S, Q(q): Q<TracesQuery>, Q(r): Q<WindowRange>) -> ApiResult<EventsByTrace> {
    let mut ids = q
        .trace_id
        .split(',')
        .map(|t| mallard_sentry_protocol::event::parse_trace_id(t.trim()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad("trace_id must be comma separated ids of 32 hex characters"))?;
    ids.sort();
    ids.dedup();
    if ids.len() > MAX_TRACES_PER_LOOKUP {
        return Err(bad(format!("at most {MAX_TRACES_PER_LOOKUP} trace ids per request")));
    }
    let w = window(&state, q.since.as_deref(), &r, 30 * DAY)?;
    Ok(Json(state.db.read(move |c, ns| read::events_by_traces(c, ns, &ids, &w)).await?))
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
    Q(q): Q<EventDetailQuery>,
) -> ApiResult<EventDetail> {
    let pid = project(&state, &p).await?.id;
    let eid = mallard_sentry_protocol::event::parse_uuid(&id).ok_or_else(|| bad("invalid event id"))?;
    let g = q.issue.as_deref().map(issue_hash).transpose()?;
    state
        .db
        .read(move |c, ns| read::event_detail(c, ns, Some(pid), eid, g))
        .await?
        .map(Json)
        .ok_or_else(|| not_found("event"))
}

#[utoipa::path(get, path = "/v1/events/{event_id}",
    params(("event_id" = String, Path, description = "32 hex chars")),
    description = "One event by id, in any project (for looking up an id without knowing its project).",
    responses((status = 200, body = EventDetail), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn find_event(State(state): S, Path(id): Path<String>) -> ApiResult<EventDetail> {
    let eid = mallard_sentry_protocol::event::parse_uuid(&id).ok_or_else(|| bad("invalid event id"))?;
    state
        .db
        .read(move |c, ns| read::event_detail(c, ns, None, eid, None))
        .await?
        .map(Json)
        .ok_or_else(|| not_found("event"))
}

#[utoipa::path(get, path = "/v1/projects/{project}/stats", params(("project" = String, Path), WindowQuery, WindowRange),
    responses((status = 200, body = ProjectStats), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn project_stats(
    State(state): S,
    Path(p): Path<String>,
    Q(q): Q<WindowQuery>,
    Q(r): Q<WindowRange>,
) -> ApiResult<ProjectStats> {
    let pid = project(&state, &p).await?.id;
    let w = window(&state, q.since.as_deref(), &r, DAY)?;
    // Without a step, read.rs picks one (about 24 buckets).
    let step = q.interval.as_deref().or(r.step.as_deref());
    let interval_us = step.map(|s| w.step(Some(s)).map(|secs| secs as i64 * 1_000_000)).transpose()?;
    Ok(Json(state.db.read(move |c, ns| read::project_stats(c, ns, pid, &w, interval_us)).await?))
}

#[utoipa::path(get, path = "/v1/projects/{project}/releases", params(("project" = String, Path), ReleaseQuery, WindowRange),
    description = "Releases named by the project's events in the window (default 14d), newest last event first. \
        Derived from events: releases created with sentry-cli are not stored.",
    responses((status = 200, body = ReleaseList), (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn list_releases(
    State(state): S,
    Path(p): Path<String>,
    Q(q): Q<ReleaseQuery>,
    Q(r): Q<WindowRange>,
) -> ApiResult<ReleaseList> {
    let pid = project(&state, &p).await?.id;
    let w = window(&state, q.since.as_deref(), &r, 14 * DAY)?;
    let limit = q.limit.unwrap_or(50);
    Ok(Json(state.db.read(move |c, ns| read::list_releases(c, ns, pid, &w, limit)).await?))
}

/// Longest a tag suggestion may take. Suggestions are asked for while someone
/// types; a slow answer is of no use, and the connection is shared with ingest.
const TAGS_TIMEOUT: Duration = Duration::from_secs(10);

#[utoipa::path(get, path = "/v1/projects/{project}/tags", params(("project" = String, Path), TagQuery, WindowRange),
    description = "Tag keys of the project's events in the window (default 14d), or the top values of `key`, \
        most frequent first, with counts from a sample. For suggestions while typing a filter.",
    responses((status = 200, body = TagStats), (status = 400, body = ApiError), (status = 404, body = ApiError),
        (status = 504, body = ApiError)))]
async fn tag_stats(
    State(state): S,
    Path(p): Path<String>,
    Q(q): Q<TagQuery>,
    Q(r): Q<WindowRange>,
) -> ApiResult<TagStats> {
    let pid = project(&state, &p).await?.id;
    let w = window(&state, q.since.as_deref(), &r, 14 * DAY)?;
    if q.key.as_deref().is_some_and(|k| k.trim().is_empty()) {
        return Err(bad("key must not be empty"));
    }
    let limit = match q.limit {
        None => 50,
        Some(0) => return Err(bad("limit must be at least 1")),
        Some(n) => n.min(1000),
    };
    let run = state.db.read_for(TAGS_TIMEOUT, move |c, ns| {
        let search = read::TagSearch { key: q.key.as_deref(), prefix: q.prefix.as_deref(), limit };
        read::tag_stats(c, ns, pid, &w, &search)
    });
    run.await?.map(Json).ok_or_else(|| timeout(TAGS_TIMEOUT))
}

fn timeout(limit: Duration) -> ApiErr {
    ApiErr::new(
        StatusCode::GATEWAY_TIMEOUT,
        "timeout",
        format!("query exceeded the {} time limit", humantime_secs(limit)),
    )
}

#[utoipa::path(get, path = "/v1/projects/{project}/sourcemaps", params(("project" = String, Path)),
    responses((status = 200, body = Vec<ArtifactBundle>), (status = 404, body = ApiError)))]
async fn list_sourcemaps(State(state): S, Path(p): Path<String>) -> ApiResult<Vec<ArtifactBundle>> {
    let pid = project(&state, &p).await?.id;
    Ok(Json(state.db.read(move |c, ns| read::list_bundles(c, ns, pid)).await?))
}

#[utoipa::path(post, path = "/v1/maintenance",
    description = "Flush inlined data to Parquet, apply retention, compact files and expire snapshots now.",
    responses((status = 204), (status = 500, body = ApiError)))]
async fn run_maintenance(State(state): S) -> Result<StatusCode, ApiErr> {
    state.db.call(|reply| WriteCmd::Maintenance { reply }).await.map_err(ApiErr::from)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/schema",
    description = "The tables `POST /v1/sql` can query, with their columns. Works when SQL is off.",
    responses((status = 200, body = Schema)))]
async fn get_schema(State(state): S) -> ApiResult<Schema> {
    Ok(Json(state.db.read(read::schema).await?))
}

#[utoipa::path(post, path = "/v1/sql", request_body = SqlRequest,
    description = "Read-only SQL over `events`, `issues` and `issue_activity` (ADR 0011). Off unless \
        `sentry.query.enable_sql` is set; one SELECT, checked before it runs, interrupted after `sentry.query.sql_timeout`.",
    responses((status = 200, body = SqlResponse), (status = 400, body = ApiError), (status = 403, body = ApiError),
        (status = 504, body = ApiError)))]
async fn run_sql(State(state): S, J(req): J<SqlRequest>) -> ApiResult<SqlResponse> {
    if !state.cfg.query.enable_sql {
        return Err(ApiErr::new(
            StatusCode::FORBIDDEN,
            "disabled",
            "SQL over the error tables is disabled; set sentry.query.enable_sql = true in the mallard config",
        ));
    }
    let limit = req.limit.unwrap_or(1000);
    if !(1..=100_000).contains(&limit) {
        return Err(bad("limit must be between 1 and 100000"));
    }
    let timeout = state.cfg.query.sql_timeout;
    let tables = read::SQL_TABLES.map(|(name, _)| name);
    let run = state
        .db
        .read_for(timeout, move |c, _| Ok(mallard_duck::sql::run(c, crate::SCHEMA, &tables, &req.query, limit)))
        .await?;
    match run {
        None => Err(self::timeout(timeout)),
        Some(Ok(r)) => Ok(Json(SqlResponse {
            columns: r.columns.into_iter().map(|(name, data_type)| SqlColumn { name, data_type }).collect(),
            rows: r.rows,
            truncated: r.truncated,
            elapsed_ms: r.elapsed_ms,
        })),
        Some(Err(mallard_duck::sql::SqlError::Rejected(m))) => Err(bad(m)),
        Some(Err(mallard_duck::sql::SqlError::Internal(e))) => Err(e.into()),
    }
}

/// `30s`, `1.5s`.
fn humantime_secs(d: Duration) -> String {
    format!("{}s", d.as_secs_f64())
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
    let base = state.public_url.trim_end_matches('/');
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
// Stubs: `sentry-cli releases new|finalize|deploys` succeed and nothing is
// stored; `GET /v1/projects/{p}/releases` derives releases from events
// (work/issues/0060).
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
    fn write_errors_map_by_variant_not_text() {
        use super::*;
        let status = |e: WriteError| ApiErr::from(e).status;
        assert_eq!(status(WriteError::SlugTaken("x".into())), StatusCode::CONFLICT);
        assert_eq!(status(WriteError::SameApp("x".into())), StatusCode::CONFLICT);
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
