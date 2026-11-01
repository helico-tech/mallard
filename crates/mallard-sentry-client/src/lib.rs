//! Typed async client for the mallard errors API (`/v1`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), mallard_sentry_client::Error> {
//! let c = mallard_sentry_client::Client::new("http://localhost:8000", Some("token"))?;
//! let issues = c.issues("web", &Default::default()).await?;
//! for i in issues.issues { println!("{} {}", i.id, i.title); }
//! # Ok(()) }
//! ```

pub use mallard_sentry_api as types;
use mallard_sentry_api::*;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// The same variants and helpers as `mallard_otel_client::Error` (plus
/// [`Error::InvalidSegment`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid base url: {0}")]
    InvalidUrl(String),
    /// An id or slug that can't be a path segment (empty, `.` or `..`).
    #[error("invalid path segment {0:?}")]
    InvalidSegment(String),
    /// Connection, timeout or decoding error; the cause is in `source()`.
    #[error("request failed")]
    Http(#[from] reqwest::Error),
    /// The server answered with an error status. `code` is the machine
    /// readable code when the body has one (`{"error": code, "message": …}`),
    /// else empty.
    #[error("{status}: {message}")]
    Api { status: u16, code: String, message: String },
}

impl Error {
    /// HTTP status for API errors.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            Error::Http(e) => e.status().map(|s| s.as_u16()),
            _ => None,
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
}

/// Everything except RFC 3986 unreserved characters is encoded in a path
/// segment (so `/`, `?`, `#`, `%` and spaces can't change the path).
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Requests time out after this, except [`Client::maintenance`] (the same
/// in `mallard-otel-client`).
const TIMEOUT: Duration = Duration::from_secs(60);
/// Flush + compact + expire can take long on a large lake.
const MAINTENANCE_TIMEOUT: Duration = Duration::from_secs(600);
/// Longest error body (in chars) kept as the message of [`Error::Api`].
const MAX_ERROR_BODY: usize = 200;

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
    /// No or an empty `token`: no `Authorization` header is sent.
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self, Error> {
        let parsed = url::Url::parse(base_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::InvalidUrl(format!("scheme must be http or https, not {:?}", parsed.scheme())));
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("mallard-sentry-client/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()?;
        let token = token.filter(|t| !t.is_empty()).map(str::to_string);
        Ok(Client { http, base: base_url.trim_end_matches('/').to_string(), token })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Send with the token; a non-success status becomes [`Error::Api`].
    async fn checked(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, Error> {
        let req = match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        };
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        Ok(resp)
    }

    async fn send<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> Result<T, Error> {
        Ok(self.checked(req).await?.json().await?)
    }

    /// `Error::Api` from a non-success response. Reads the shared error body
    /// `{"error": code, "message": text}` and the older `{"error": text}`.
    /// Else the message is the start of a plain-text body, else the status
    /// reason (so a proxy's HTML error page becomes "Bad Gateway").
    async fn api_error(resp: reqwest::Response) -> Error {
        let status = resp.status();
        let plain = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/plain"));
        let text = resp.text().await.unwrap_or_default();
        // `{"error": code, "message": text}`; older servers sent only
        // `{"error": text}`.
        #[derive(serde::Deserialize)]
        struct Body {
            error: String,
            message: Option<String>,
        }
        let (code, message) = match serde_json::from_str::<Body>(&text) {
            Ok(Body { error, message: Some(m) }) => (error, m),
            Ok(Body { error, message: None }) => (String::new(), error),
            Err(_) if plain => (String::new(), shorten(text.trim(), MAX_ERROR_BODY)),
            Err(_) => (String::new(), status.canonical_reason().unwrap_or_default().to_string()),
        };
        Error::Api { status: status.as_u16(), code, message }
    }

    async fn get<T: DeserializeOwned, Q: Serialize + ?Sized>(&self, path: &str, q: &Q) -> Result<T, Error> {
        self.send(self.http.get(self.url(path)).query(q)).await
    }

    /// GET with two query structs, e.g. a filter and a [`WindowRange`].
    async fn get2<T: DeserializeOwned, A: Serialize + ?Sized, B: Serialize + ?Sized>(
        &self,
        path: &str,
        a: &A,
        b: &B,
    ) -> Result<T, Error> {
        self.send(self.http.get(self.url(path)).query(a).query(b)).await
    }

    /// [`Client::get2`] plus repeated `tag` parameters (`key:value`), which
    /// the query structs can't carry.
    async fn get_tagged<T: DeserializeOwned, A: Serialize + ?Sized, B: Serialize + ?Sized>(
        &self,
        path: &str,
        a: &A,
        b: &B,
        tags: &[String],
    ) -> Result<T, Error> {
        let tags: Vec<(&str, &str)> = tags.iter().map(|t| ("tag", t.as_str())).collect();
        self.send(self.http.get(self.url(path)).query(a).query(b).query(&tags)).await
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
        self.issues_in(project, q, &WindowRange::default()).await
    }

    pub async fn issue(&self, project: &str, issue_id: &str, q: &WindowQuery) -> Result<IssueDetail, Error> {
        self.get(&format!("/v1/projects/{}/issues/{}", segment(project)?, segment(issue_id)?), q).await
    }

    /// [`Client::issues`] with an absolute window (`from`/`to` win over `since`).
    pub async fn issues_in(&self, project: &str, q: &IssueQuery, w: &WindowRange) -> Result<IssueList, Error> {
        self.get_tagged(&format!("/v1/projects/{}/issues", segment(project)?), q, w, &q.tag).await
    }

    /// [`Client::issue`] with an absolute window.
    pub async fn issue_in(
        &self,
        project: &str,
        issue_id: &str,
        q: &WindowQuery,
        w: &WindowRange,
    ) -> Result<IssueDetail, Error> {
        self.get2(&format!("/v1/projects/{}/issues/{}", segment(project)?, segment(issue_id)?), q, w).await
    }

    /// Set the status with a reason. The answer's `changed` is false when
    /// the issue already had that status.
    pub async fn update_issue(&self, project: &str, issue_id: &str, req: &UpdateIssue) -> Result<IssueDetail, Error> {
        let url = self.url(&format!("/v1/projects/{}/issues/{}", segment(project)?, segment(issue_id)?));
        self.send(self.http.patch(url).json(req)).await
    }

    pub async fn set_issue_status(
        &self,
        project: &str,
        issue_id: &str,
        status: IssueStatus,
    ) -> Result<IssueDetail, Error> {
        let url = self.url(&format!("/v1/projects/{}/issues/{}", segment(project)?, segment(issue_id)?));
        self.send(self.http.patch(url).json(&UpdateIssue { status, reason: None })).await
    }

    /// Newest events first; set `q.issue` to restrict to one issue.
    pub async fn events(&self, project: &str, q: &EventQuery) -> Result<EventList, Error> {
        self.events_in(project, q, &WindowRange::default()).await
    }

    /// [`Client::events`] with an absolute window.
    pub async fn events_in(&self, project: &str, q: &EventQuery, w: &WindowRange) -> Result<EventList, Error> {
        self.get_tagged(&format!("/v1/projects/{}/events", segment(project)?), q, w, &q.tag).await
    }

    /// Events of one trace across all projects (last 30 days by default).
    pub async fn events_by_trace(&self, trace_id: &str) -> Result<EventList, Error> {
        self.events_by_trace_query(&TraceQuery { trace_id: trace_id.to_string(), ..Default::default() }).await
    }

    pub async fn events_by_trace_query(&self, q: &TraceQuery) -> Result<EventList, Error> {
        self.get("/v1/events", q).await
    }

    /// [`Client::events_by_trace_query`] with an absolute window.
    pub async fn events_by_trace_in(&self, q: &TraceQuery, w: &WindowRange) -> Result<EventList, Error> {
        self.get2("/v1/events", q, w).await
    }

    /// Error events of many traces (all projects): per trace id the count and
    /// the newest events. One request per [`MAX_TRACES_PER_LOOKUP`] ids;
    /// no ids, no request.
    pub async fn errors_by_trace(&self, trace_ids: &[String], w: &WindowRange) -> Result<EventsByTrace, Error> {
        let mut out = EventsByTrace::default();
        for ids in trace_ids.chunks(MAX_TRACES_PER_LOOKUP) {
            let q = TracesQuery { trace_id: ids.join(","), since: None };
            let part: EventsByTrace = self.get2("/v1/events/by-trace", &q, w).await?;
            out.traces.extend(part.traces);
        }
        Ok(out)
    }

    /// One event by id in any project.
    pub async fn find_event(&self, event_id: &str) -> Result<EventDetail, Error> {
        self.get(&format!("/v1/events/{}", segment(event_id)?), &()).await
    }

    /// Pass the issue id when known: it makes the lookup much cheaper.
    pub async fn event(&self, project: &str, event_id: &str, issue_id: Option<&str>) -> Result<EventDetail, Error> {
        let q: Vec<(&str, &str)> = issue_id.map(|i| vec![("issue", i)]).unwrap_or_default();
        self.get(&format!("/v1/projects/{}/events/{}", segment(project)?, segment(event_id)?), &q).await
    }

    pub async fn stats(&self, project: &str, q: &WindowQuery) -> Result<ProjectStats, Error> {
        self.get(&format!("/v1/projects/{}/stats", segment(project)?), q).await
    }

    /// [`Client::stats`] with an absolute window.
    pub async fn stats_in(&self, project: &str, q: &WindowQuery, w: &WindowRange) -> Result<ProjectStats, Error> {
        self.get2(&format!("/v1/projects/{}/stats", segment(project)?), q, w).await
    }

    /// Releases named by the project's events in the window.
    pub async fn releases(&self, project: &str, q: &ReleaseQuery, w: &WindowRange) -> Result<ReleaseList, Error> {
        self.get2(&format!("/v1/projects/{}/releases", segment(project)?), q, w).await
    }

    /// Tag keys with counts, or the top values of `q.key`, for a filter bar.
    pub async fn tags(&self, project: &str, q: &TagQuery) -> Result<TagStats, Error> {
        self.get(&format!("/v1/projects/{}/tags", segment(project)?), q).await
    }

    /// Read-only SQL over the error tables (the server must enable it).
    pub async fn sql(&self, query: &str, limit: Option<u32>) -> Result<SqlResponse, Error> {
        let req = SqlRequest { query: query.to_string(), limit };
        self.send(self.http.post(self.url("/v1/sql")).json(&req)).await
    }

    /// The tables [`Client::sql`] can query (works when SQL is off).
    pub async fn schema(&self) -> Result<Schema, Error> {
        self.get("/v1/schema", &()).await
    }

    pub async fn sourcemaps(&self, project: &str) -> Result<Vec<ArtifactBundle>, Error> {
        self.get(&format!("/v1/projects/{}/sourcemaps", segment(project)?), &()).await
    }

    /// Flush, compact and expire now. Waits up to 10 minutes.
    pub async fn maintenance(&self) -> Result<(), Error> {
        self.checked(self.http.post(self.url("/v1/maintenance")).timeout(MAINTENANCE_TIMEOUT)).await?;
        Ok(())
    }
}

/// The first `max` chars of `s`, with "…" when cut.
fn shorten(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
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
        let c = Client::new("http://127.0.0.1:9", Some("s3cret-token")).unwrap();
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

    async fn api_message(status: u16, content_type: &'static str, body: &str) -> (u16, String) {
        let (url, _) = serve_once(status, content_type, body.to_string());
        match Client::new(&url, Some("t")).unwrap().info().await {
            Err(Error::Api { status, message, .. }) => (status, message),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn api_errors_use_the_json_message_or_a_short_body() {
        assert_eq!(
            api_message(404, "application/json", r#"{"error":"project not found"}"#).await,
            (404, "project not found".into())
        );
        let (_, m) = api_message(400, "text/plain; charset=utf-8", &"bad query ".repeat(100)).await;
        assert!(m.starts_with("bad query") && m.ends_with('…') && m.chars().count() == MAX_ERROR_BODY + 1, "{m}");
        let (url, _) = serve_once(400, "application/json", r#"{"error":"bad_request","message":"bad since"}"#.into());
        match Client::new(&url, None).unwrap().info().await {
            Err(Error::Api { status: 400, code, message }) => {
                assert_eq!((code.as_str(), message.as_str()), ("bad_request", "bad since"))
            }
            other => panic!("unexpected {other:?}"),
        }
        let html = format!("<html><body>{}</body></html>", "x".repeat(10_000));
        assert_eq!(api_message(502, "text/html", &html).await, (502, "Bad Gateway".into()));
    }

    #[tokio::test]
    async fn maintenance_reads_errors_from_any_body() {
        let (url, _) = serve_once(500, "application/json", r#"{"error":"disk full"}"#.into());
        let r = Client::new(&url, Some("t")).unwrap().maintenance().await;
        assert!(matches!(r, Err(Error::Api { status: 500, message, .. }) if message == "disk full"));
        let (url, _) = serve_once(503, "text/html", "<h1>down</h1>".into());
        let r = Client::new(&url, Some("t")).unwrap().maintenance().await;
        assert!(matches!(r, Err(Error::Api { status: 503, message, .. }) if message == "Service Unavailable"));
    }

    #[test]
    fn shorten_cuts_on_char_boundaries() {
        assert_eq!(shorten("héllo", 10), "héllo");
        assert_eq!(shorten("héllo", 2), "hé…");
    }

    #[tokio::test]
    async fn empty_token_sends_no_authorization_header() {
        let info =
            r#"{"version":"1","duckdb_version":"v","catalog":"c","storage":"s","queue_depth":0,"queue_capacity":1}"#;
        let (url, head) = serve_once(200, "application/json", info.into());
        Client::new(&url, None).unwrap().info().await.unwrap();
        let head = head.recv().unwrap().to_ascii_lowercase();
        assert!(!head.contains("authorization"), "{head}");

        let (url, head) = serve_once(200, "application/json", info.into());
        Client::new(&url, Some("tok")).unwrap().info().await.unwrap();
        assert!(head.recv().unwrap().to_ascii_lowercase().contains("authorization: bearer tok"));
    }

    #[tokio::test]
    async fn errors_by_trace_sends_one_batch() {
        let (a, b) = ("0af7651916cd43dd8448eb211c80319c".to_string(), "4bf92f3577b34da6a3ce929d0e0e4736".to_string());
        let body = format!(
            r#"{{"traces":{{"{a}":{{"count":3,"events":[{{"event_id":"e1","issue_id":"85fede14250ec6be","project_id":1,"span_id":"00f067aa0ba902b7","title":"boom"}}]}}}}}}"#
        );
        let (url, head) = serve_once(200, "application/json", body);
        let from = "2026-11-01T10:00:00Z".parse().unwrap();
        let w = WindowRange { from: Some(from), to: None, step: None };
        let r = Client::new(&url, None).unwrap().errors_by_trace(&[a.clone(), b.clone()], &w).await.unwrap();
        let head = head.recv().unwrap();
        let line = head.lines().next().unwrap();
        assert!(
            line.starts_with(&format!("GET /v1/events/by-trace?trace_id={a}%2C{b}&from=2026-11-01T10%3A00%3A00Z ")),
            "{line}"
        );
        let t = &r.traces[&a];
        assert_eq!((t.count, t.events[0].span_id.as_deref()), (3, Some("00f067aa0ba902b7")));
        assert!(!r.traces.contains_key(&b), "traces without events are left out");

        // No ids: no request (this client could not reach anything).
        let c = Client::new("http://127.0.0.1:9", None).unwrap();
        assert_eq!(block_on(c.errors_by_trace(&[], &w)).unwrap(), EventsByTrace::default());
    }

    #[tokio::test]
    async fn tags_are_repeated_parameters() {
        let body = r#"{"issues":[],"window_start":"2026-11-01T10:00:00Z","window_end":"2026-11-01T11:00:00Z"}"#;
        let (url, head) = serve_once(200, "application/json", body.into());
        let q = IssueQuery {
            tag: vec!["url:/a?b".into(), "user:u-1".into()],
            cursor: Some("c".into()),
            ..Default::default()
        };
        let list = Client::new(&url, None).unwrap().issues("web", &q).await.unwrap();
        assert_eq!(list.next_cursor, None, "older servers send no cursor");
        let line = head.recv().unwrap().lines().next().unwrap().to_string();
        assert!(line.starts_with("GET /v1/projects/web/issues?cursor=c&tag=url%3A%2Fa%3Fb&tag=user%3Au-1 "), "{line}");

        let body = r#"{"events":[],"next_cursor":null}"#;
        let (url, head) = serve_once(200, "application/json", body.into());
        let q = EventQuery { tag: vec!["k:v".into()], release: Some("1.0".into()), ..Default::default() };
        Client::new(&url, None).unwrap().events("web", &q).await.unwrap();
        let line = head.recv().unwrap().lines().next().unwrap().to_string();
        assert!(line.starts_with("GET /v1/projects/web/events?release=1.0&tag=k%3Av "), "{line}");
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
        let c = Client::new("http://127.0.0.1:9", Some("t")).unwrap();
        // The error is returned before any network I/O, so this resolves on the first poll.
        assert!(matches!(block_on(c.project("..")), Err(Error::InvalidSegment(_))));
        assert!(matches!(block_on(c.issue("web", ".", &Default::default())), Err(Error::InvalidSegment(_))));
        assert!(matches!(Client::new("not a url", Some("t")), Err(Error::InvalidUrl(_))));
        assert!(matches!(Client::new("localhost:8000", None), Err(Error::InvalidUrl(_))), "no http scheme");
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
