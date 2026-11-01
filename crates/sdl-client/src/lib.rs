//! Typed async client for the sentry-ducklake `/v1` API.
//!
//! ```no_run
//! # async fn demo() -> Result<(), sdl_client::Error> {
//! let c = sdl_client::Client::new("http://localhost:8000", "token")?;
//! let issues = c.issues("web", &Default::default()).await?;
//! for i in issues.issues { println!("{} {}", i.id, i.title); }
//! # Ok(()) }
//! ```

pub use sdl_types as types;
use sdl_types::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid base url: {0}")]
    Url(String),
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{status}: {message}")]
    Api { status: u16, message: String },
}

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Api { status: 404, .. })
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// `base_url` may include a path prefix (e.g. `https://host/sentry`).
    pub fn new(base_url: &str, token: &str) -> Result<Self, Error> {
        url::Url::parse(base_url).map_err(|e| Error::Url(e.to_string()))?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("sdl-client/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Client { http, base: base_url.trim_end_matches('/').to_string(), token: token.to_string() })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn send<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> Result<T, Error> {
        let resp = req.bearer_auth(&self.token).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let message = serde_json::from_str::<ApiError>(&text).map(|e| e.error).unwrap_or(text);
            return Err(Error::Api { status: status.as_u16(), message });
        }
        Ok(resp.json().await?)
    }

    async fn get<T: DeserializeOwned, Q: Serialize + ?Sized>(&self, path: &str, q: &Q) -> Result<T, Error> {
        self.send(self.http.get(self.url(path)).query(q)).await
    }

    fn enc(s: &str) -> String {
        url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
    }

    pub async fn info(&self) -> Result<ServerInfo, Error> {
        self.get("/v1/info", &()).await
    }

    pub async fn projects(&self) -> Result<Vec<Project>, Error> {
        self.get("/v1/projects", &()).await
    }

    /// `project` is an id or slug.
    pub async fn project(&self, project: &str) -> Result<Project, Error> {
        self.get(&format!("/v1/projects/{}", Self::enc(project)), &()).await
    }

    pub async fn create_project(&self, req: &CreateProject) -> Result<Project, Error> {
        self.send(self.http.post(self.url("/v1/projects")).json(req)).await
    }

    pub async fn issues(&self, project: &str, q: &IssueQuery) -> Result<IssueList, Error> {
        self.get(&format!("/v1/projects/{}/issues", Self::enc(project)), q).await
    }

    pub async fn issue(&self, project: &str, issue_id: &str, q: &WindowQuery) -> Result<IssueDetail, Error> {
        self.get(&format!("/v1/projects/{}/issues/{}", Self::enc(project), Self::enc(issue_id)), q).await
    }

    pub async fn set_issue_status(&self, project: &str, issue_id: &str, status: IssueStatus) -> Result<IssueDetail, Error> {
        let url = self.url(&format!("/v1/projects/{}/issues/{}", Self::enc(project), Self::enc(issue_id)));
        self.send(self.http.patch(url).json(&UpdateIssue { status })).await
    }

    pub async fn resolve(&self, project: &str, issue_id: &str) -> Result<IssueDetail, Error> {
        self.set_issue_status(project, issue_id, IssueStatus::Resolved).await
    }

    pub async fn ignore(&self, project: &str, issue_id: &str) -> Result<IssueDetail, Error> {
        self.set_issue_status(project, issue_id, IssueStatus::Ignored).await
    }

    pub async fn unresolve(&self, project: &str, issue_id: &str) -> Result<IssueDetail, Error> {
        self.set_issue_status(project, issue_id, IssueStatus::Unresolved).await
    }

    /// Newest events first; set `q.issue` to restrict to one issue.
    pub async fn events(&self, project: &str, q: &EventQuery) -> Result<EventList, Error> {
        self.get(&format!("/v1/projects/{}/events", Self::enc(project)), q).await
    }

    /// Events of one trace across all projects (last 30 days by default).
    pub async fn events_by_trace(&self, trace_id: &str) -> Result<EventList, Error> {
        self.events_by_trace_query(&TraceQuery { trace_id: trace_id.to_string(), ..Default::default() }).await
    }

    pub async fn events_by_trace_query(&self, q: &TraceQuery) -> Result<EventList, Error> {
        self.get("/v1/events", q).await
    }

    /// Pass the issue id when known: it makes the lookup much cheaper.
    pub async fn event(&self, project: &str, event_id: &str, issue_id: Option<&str>) -> Result<EventDetail, Error> {
        let q: Vec<(&str, &str)> = issue_id.map(|i| vec![("issue", i)]).unwrap_or_default();
        self.get(&format!("/v1/projects/{}/events/{}", Self::enc(project), Self::enc(event_id)), &q).await
    }

    pub async fn stats(&self, project: &str, q: &WindowQuery) -> Result<ProjectStats, Error> {
        self.get(&format!("/v1/projects/{}/stats", Self::enc(project)), q).await
    }

    pub async fn sourcemaps(&self, project: &str) -> Result<Vec<ArtifactBundle>, Error> {
        self.get(&format!("/v1/projects/{}/sourcemaps", Self::enc(project)), &()).await
    }

    /// Flush, compact and expire now.
    pub async fn maintenance(&self) -> Result<(), Error> {
        let resp = self.http.post(self.url("/v1/maintenance")).bearer_auth(&self.token).send().await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            Err(Error::Api { status, message: resp.text().await.unwrap_or_default() })
        }
    }
}
