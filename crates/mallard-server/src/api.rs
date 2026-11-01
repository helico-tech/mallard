//! The API that spans both sides (ADR 0027, 0028), behind `server.api_tokens`:
//! `GET /api/v1/apps` and `GET /api/v1/find`. It asks each library through its
//! `Server` methods and knows nothing of their tables. A side that fails does not
//! fail the answer, unless it may have changed it.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode, middleware};
use chrono::TimeDelta;
use mallard_api::{ApiError, App, AppList, AppsQuery, FindId, FindMatch, FindQuery, FindResult};
use mallard_common::{
    auth::{AuthPolicy, Tokens},
    http::{ApiErr, BearerAuth, Q, require_bearer},
    window::Window,
};
use mallard_otel_server::Server as Otel;
use mallard_sentry_server::Server as Sentry;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

/// The window of a call without one. Pickers send theirs.
const APPS_WINDOW: Duration = Duration::from_secs(86_400);
/// Where `find` looks without a window, the same as `mallard find`.
const FIND_WINDOW: Duration = Duration::from_secs(7 * 86_400);

pub(crate) struct ApiState {
    pub otel: Arc<Otel>,
    pub sentry: Sentry,
}

type S = State<Arc<ApiState>>;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "mallard API",
        description = "What spans errors and telemetry: the list of apps and looking up any id.",
        license(name = "MIT OR Apache-2.0")
    ),
    components(schemas(App, AppList, FindMatch, FindResult, ApiError)),
    modifiers(&BearerAuth),
    security(("bearer" = []))
)]
struct ApiDoc;

fn api_routes() -> OpenApiRouter<Arc<ApiState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi()).routes(routes!(apps)).routes(routes!(find))
}

pub(crate) fn router(state: Arc<ApiState>, api_tokens: &[String]) -> Router {
    api_routes()
        .layer(middleware::from_fn_with_state(Tokens::new(api_tokens, AuthPolicy::Required), require_bearer))
        .with_state(state)
        .split_for_parts()
        .0
}

/// The OpenAPI document (`docs/openapi.json`; `tests/openapi.rs` checks it).
pub fn openapi() -> utoipa::openapi::OpenApi {
    api_routes().split_for_parts().1
}

/// A valid project slug for a service name (`unknown_service:node` →
/// `unknown-service-node`). The one slug rule: a project and a service with the
/// same slug under it are one app, and the bridge sends exceptions there. Project
/// slugs go through it too, see [`app_projects`].
pub(crate) fn service_slug(service: &str) -> String {
    let mut s = mallard_sentry_api::slugify(service);
    s.truncate(64);
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "unknown-service".into()
    } else if s.bytes().all(|b| b.is_ascii_digit()) {
        format!("service-{s}")
    } else {
        s
    }
}

/// The project that takes the services of each [`service_slug`], from `(id, slug)`.
/// A project may have a slug the rule never gives (`my_app`), and it still joins the
/// service `my_app`. If several projects share a slug under the rule, the one whose slug
/// is exactly that wins, and then the oldest. The others stay apps of their own.
pub(crate) fn app_projects<'a>(projects: impl IntoIterator<Item = (u32, &'a str)>) -> HashMap<String, (u32, &'a str)> {
    let mut found: HashMap<String, (u32, &str)> = HashMap::new();
    for (id, slug) in projects {
        let key = service_slug(slug);
        let rank = |(id, slug): (u32, &str)| (slug != key, id);
        match found.get_mut(&key) {
            Some(best) if rank((id, slug)) < rank(*best) => *best = (id, slug),
            Some(_) => {}
            None => {
                found.insert(key, (id, slug));
            }
        }
    }
    found
}

/// One list from `(id, slug, name)` of the projects and the service names. An app with a
/// project has the project's slug, because clients use it as the project's name.
fn merge_apps(projects: Vec<(u32, String, String)>, services: Vec<String>) -> Vec<App> {
    let joins: HashMap<String, String> = app_projects(projects.iter().map(|(id, slug, _)| (*id, slug.as_str())))
        .into_iter()
        .map(|(key, (_, slug))| (key, slug.to_string()))
        .collect();
    let mut apps: BTreeMap<String, App> = projects
        .into_iter()
        .map(|(id, slug, name)| (slug.clone(), App { slug, name, project: Some(id), services: vec![] }))
        .collect();
    for service in services {
        let key = service_slug(&service);
        let slug = joins.get(&key).cloned().unwrap_or(key);
        let app = apps.entry(slug.clone()).or_insert_with(|| App {
            slug,
            name: service.clone(),
            project: None,
            services: vec![],
        });
        app.services.push(service);
    }
    apps.into_values().collect()
}

/// The last `max` of `w`, or `w` when it fits.
fn cut_to(w: Window, max: Duration) -> Window {
    let fits = w.to.signed_duration_since(w.from).to_std().is_ok_and(|d| d <= max);
    let from = TimeDelta::from_std(max).ok().and_then(|d| w.to.checked_sub_signed(d));
    match from {
        Some(from) if !fits => Window { from, to: w.to },
        _ => w,
    }
}

#[utoipa::path(get, path = "/api/v1/apps", tag = "apps", params(AppsQuery),
    description = "Every app: a project and the services with the same slug, or either alone. Projects are \
        listed whatever the window; services need data in it. A side that cannot be read, or a window longer \
        than `otel.query.max_window`, gives what there is plus `warnings`.",
    responses((status = 200, body = AppList), (status = 400, body = ApiError), (status = 401, body = ApiError),
        (status = 500, body = ApiError), (status = 503, body = ApiError), (status = 504, body = ApiError)))]
async fn apps(State(st): S, Q(q): Q<AppsQuery>) -> Result<Json<AppList>, ApiErr> {
    let w = Window::resolve(q.from, q.to, q.since.as_deref(), APPS_WINDOW, Duration::MAX)?;
    let mut warnings = Vec::new();
    let telemetry = cut_to(w, st.otel.max_window());
    if telemetry != w {
        warnings.push("telemetry: services are from the end of the window only, up to otel.query.max_window".into());
    }
    let (projects, services) = tokio::join!(st.sentry.projects(), st.otel.service_names(telemetry));
    let (projects, services) = match (projects, services) {
        (Err(e), Err(_)) => return Err(e),
        all => all,
    };
    let projects = projects.map(|p| p.into_iter().map(|p| (p.id, p.slug, p.name)).collect()).unwrap_or_else(|e| {
        warnings.push(format!("errors: projects could not be read: {}", e.message));
        vec![]
    });
    let services = match services {
        Ok((names, more)) => {
            if more {
                warnings.push(format!("telemetry: only the first {} services", names.len()));
            }
            names
        }
        Err(e) => {
            warnings.push(format!("telemetry: services could not be read: {}", e.message));
            vec![]
        }
    };
    Ok(Json(AppList { apps: merge_apps(projects, services), warnings }))
}

/// What one lookup left: matches, and failures with the source they came from.
#[derive(Default)]
struct Found {
    matches: Vec<FindMatch>,
    failed: Vec<(&'static str, ApiErr)>,
}

impl Found {
    /// The value, or nothing and a note of the failure.
    fn take<T>(&mut self, source: &'static str, r: Result<T, ApiErr>) -> Option<T> {
        r.map_err(|e| self.failed.push((source, e))).ok()
    }

    /// Matches and warnings. No match: the first failure if a source failed, as it
    /// may have hidden the answer, and a 404 only when every source answered.
    fn finish(self, id: &str) -> Result<FindResult, ApiErr> {
        let warnings = self.failed.iter().map(|(source, e)| format!("{source}: {}", e.message)).collect();
        if !self.matches.is_empty() {
            return Ok(FindResult { matches: self.matches, warnings });
        }
        match self.failed.into_iter().next() {
            Some((_, e)) => Err(e),
            None => Err(ApiErr::not_found(format!("nothing found for {id}"))),
        }
    }
}

#[utoipa::path(get, path = "/api/v1/find", tag = "find", params(FindQuery),
    description = "What an id points at. 32 hex: a trace and/or a Sentry event. 16 hex: issues and/or a span. \
        A source that fails while something else was found adds a warning. When nothing was found, its error \
        is the answer (5xx), since it may have hidden the match: a 404 means every source was read.",
    responses((status = 200, body = FindResult), (status = 400, body = ApiError), (status = 401, body = ApiError),
        (status = 404, description = "No match (`not_found`), or no such project (`unknown_project`)", body = ApiError),
        (status = 500, body = ApiError), (status = 503, body = ApiError), (status = 504, body = ApiError)))]
async fn find(State(st): S, Q(q): Q<FindQuery>) -> Result<Json<FindResult>, ApiErr> {
    let id = FindId::parse(&q.id).ok_or_else(|| {
        ApiErr::bad_request("id must be a trace or event id (32 hex) or an issue or span id (16 hex)")
    })?;
    // A bad window is a 400 here, not one failed source per side. Only a window
    // above one side's limit stays a failure of that side.
    Window::resolve(q.from, q.to, q.since.as_deref(), FIND_WINDOW, Duration::MAX)?;
    let found = match &id {
        FindId::Hex32(id) => find_hex32(&st, id, &q).await,
        FindId::Hex16(id) => find_hex16(&st, id, &q).await,
    };
    let (FindId::Hex32(id) | FindId::Hex16(id)) = &id;
    Ok(Json(found.finish(id)?))
}

/// The window of a query against one side's limit.
fn window_of(q: &FindQuery, max: Duration) -> Result<Window, ApiErr> {
    Ok(Window::resolve(q.from, q.to, q.since.as_deref(), FIND_WINDOW, max)?)
}

/// The project's slug for links; its id, which links also accept, if it is gone.
async fn slug_of(st: &ApiState, id: u32) -> String {
    st.sentry.project(&id.to_string()).await.map_or_else(|| id.to_string(), |p| p.slug)
}

/// A trace (its spans, errors and logs, each read on its own) and a Sentry event.
async fn find_hex32(st: &ApiState, id: &str, q: &FindQuery) -> Found {
    let uuid = uuid::Uuid::parse_str(id).expect("32 hex characters");
    let spans = async { st.otel.trace_spans(window_of(q, st.otel.max_window())?, id.to_string()).await };
    let logs = async { st.otel.trace_logs(window_of(q, st.otel.max_window())?, id.to_string()).await };
    let errors = async { st.sentry.trace_events(window_of(q, st.sentry.max_window())?, uuid).await };
    let (spans, errors, logs, event) = tokio::join!(spans, errors, logs, st.sentry.find_event(uuid));
    let mut out = Found::default();
    let spans = out.take("spans", spans);
    let errors = out.take("errors", errors).unwrap_or_default();
    let logs = out.take("logs", logs);
    let first = [spans.and_then(|s| s.first), errors.iter().map(|e| e.timestamp).min(), logs.and_then(|l| l.first)];
    if let Some(time) = first.into_iter().flatten().min() {
        out.matches.push(FindMatch::Trace {
            trace_id: id.to_string(),
            time,
            spans: spans.map_or(0, |s| s.count),
            errors: errors.len() as u64,
            logs: logs.map_or(0, |l| l.count),
        });
    }
    if let Some(Some(e)) = out.take("event", event) {
        let e = e.summary;
        out.matches.push(FindMatch::Event {
            project: slug_of(st, e.project_id).await,
            event_id: e.event_id,
            issue_id: e.issue_id,
            project_id: e.project_id,
            title: e.title,
            trace_id: e.trace_id,
        });
    }
    out
}

/// Issues, in `project` or in all projects, and a span.
async fn find_hex16(st: &ApiState, id: &str, q: &FindQuery) -> Found {
    let group = u64::from_str_radix(id, 16).expect("16 hex characters");
    let issues = async {
        // Read, not from the cache: a failed read must not look like no such project.
        let project = match &q.project {
            None => None,
            Some(r) => {
                let projects = st.sentry.projects().await?;
                let project = projects.iter().find(|p| p.slug == *r || p.id.to_string() == *r);
                let not_found = || ApiErr::new(StatusCode::NOT_FOUND, "unknown_project", "project not found");
                Some(project.ok_or_else(not_found)?.id)
            }
        };
        st.sentry.find_issues(group, project).await
    };
    let span = async { st.otel.find_span(window_of(q, st.otel.max_window())?, id.to_string()).await };
    let (issues, span) = tokio::join!(issues, span);
    let mut out = Found::default();
    for (project_id, d) in out.take("issues", issues).unwrap_or_default() {
        out.matches.push(FindMatch::Issue {
            project: slug_of(st, project_id).await,
            project_id,
            issue_id: d.summary.id,
            title: d.summary.title,
            status: d.summary.status.to_string(),
            count: d.summary.count,
        });
    }
    if let Some(Some(s)) = out.take("span", span) {
        out.matches.push(FindMatch::Span {
            span_id: s.span_id,
            trace_id: s.trace_id,
            service: s.service_name,
            name: s.name,
            time: s.start_time,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_become_valid_slugs() {
        assert_eq!(service_slug("checkout"), "checkout");
        assert_eq!(service_slug("Checkout API"), "checkout-api");
        assert_eq!(service_slug("unknown_service:node"), "unknown-service-node");
        assert_eq!(service_slug("42"), "service-42");
        assert_eq!(service_slug("::"), "unknown-service");
        assert_eq!(service_slug(&"a".repeat(80)).len(), 64);
    }

    #[test]
    fn apps_join_on_the_slug() {
        let apps = merge_apps(
            vec![(2, "checkout-api".into(), "Checkout".into()), (1, "web".into(), "Web".into())],
            vec!["Checkout API".into(), "checkout-api".into(), "worker".into()],
        );
        let slugs: Vec<&str> = apps.iter().map(|a| a.slug.as_str()).collect();
        assert_eq!(slugs, ["checkout-api", "web", "worker"]);
        assert_eq!(apps[0].project, Some(2));
        assert_eq!(apps[0].name, "Checkout");
        assert_eq!(apps[0].services, ["Checkout API", "checkout-api"], "two names, one app");
        assert!(apps[1].services.is_empty(), "a project without telemetry is an app");
        assert_eq!((apps[2].project, apps[2].name.as_str()), (None, "worker"), "so is a service without a project");
    }

    /// `check_slug` allows `_` and `--`, which the rule never gives (issue 0142).
    #[test]
    fn a_project_slug_the_rule_never_gives_still_joins_its_service() {
        let apps = merge_apps(
            vec![(1, "my_app".into(), "Mine".into()), (2, "a--b".into(), "Ab".into())],
            vec!["my_app".into(), "My App".into(), "my-app".into(), "a-b".into(), "other_one".into()],
        );
        let slugs: Vec<&str> = apps.iter().map(|a| a.slug.as_str()).collect();
        assert_eq!(slugs, ["a--b", "my_app", "other-one"], "the project's own slug, which clients send back");
        assert_eq!((apps[0].project, apps[0].services.as_slice()), (Some(2), ["a-b".to_string()].as_slice()));
        assert_eq!(apps[1].project, Some(1));
        assert_eq!(apps[1].services, ["my_app", "My App", "my-app"]);
        assert_eq!(apps[2].project, None, "without a project the slug is still the rule's");
    }

    /// A server that already split `my_app` has both projects. No project is merged or
    /// renamed, and the services stay where they were.
    #[test]
    fn projects_with_one_slug_stay_two_apps() {
        let apps =
            merge_apps(vec![(1, "my_app".into(), "a".into()), (2, "my-app".into(), "b".into())], vec!["my_app".into()]);
        assert_eq!(apps.len(), 2);
        assert_eq!((apps[0].slug.as_str(), apps[0].services.len()), ("my-app", 1), "the exact slug wins");
        assert_eq!((apps[1].slug.as_str(), apps[1].services.len()), ("my_app", 0));
    }

    #[test]
    fn the_oldest_project_wins_when_none_has_the_exact_slug() {
        for order in [[(7, "a__b"), (3, "a_b")], [(3, "a_b"), (7, "a__b")]] {
            assert_eq!(app_projects(order)["a-b"], (3, "a_b"), "{order:?}");
        }
        assert_eq!(app_projects([(3, "a_b"), (7, "a-b")])["a-b"], (7, "a-b"));
        assert_eq!(app_projects([(7, "a-b"), (3, "a_b")])["a-b"], (7, "a-b"));
    }

    #[test]
    fn a_long_window_is_cut_at_its_end() {
        let w = Window::resolve(None, None, Some("40d"), APPS_WINDOW, Duration::MAX).unwrap();
        let max = Duration::from_secs(31 * 86_400);
        let cut = cut_to(w, max);
        assert_eq!(cut.to, w.to);
        assert_eq!((cut.to - cut.from).num_days(), 31);
        let short = Window::resolve(None, None, Some("1d"), APPS_WINDOW, Duration::MAX).unwrap();
        assert_eq!(cut_to(short, max), short);
    }

    /// A server on a real lake, plus a connection to break it with.
    async fn platform(
        dir: &std::path::Path,
        change: impl FnOnce(&mut crate::Config),
    ) -> (crate::Platform, mallard_duck::duckdb::Connection) {
        let mut cfg = crate::Config::default();
        cfg.server.api_tokens = vec!["t".into()];
        cfg.storage.catalog = dir.join("catalog.ducklake").display().to_string();
        cfg.storage.data_path = format!("{}/lake/", dir.display());
        change(&mut cfg);
        let conn = mallard_duck::open_connection(&cfg.storage, &cfg.duckdb).unwrap();
        let otel = Arc::new(Otel::with_connection(cfg.otel_config(), conn.try_clone().unwrap()).await.unwrap());
        let sentry =
            Sentry::with_connection(cfg.sentry.clone(), cfg.sentry_host(), conn.try_clone().unwrap()).await.unwrap();
        let bridge = crate::bridge::install(&cfg.bridge, &sentry, &otel).unwrap();
        (crate::Platform { cfg: Arc::new(cfg), otel, sentry, bridge, metrics: None }, conn)
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let req = axum::http::Request::get(uri).header("authorization", "Bearer t");
        let r = app.clone().oneshot(req.body(axum::body::Body::empty()).unwrap()).await.unwrap();
        let status = r.status();
        let body = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    const SPAN: &str = "INSERT INTO lake.otel.spans (start_time, duration_ns, trace_id, span_id, service_name, name, kind, status_code)
        VALUES (now()::TIMESTAMP - INTERVAL 1 HOUR, 1000, '0af76519-16cd-43dd-8448-eb211c80319c', 42, 'Web', 'GET /', 'SERVER', 'OK')";
    const TRACE: &str = "0af7651916cd43dd8448eb211c80319c";

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_side_failing_does_not_fail_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let (p, conn) = platform(dir.path(), |_| {}).await;
        p.sentry.create_project("Web", "web", None).await.unwrap();
        conn.execute_batch(SPAN).unwrap();
        let app = p.http_router();
        let (s, v) = get(&app, "/api/v1/apps?since=1d").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["apps"][0]["slug"], "web");
        assert_eq!(v["apps"][0]["services"], serde_json::json!(["Web"]));
        assert_eq!(v["warnings"], serde_json::json!([]));

        // Telemetry broken: the projects, and a warning that does not leak why.
        conn.execute_batch("DROP TABLE lake.otel.logs").unwrap();
        let (s, v) = get(&app, "/api/v1/apps?since=1d").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["apps"][0]["project"], serde_json::json!(1), "{v}");
        assert_eq!(v["apps"][0]["services"], serde_json::json!([]));
        let warning = v["warnings"][0].as_str().unwrap();
        assert!(warning.starts_with("telemetry:") && !warning.contains("logs"), "{warning}");

        // Both broken: no list to give.
        conn.execute_batch("DROP TABLE lake.sentry.projects").unwrap();
        let (s, v) = get(&app, "/api/v1/apps?since=1d").await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
        p.sentry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_errors_side_failing_leaves_the_services() {
        let dir = tempfile::tempdir().unwrap();
        let (p, conn) = platform(dir.path(), |_| {}).await;
        conn.execute_batch(SPAN).unwrap();
        conn.execute_batch("DROP TABLE lake.sentry.projects").unwrap();
        let (s, v) = get(&p.http_router(), "/api/v1/apps?since=1d").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!((v["apps"][0]["slug"].as_str(), v["apps"][0]["project"].as_u64()), (Some("web"), None), "{v}");
        assert!(v["warnings"][0].as_str().unwrap().starts_with("errors:"), "{v}");
        p.sentry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_window_above_max_window_is_no_400() {
        let dir = tempfile::tempdir().unwrap();
        let (p, conn) = platform(dir.path(), |c| c.otel.query.max_window = Duration::from_secs(2 * 3600)).await;
        p.sentry.create_project("Web", "web", None).await.unwrap();
        conn.execute_batch(SPAN).unwrap();
        let (s, v) = get(&p.http_router(), "/api/v1/apps?since=30d").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["apps"][0]["services"], serde_json::json!(["Web"]), "the last two hours are still read");
        assert!(v["warnings"][0].as_str().unwrap().contains("max_window"), "{v}");
        p.sentry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_side_is_never_a_404() {
        let dir = tempfile::tempdir().unwrap();
        let (p, conn) = platform(dir.path(), |_| {}).await;
        let app = p.http_router();
        // Everything read: a real no match.
        let uri = format!("/api/v1/find?id={TRACE}");
        let (s, v) = get(&app, &uri).await;
        assert_eq!((s, v["error"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")), "{v}");

        // One source down and nothing found: it may have hidden the answer.
        conn.execute_batch("DROP TABLE lake.otel.logs").unwrap();
        let (s, v) = get(&app, &uri).await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{v}");

        // Something found: the match and a warning.
        conn.execute_batch(SPAN).unwrap();
        let (s, v) = get(&app, &uri).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!((v["matches"][0]["kind"].as_str(), v["matches"][0]["spans"].as_u64()), (Some("trace"), Some(1)));
        assert!(v["matches"][0]["time"].is_string(), "{v}");
        assert!(v["warnings"][0].as_str().unwrap().starts_with("logs:"), "{v}");

        // A span id with its side down: the same.
        conn.execute_batch("DROP TABLE lake.otel.spans").unwrap();
        let (s, v) = get(&app, "/api/v1/find?id=85fede14250ec6be").await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
        p.sentry.shutdown().await;
    }

    /// Suggestions read the newest part of a wide window once it has enough rows.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wide_suggestions_read_only_the_newest_rows() {
        let dir = tempfile::tempdir().unwrap();
        let (p, conn) = platform(dir.path(), |_| {}).await;
        let id = p.sentry.create_project("Web", "web", None).await.unwrap();
        // 500,000 rows in the last half hour, one more two days ago.
        conn.execute_batch(
            "INSERT INTO lake.otel.spans (start_time, duration_ns, trace_id, span_id, service_name, name, kind, status_code, attributes)
             SELECT now()::TIMESTAMP - INTERVAL 30 MINUTE, 1, uuid(), i, 'busy', 'x', 'SERVER', 'OK', MAP {'k': 'new'}
             FROM range(500000) t(i);
             INSERT INTO lake.otel.spans (start_time, duration_ns, trace_id, span_id, service_name, name, kind, status_code, attributes)
             VALUES (now()::TIMESTAMP - INTERVAL 2 DAY, 1, uuid(), 1, 'quiet', 'x', 'SERVER', 'OK', MAP {'k': 'old'});",
        )
        .unwrap();
        let columns = "project_id, received_at, timestamp, event_id, group_hash, level, platform, title, size_bytes, payload, tags";
        conn.execute_batch(&format!(
            "INSERT INTO lake.sentry.events ({columns})
             SELECT {id}, now()::TIMESTAMP, now()::TIMESTAMP - INTERVAL 30 MINUTE, uuid(), 1, 'error', 'node', 't', 1, '{{}}', MAP {{'k': 'new'}}
             FROM range(500000) t(i);
             INSERT INTO lake.sentry.events ({columns})
             VALUES ({id}, now()::TIMESTAMP - INTERVAL 2 DAY, now()::TIMESTAMP - INTERVAL 2 DAY, uuid(), 1, 'error', 'node', 't', 1, '{{}}', MAP {{'k': 'old'}});"
        ))
        .unwrap();
        let app = p.http_router();
        let hours_back = |v: &serde_json::Value| {
            let from: chrono::DateTime<chrono::Utc> = v["counted_from"].as_str().unwrap().parse().unwrap();
            (chrono::Utc::now() - from).num_minutes() as f64 / 60.0
        };
        let values = |v: &serde_json::Value| {
            v["items"].as_array().unwrap().iter().map(|i| i["value"].clone()).collect::<Vec<_>>()
        };
        let new = vec![serde_json::json!("new")];

        for path in ["/otel/api/v1/attributes?signal=spans&", "/sentry/v1/projects/web/tags?"] {
            // A wide window: the last hour has enough, so that is all that is read.
            let (s, v) = get(&app, &format!("{path}since=30d&key=k")).await;
            assert_eq!(s, StatusCode::OK, "{v}");
            assert_eq!(values(&v), new, "{v}");
            assert_eq!(v["total_rows"], 500_000, "{v}");
            assert_eq!(v["sampled_rows"], 10_000, "{v}");
            assert!((hours_back(&v) - 1.0).abs() < 0.1, "{v}");
            // A window that starts later than one step: read as it is.
            let (_, v) = get(&app, &format!("{path}since=45m&key=k")).await;
            assert!((hours_back(&v) - 0.75).abs() < 0.1, "{v}");
        }

        // Too few rows of this service in every part: the whole window, the old value too.
        let (s, v) = get(&app, "/otel/api/v1/attributes?signal=spans&since=30d&key=k&service=quiet").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(values(&v), [serde_json::json!("old")], "{v}");
        assert!(hours_back(&v) > 700.0, "{v}");
        p.sentry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_project_read_that_fails_is_no_unknown_project() {
        let dir = tempfile::tempdir().unwrap();
        let (p, conn) = platform(dir.path(), |_| {}).await;
        let app = p.http_router();
        let (s, v) = get(&app, "/api/v1/find?id=85fede14250ec6be&project=nope").await;
        assert_eq!((s, v["error"].as_str()), (StatusCode::NOT_FOUND, Some("unknown_project")), "{v}");
        conn.execute_batch("DROP TABLE lake.sentry.projects").unwrap();
        let (s, v) = get(&app, "/api/v1/find?id=85fede14250ec6be&project=nope").await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
        p.sentry.shutdown().await;
    }
}
