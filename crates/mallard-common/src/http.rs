//! HTTP pieces both APIs share: the JSON error body, extractors that use
//! it, the bearer-token middleware, CORS for ingest, API docs, readiness.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{FromRequest, FromRequestParts, Query, Request, State},
    http::{HeaderName, Method, StatusCode, header, request::Parts},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::{ApiError, auth::Tokens, window::WindowError};

/// An error answer of either API: a status plus [`ApiError`] as JSON.
#[derive(Debug)]
pub struct ApiErr {
    pub status: StatusCode,
    /// Machine readable code, e.g. `bad_request`.
    pub code: &'static str,
    pub message: String,
}

impl ApiErr {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "invalid or missing bearer token")
    }

    /// A 500 whose details stay in the server log.
    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error; see the server log")
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.code.into(), message: self.message })).into_response()
    }
}

/// Unexpected errors: logged in full, answered as a plain 500.
impl From<anyhow::Error> for ApiErr {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = format!("{e:#}"), "api error");
        Self::internal()
    }
}

impl From<WindowError> for ApiErr {
    fn from(e: WindowError) -> Self {
        Self::bad_request(e.to_string())
    }
}

/// `Query` extractor whose errors use the JSON error body.
pub struct Q<T>(pub T);

impl<T: serde::de::DeserializeOwned, S: Send + Sync> FromRequestParts<S> for Q<T> {
    type Rejection = ApiErr;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiErr> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(v)| Q(v))
            .map_err(|e| ApiErr::bad_request(e.body_text()))
    }
}

/// `Json` extractor whose errors use the JSON error body.
pub struct J<T>(pub T);

impl<T: serde::de::DeserializeOwned, S: Send + Sync> FromRequest<S> for J<T> {
    type Rejection = ApiErr;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiErr> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| J(v))
            .map_err(|e| ApiErr::new(e.status(), "bad_request", e.body_text()))
    }
}

/// Middleware: answer 401 unless the request carries an accepted bearer
/// token. Use with `axum::middleware::from_fn_with_state(tokens, require_bearer)`.
pub async fn require_bearer(State(tokens): State<Tokens>, req: Request, next: Next) -> Response {
    let header = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if tokens.allows(header) { next.run(req).await } else { ApiErr::unauthorized().into_response() }
}

/// CORS for ingest endpoints that browsers post to: any origin, `POST`,
/// the usual headers plus `extra_headers`; `Retry-After` plus
/// `extra_exposed` are readable by the page.
pub fn ingest_cors(extra_headers: &[&'static str], extra_exposed: &[&'static str]) -> CorsLayer {
    let base = [header::CONTENT_TYPE, header::CONTENT_ENCODING, header::AUTHORIZATION, header::ACCEPT, header::ORIGIN];
    let allow = base.into_iter().chain(extra_headers.iter().map(|h| HeaderName::from_static(h)));
    let expose = std::iter::once(header::RETRY_AFTER).chain(extra_exposed.iter().map(|h| HeaderName::from_static(h)));
    CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers(allow.collect::<Vec<_>>())
        .expose_headers(expose.collect::<Vec<_>>())
        .max_age(Duration::from_secs(3600))
}

/// `bearer` security scheme for an OpenAPI document (`modifiers(&BearerAuth)`).
pub struct BearerAuth;

impl utoipa::Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

/// `/openapi.json` and `/docs` (an API reference page). Paths are relative,
/// so the router can be nested. The page script comes from a CDN pinned
/// with SRI, so `/docs` needs internet access in the browser.
pub fn docs_routes<S: Clone + Send + Sync + 'static>(
    spec: &utoipa::openapi::OpenApi,
    title: &'static str,
) -> Router<S> {
    let spec_json = spec.to_json().unwrap_or_default();
    let html = format!(
        r#"<!doctype html>
<html><head><title>{title}</title><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/></head>
<body><script id="api-reference" data-url="openapi.json"></script>
<script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.25.0" integrity="sha384-VPR3QD7bdyIR9EHf6dRrFXz7NO0yJV7/UCVTLQ6e7a0wgzot5WaxvciVrrYOKEpU" crossorigin="anonymous"></script></body></html>"#
    );
    Router::new()
        .route("/openapi.json", get(move || async move { ([(header::CONTENT_TYPE, "application/json")], spec_json) }))
        .route("/docs", get(move || async move { Html(html) }))
}

/// A `/readyz` answer: `200 ready`, or `503` with the reason.
pub fn readiness(state: Result<(), String>) -> Response {
    match state {
        Ok(()) => (StatusCode::OK, "ready").into_response(),
        Err(why) => (StatusCode::SERVICE_UNAVAILABLE, why).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, middleware};
    use tower::ServiceExt;

    use super::*;
    use crate::auth::AuthPolicy;

    async fn call(app: Router, req: axum::http::Request<Body>) -> (StatusCode, String) {
        let r = app.oneshot(req).await.unwrap();
        let status = r.status();
        let body = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn get(uri: &str) -> axum::http::Request<Body> {
        axum::http::Request::get(uri).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn bearer_and_error_body() {
        let tokens = Tokens::new(["t"], AuthPolicy::Required);
        #[derive(serde::Deserialize)]
        struct P {
            #[allow(dead_code)]
            n: u32,
        }
        let app = Router::new()
            .route("/x", axum::routing::get(|Q(_): Q<P>| async { "ok" }))
            .layer(middleware::from_fn_with_state(tokens, require_bearer));
        let (s, body) = call(app.clone(), get("/x?n=1")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let e: ApiError = serde_json::from_str(&body).unwrap();
        assert_eq!(e.error, "unauthorized");
        let authed = |uri: &str| axum::http::Request::get(uri).header("authorization", "Bearer t").body(Body::empty());
        assert_eq!(call(app.clone(), authed("/x?n=1").unwrap()).await, (StatusCode::OK, "ok".into()));
        let (s, body) = call(app, authed("/x?n=x").unwrap()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(serde_json::from_str::<ApiError>(&body).unwrap().error, "bad_request");
    }

    #[tokio::test]
    async fn cors_preflight_is_answered() {
        let app = Router::new().route("/v1/traces", axum::routing::post(|| async { "" })).layer(ingest_cors(&[], &[]));
        let req = axum::http::Request::options("/v1/traces")
            .header("origin", "https://app.example")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .unwrap();
        let r = app.oneshot(req).await.unwrap();
        assert!(r.status().is_success());
        assert_eq!(r.headers()["access-control-allow-origin"], "https://app.example");
    }

    #[tokio::test]
    async fn docs_and_readiness() {
        let spec = utoipa::openapi::OpenApiBuilder::new().build();
        let app: Router = docs_routes(&spec, "x API");
        assert_eq!(call(app.clone(), get("/openapi.json")).await.0, StatusCode::OK);
        assert!(call(app, get("/docs")).await.1.contains("<title>x API</title>"));
        assert_eq!(readiness(Err("down".into())).status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
