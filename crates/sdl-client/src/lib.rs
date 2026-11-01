//! Typed async client for the sentry-ducklake `/v1` API.
//!
//! ```no_run
//! # async fn demo() -> Result<(), sdl_client::Error> {
//! let c = sdl_client::Client::new("http://localhost:8000", "token")?;
//! let issues = c.issues("web", &Default::default()).await?;
//! for i in issues.issues { println!("{} {}", i.id, i.title); }
//! # Ok(()) }
//! ```

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
pub use sdl_types as types;
use sdl_types::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid base url: {0}")]
    Url(String),
    /// An id or slug that can't be a path segment (empty, `.` or `..`).
    #[error("invalid path segment {0:?}")]
    InvalidSegment(String),
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

/// Everything except RFC 3986 unreserved characters is encoded in a path
/// segment (so `/`, `?`, `#`, `%` and spaces can't change the path).
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Encode one path segment. Empty, `.` and `..` segments are rejected:
/// URL parsers resolve them (even percent-encoded) and the request would
/// go to another path.
fn segment(s: &str) -> Result<String, Error> {
    if s.is_empty() || s.chars().all(|c| c == '.') {
        return Err(Error::InvalidSegment(s.to_string()));
    }
    Ok(utf8_percent_encode(s, SEGMENT).to_string())
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

/// The token is never printed.
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// `base_url` may include a path prefix (e.g. `https://host/sentry`).
    /// An empty `token` means no token: no `Authorization` header is sent.
    pub fn new(base_url: &str, token: &str) -> Result<Self, Error> {
        url::Url::parse(base_url).map_err(|e| Error::Url(e.to_string()))?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("sdl-client/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let token = (!token.is_empty()).then(|| token.to_string());
        Ok(Client { http, base: base_url.trim_end_matches('/').to_string(), token })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    async fn send<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> Result<T, Error> {
        let resp = self.auth(req).send().await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        Ok(resp.json().await?)
    }

    /// `Error::Api` from a non-success response; the message is the
    /// `ApiError.error` field when the body is one, else the raw body.
    async fn api_error(resp: reqwest::Response) -> Error {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<ApiError>(&text).map(|e| e.error).unwrap_or(text);
        Error::Api { status, message }
    }

    async fn get<T: DeserializeOwned, Q: Serialize + ?Sized>(&self, path: &str, q: &Q) -> Result<T, Error> {
        self.send(self.http.get(self.url(path)).query(q)).await
    }

    pub async fn info(&self) -> Result<ServerInfo, Error> {
        self.get("/v1/info", &()).await
    }

    pub async fn projects(&self) -> Result<Vec<Project>, Error> {
        self.get("/v1/projects", &()).await
    }

    /// `project` is an id or slug.
    pub async fn project(&self, project: &str) -> Result<Project, Error> {
        self.get(&format!("/v1/projects/{}", segment(project)?), &()).await
    }

    pub async fn create_project(&self, req: &CreateProject) -> Result<Project, Error> {
        self.send(self.http.post(self.url("/v1/projects")).json(req)).await
    }

    pub async fn issues(&self, project: &str, q: &IssueQuery) -> Result<IssueList, Error> {
        self.get(&format!("/v1/projects/{}/issues", segment(project)?), q).await
    }

    pub async fn issue(&self, project: &str, issue_id: &str, q: &WindowQuery) -> Result<IssueDetail, Error> {
        self.get(&format!("/v1/projects/{}/issues/{}", segment(project)?, segment(issue_id)?), q).await
    }

    pub async fn set_issue_status(
        &self,
        project: &str,
        issue_id: &str,
        status: IssueStatus,
    ) -> Result<IssueDetail, Error> {
        let url = self.url(&format!("/v1/projects/{}/issues/{}", segment(project)?, segment(issue_id)?));
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
        self.get(&format!("/v1/projects/{}/events", segment(project)?), q).await
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
        self.get(&format!("/v1/projects/{}/events/{}", segment(project)?, segment(event_id)?), &q).await
    }

    pub async fn stats(&self, project: &str, q: &WindowQuery) -> Result<ProjectStats, Error> {
        self.get(&format!("/v1/projects/{}/stats", segment(project)?), q).await
    }

    pub async fn sourcemaps(&self, project: &str) -> Result<Vec<ArtifactBundle>, Error> {
        self.get(&format!("/v1/projects/{}/sourcemaps", segment(project)?), &()).await
    }

    /// Flush, compact and expire now.
    pub async fn maintenance(&self) -> Result<(), Error> {
        let resp = self.auth(self.http.post(self.url("/v1/maintenance"))).send().await?;
        if resp.status().is_success() { Ok(()) } else { Err(Self::api_error(resp).await) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn debug_never_shows_the_token() {
        let c = Client::new("http://127.0.0.1:9", "s3cret-token").unwrap();
        let s = format!("{c:?}");
        assert!(!s.contains("s3cret") && s.contains("<redacted>"), "{s}");
    }

    /// Answer one request with `status`, `content_type` and `body`; the
    /// request head (request line + headers) comes back on the channel.
    fn serve_once(status: u16, content_type: &'static str, body: String) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut head, mut len) = (String::new(), 0usize);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
            }
            let _ = reader.read_exact(&mut vec![0; len]);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tx.send(head);
        });
        (url, rx)
    }

    #[tokio::test]
    async fn empty_token_sends_no_authorization_header() {
        let info = r#"{"version":"1","duckdb_version":"v","catalog":"c","storage":"s","queue_depth":0,"queue_capacity":1}"#;
        let (url, head) = serve_once(200, "application/json", info.into());
        Client::new(&url, "").unwrap().info().await.unwrap();
        let head = head.recv().unwrap().to_ascii_lowercase();
        assert!(!head.contains("authorization"), "{head}");

        let (url, head) = serve_once(200, "application/json", info.into());
        Client::new(&url, "tok").unwrap().info().await.unwrap();
        assert!(head.recv().unwrap().to_ascii_lowercase().contains("authorization: bearer tok"));
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(segment("web-app").unwrap(), "web-app");
        assert_eq!(segment("a b").unwrap(), "a%20b");
        assert_eq!(segment("a/b?c#d").unwrap(), "a%2Fb%3Fc%23d");
        assert_eq!(segment("100%+x").unwrap(), "100%25%2Bx");
        assert_eq!(segment("v1.2").unwrap(), "v1.2");
        assert_eq!(segment("é").unwrap(), "%C3%A9");
        for bad in ["", ".", "..", "..."] {
            assert!(matches!(segment(bad), Err(Error::InvalidSegment(s)) if s == bad), "{bad:?}");
        }
    }

    #[test]
    fn dot_segments_never_reach_the_server() {
        let c = Client::new("http://127.0.0.1:9", "t").unwrap();
        // The error is returned before any network I/O, so this resolves on the first poll.
        assert!(matches!(block_on(c.project("..")), Err(Error::InvalidSegment(_))));
        assert!(matches!(block_on(c.issue("web", ".", &Default::default())), Err(Error::InvalidSegment(_))));
        assert!(matches!(Client::new("not a url", "t"), Err(Error::Url(_))));
    }

    /// Minimal executor for futures that resolve without I/O.
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }
}
