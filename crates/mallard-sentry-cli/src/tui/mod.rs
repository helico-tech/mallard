//! The errors tab of `mallard tui`.
//!
//! [`Embedded`] is the whole TUI as a component without terminal setup, so
//! a host program (e.g. mallard) can draw it inside its own screen and
//! event loop.

mod app;
#[cfg(test)]
mod fixtures;
mod theme;
mod ui;

use crate::cli::describe;
use app::{App, FetchError, Kind, Req, Resp, Window};
use crossterm::event::{Event, KeyEventKind};
use futures::FutureExt;
use mallard_sentry_client::Client;
use mallard_sentry_client::types::{EventQuery, TagQuery, WindowQuery};
use mallard_term::error::Hints;
use mallard_term::tui::Scope;
use mallard_term::tui::suggest::{FETCH_TIMEOUT, LIMIT};
use ratatui::Frame;
use ratatui::layout::Rect;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Instant;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

/// What the host should do after [`Embedded::handle_event`]; `x` on an
/// event with a trace id is [`Outcome::Jump`].
pub use mallard_term::tui::Outcome;

/// The TUI as an embeddable component. Data loads run as tasks on the tokio
/// runtime that was current when [`Embedded::new`] was called; their results
/// are picked up in [`Embedded::tick`]. Dropping it cancels the loads still
/// running.
pub struct Embedded {
    app: App,
    client: Client,
    handle: Handle,
    tx: mpsc::UnboundedSender<Resp>,
    rx: mpsc::UnboundedReceiver<Resp>,
    started: bool,
    /// Loads that may still be running.
    tasks: Vec<AbortHandle>,
    /// The search bar's fetch: a newer one replaces it.
    suggest_task: Option<AbortHandle>,
}

impl Drop for Embedded {
    fn drop(&mut self) {
        for t in self.tasks.iter().chain(&self.suggest_task) {
            t.abort();
        }
    }
}

impl Embedded {
    /// Must be called inside a tokio runtime context (e.g. from async code
    /// or under `Runtime::enter`); panics otherwise. The other methods can
    /// then be called from any thread.
    pub fn new(client: Client, project: Option<String>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = Handle::current();
        Embedded {
            app: App::new(client.base_url(), project),
            client,
            handle,
            tx,
            rx,
            started: false,
            tasks: vec![],
            suggest_task: None,
        }
    }

    /// Name the host's command and env vars in hints (default [`crate::HINTS`]).
    pub fn with_hints(mut self, hints: Hints) -> Self {
        self.app.hints = hints;
        self
    }

    /// The host's one window for all tabs. A window the errors side does not know
    /// changes nothing.
    pub fn set_window(&mut self, since: &str) {
        let Some(w) = Window::parse(since) else { return };
        let reqs = self.app.set_window(w);
        self.send_when_started(reqs);
    }

    /// The host's app.
    pub fn set_scope(&mut self, scope: &Scope) {
        let reqs = self.app.set_scope(scope);
        self.send_when_started(reqs);
    }

    fn send_when_started(&mut self, reqs: Vec<Req>) {
        // Before the start there is nothing to reload: the start loads what is set now.
        if self.started {
            let reqs = self.app.track(reqs);
            self.dispatch(reqs);
        }
    }

    /// The project shown: the host's choice, or one this tab went to for an event.
    pub fn project(&self) -> Option<&str> {
        self.app.project.as_deref()
    }

    /// The server's longest window, once it has said so.
    pub fn max_window_secs(&self) -> Option<u64> {
        self.app.info.as_ref()?.max_window_secs
    }

    /// Draw inside `area` only.
    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        ui::render(f, area, &mut self.app);
    }

    pub fn handle_event(&mut self, ev: &Event) -> Outcome {
        self.ensure_started();
        let Event::Key(k) = ev else {
            return Outcome::Ignored;
        };
        if k.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        let (reqs, outcome) = self.app.on_key(*k);
        self.dispatch(reqs);
        outcome
    }

    /// Call a few times per second. Never blocks: applies finished loads and
    /// starts the periodic refresh.
    pub fn tick(&mut self) {
        self.ensure_started();
        while let Ok(resp) = self.rx.try_recv() {
            let reqs = self.app.on_response(resp);
            self.dispatch(reqs);
        }
        let reqs = self.app.on_tick(Instant::now());
        self.dispatch(reqs);
    }

    /// A background load panicked: the message, once. The panic hook has
    /// already run (a ratatui hook restores the terminal), so the host
    /// should stop drawing, restore the terminal and exit with this error.
    /// Check it after [`Embedded::tick`].
    pub fn take_fatal(&mut self) -> Option<String> {
        self.app.fatal.take()
    }

    /// Show the error events of this trace (all projects). The other side's
    /// `Embedded::open_trace` has the same name.
    pub fn open_trace(&mut self, trace_id: &str) {
        self.ensure_started();
        let reqs = self.app.show_trace(trace_id);
        self.dispatch(reqs);
    }

    /// [`Embedded::open_trace`], then open the event `event_id` of that
    /// trace (its issue, on the stack trace) once the list has loaded.
    pub fn open_trace_event(&mut self, trace_id: &str, event_id: &str) {
        self.ensure_started();
        let reqs = self.app.show_trace_event(trace_id, event_id);
        self.dispatch(reqs);
    }

    /// Open an issue of a project by id; with an event, that event on the stack trace.
    pub fn open_issue(&mut self, project: &str, issue: &str, event: Option<&str>) {
        self.ensure_started();
        let reqs = self.app.show_issue(project, issue, event);
        self.dispatch(reqs);
    }

    fn ensure_started(&mut self) {
        if !self.started {
            self.started = true;
            let reqs = self.app.start();
            self.dispatch(reqs);
        }
    }

    fn dispatch(&mut self, reqs: Vec<Req>) {
        self.tasks.retain(|t| !t.is_finished());
        for r in reqs {
            let kind = r.kind();
            let task = self.handle.spawn(guarded(kind, fetch(self.client.clone(), r, self.app.hints), self.tx.clone()));
            if kind == Kind::Suggest {
                if let Some(old) = self.suggest_task.replace(task.abort_handle()) {
                    old.abort();
                }
            } else {
                self.tasks.push(task.abort_handle());
            }
        }
    }
}

/// Run a load and send its response; a panic inside becomes `Resp::Panicked`
/// instead of a silently lost task.
async fn guarded(kind: Kind, fut: impl Future<Output = Resp>, tx: mpsc::UnboundedSender<Resp>) {
    let resp = match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "background task panicked".into());
            Resp::Panicked { kind, msg }
        }
    };
    let _ = tx.send(resp);
}

fn fetch_error(err: mallard_sentry_client::Error, url: &str, hints: &Hints) -> FetchError {
    // Network-level failures mean "offline"; a body we can't decode is a
    // version mismatch, not an outage.
    let offline = matches!(&err, mallard_sentry_client::Error::Http(h) if !h.is_decode());
    FetchError { message: describe(&err, url, hints), offline }
}

async fn fetch(c: Client, req: Req, hints: Hints) -> Resp {
    let url = c.base_url().to_string();
    let e = |err: mallard_sentry_client::Error| fetch_error(err, &url, &hints);
    match req {
        Req::Info => Resp::Info(c.info().await.map_err(e)),
        Req::Projects => Resp::Projects(c.projects().await.map_err(e)),
        Req::Issues(key) => {
            let r = c.issues(&key.project, &key.query()).await.map_err(e);
            Resp::Issues(key, r)
        }
        Req::Stats { project, window } => {
            let q = WindowQuery { since: Some(window.since().into()), interval: Some(window.interval().into()) };
            let result = c.stats(&project, &q).await.map_err(e);
            Resp::Stats { project, window, result }
        }
        Req::Issue { project, id, window } => {
            let q = WindowQuery { since: Some(window.since().into()), interval: None };
            let result = c.issue(&project, &id, &q).await.map_err(e);
            Resp::Issue { id, result }
        }
        Req::Events { project, issue, before } => {
            let q = EventQuery {
                issue: Some(issue.clone()),
                limit: Some(50),
                before: before.clone(),
                ..Default::default()
            };
            let result = c.events(&project, &q).await.map_err(e);
            Resp::Events { issue, before, result }
        }
        Req::Event { project, issue, event_id } => {
            let result = c.event(&project, &event_id, Some(&issue)).await.map_err(e);
            Resp::Event { event_id, result }
        }
        Req::SetStatus { project, id, status } => {
            let result = c.set_issue_status(&project, &id, status).await.map_err(e);
            Resp::SetStatus { id, status, result }
        }
        Req::Trace { trace_id } => {
            let result = c.events_by_trace(&trace_id).await.map_err(e);
            Resp::Trace { trace_id, result }
        }
        Req::Suggest { project, window, scope, want } => {
            let q = TagQuery {
                since: Some(window.since().into()),
                key: want.key.clone(),
                prefix: (!want.prefix.is_empty()).then(|| want.prefix.clone()),
                limit: Some(LIMIT),
            };
            let answer = tokio::time::timeout(FETCH_TIMEOUT, c.tags(&project, &q)).await;
            let items = answer
                .ok()
                .and_then(Result::ok)
                .map(|s| s.items.into_iter().map(|i| (i.value.unwrap_or(i.key), i.count)).collect());
            Resp::Suggest { scope, want, items }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::screen;
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    fn key(c: KeyCode) -> Event {
        Event::Key(KeyEvent::from(c))
    }

    /// Drives `Embedded` through draw / handle_event / tick. No server runs
    /// on the port, so loads fail; the UI must stay responsive and show the
    /// error instead of hanging.
    #[tokio::test]
    async fn embedded_draw_handle_tick() {
        let client = Client::new("http://127.0.0.1:9", Some("t")).unwrap();
        let mut ui = Embedded::new(client, Some("web".into()));
        let mut term = Terminal::new(TestBackend::new(120, 36)).unwrap();
        term.draw(|f| ui.draw(f, Rect { x: 0, y: 1, width: 120, height: 34 })).unwrap();
        assert!(screen(&term).contains("loading issues"), "{}", screen(&term));

        assert_eq!(ui.handle_event(&key(KeyCode::F(1))), Outcome::Ignored);
        assert_eq!(ui.handle_event(&key(KeyCode::Char('z'))), Outcome::Ignored);
        assert_eq!(ui.handle_event(&key(KeyCode::Char('s'))), Outcome::Consumed);
        assert_eq!(ui.handle_event(&key(KeyCode::Char('?'))), Outcome::Consumed);
        term.draw(|f| ui.draw(f, f.area())).unwrap();
        assert!(screen(&term).contains("Keys"));
        assert_eq!(ui.handle_event(&key(KeyCode::Esc)), Outcome::Consumed);

        // Failed loads arrive via tick and show up as an error.
        for _ in 0..50 {
            ui.tick();
            if ui.app.offline.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        term.draw(|f| ui.draw(f, f.area())).unwrap();
        let s = screen(&term);
        assert!(s.contains("cannot reach server"), "{s}");

        ui.open_trace("b7644661850f40e69380d8c9ea253e84");
        term.draw(|f| ui.draw(f, f.area())).unwrap();
        assert!(screen(&term).contains("Trace"));
        assert_eq!(ui.handle_event(&key(KeyCode::Esc)), Outcome::Consumed);
        assert_eq!(ui.handle_event(&key(KeyCode::Char('q'))), Outcome::Quit);
    }

    /// `new` captures the runtime handle, so the host may call the other
    /// methods from a thread that is not inside the runtime context.
    #[test]
    fn embedded_works_outside_the_runtime_context() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let mut ui = {
            let _guard = rt.enter();
            Embedded::new(Client::new("http://127.0.0.1:9", Some("t")).unwrap(), None)
        };
        // Not in a runtime context here: these spawn loads and must not panic.
        assert_eq!(ui.handle_event(&key(KeyCode::Char('r'))), Outcome::Consumed);
        ui.tick();
        ui.open_trace("b7644661850f40e69380d8c9ea253e84");
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| ui.draw(f, f.area())).unwrap();
    }

    /// The host learns about a panicked load through `take_fatal`.
    #[tokio::test]
    async fn a_panicked_load_reaches_the_host() {
        let mut ui = Embedded::new(Client::new("http://127.0.0.1:9", Some("t")).unwrap(), Some("web".into()));
        assert_eq!(ui.take_fatal(), None);
        ui.tx.send(Resp::Panicked { kind: Kind::Info, msg: "boom".into() }).unwrap();
        ui.tick();
        assert_eq!(ui.take_fatal().as_deref(), Some("boom"));
        assert_eq!(ui.take_fatal(), None, "reported once");
    }

    /// Loads still running when the component is dropped are cancelled.
    #[tokio::test]
    async fn dropping_cancels_running_loads() {
        // Accepts connections but never answers, so the loads hang.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut ui = Embedded::new(Client::new(&url, Some("t")).unwrap(), Some("web".into()));
        ui.tick();
        let tasks = ui.tasks.clone();
        assert!(!tasks.is_empty() && tasks.iter().all(|t| !t.is_finished()));
        drop(ui);
        tokio::task::yield_now().await;
        for _ in 0..50 {
            if tasks.iter().all(AbortHandle::is_finished) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("loads still running after drop");
    }

    /// The server never answers: keys are handled at once and no text is lost.
    #[tokio::test]
    async fn typing_never_waits_on_a_server_that_never_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut ui = Embedded::new(Client::new(&url, Some("t")).unwrap(), Some("web".into()));
        ui.tick();
        let started = Instant::now();
        assert_eq!(ui.handle_event(&key(KeyCode::Char('/'))), Outcome::Consumed);
        for c in "url=".chars() {
            assert_eq!(ui.handle_event(&key(KeyCode::Char(c))), Outcome::Consumed);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        ui.tick();
        let first = ui.suggest_task.clone().expect("a fetch is running");
        assert!(!first.is_finished(), "and it does not end");
        for c in "/check".chars() {
            assert_eq!(ui.handle_event(&key(KeyCode::Char(c))), Outcome::Consumed);
        }
        ui.tick();
        assert_eq!(ui.app.input.as_deref(), Some("url=/check"));
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        let mut term = Terminal::new(TestBackend::new(120, 36)).unwrap();
        term.draw(|f| ui.draw(f, f.area())).unwrap();
        assert!(screen(&term).contains("url=/check"), "{}", screen(&term));
        assert_eq!(ui.handle_event(&key(KeyCode::Enter)), Outcome::Consumed);
        assert_eq!(ui.app.query, "url=/check", "applied without an answer");
    }

    /// A server with one project, one tag value holding U+202E and no issues. Hands back
    /// what it was asked, with the query strings as they came in.
    async fn server_with_a_hostile_tag() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use mallard_sentry_client::types::{IssueList, TagCount, TagStats};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let hits = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let hits = hits.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16384];
                    let mut n = 0;
                    while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf[n..]).await {
                            Ok(0) | Err(_) => return,
                            Ok(k) => n += k,
                        }
                    }
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let full = head.split_whitespace().nth(1).unwrap_or("").to_string();
                    hits.lock().unwrap().push(full.clone());
                    let at = crate::tui::fixtures::now();
                    let body = match full.split('?').next().unwrap() {
                        "/v1/projects" => serde_json::to_string(&crate::tui::fixtures::projects()).unwrap(),
                        p if p.ends_with("/tags") => serde_json::to_string(&TagStats {
                            total_rows: 1,
                            sampled_rows: 1,
                            counted_from: at,
                            items: vec![TagCount { key: "user".into(), value: Some("a\u{202e}b".into()), count: 1 }],
                        })
                        .unwrap(),
                        p if p.ends_with("/issues") => serde_json::to_string(&IssueList {
                            issues: vec![],
                            window_start: at,
                            window_end: at,
                            next_cursor: None,
                        })
                        .unwrap(),
                        _ => "{}".to_string(),
                    };
                    let status = if body == "{}" { "503 X" } else { "200 OK" };
                    let answer = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(answer.as_bytes()).await;
                });
            }
        });
        (url, seen)
    }

    /// Issue 0148. Accepting a suggestion puts the value in the bar as the server has it:
    /// the frame only ever shows it cleaned, in the list, the bar, the filter row and the
    /// empty list, and the request holds the real value, or it would filter on nothing.
    #[tokio::test]
    async fn an_accepted_suggestion_shows_cleaned_and_sends_the_real_value() {
        let (url, seen) = server_with_a_hostile_tag().await;
        let mut ui = Embedded::new(Client::new(&url, Some("t")).unwrap(), Some("web".into()));
        let mut term = Terminal::new(TestBackend::new(120, 36)).unwrap();
        let mut frame = |ui: &mut Embedded, says: &str| {
            term.draw(|f| ui.draw(f, f.area())).unwrap();
            let s = screen(&term);
            assert!(!s.contains('\u{202e}'), "raw text on the terminal: {s}");
            assert!(s.contains(says), "no {says:?} in\n{s}");
        };
        const SHOWN: &str = "a\\u{202e}b";

        ui.tick();
        assert_eq!(ui.handle_event(&key(KeyCode::Char('/'))), Outcome::Consumed);
        for c in "user=".chars() {
            assert_eq!(ui.handle_event(&key(KeyCode::Char(c))), Outcome::Consumed);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        for _ in 0..200 {
            ui.tick();
            if ui.app.suggest.has_items() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ui.app.suggest.has_items(), "no suggestion: {:?}", seen.lock().unwrap());
        frame(&mut ui, SHOWN);

        assert_eq!(ui.handle_event(&key(KeyCode::Tab)), Outcome::Consumed);
        assert_eq!(ui.app.input.as_deref(), Some("user=a\u{202e}b "), "raw in the bar");
        frame(&mut ui, &format!("user={SHOWN}"));

        assert_eq!(ui.handle_event(&key(KeyCode::Enter)), Outcome::Consumed);
        assert_eq!(ui.app.query, "user=a\u{202e}b");
        let sent = |seen: &std::sync::Mutex<Vec<String>>| {
            seen.lock().unwrap().iter().any(|q| q.contains("/issues?") && q.contains("tag=user%3Aa%E2%80%AEb"))
        };
        for _ in 0..200 {
            ui.tick();
            if sent(&seen) && ui.app.issues_loaded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sent(&seen), "{:?}", seen.lock().unwrap());
        assert!(ui.app.issues_loaded);
        frame(&mut ui, &format!("matching \"user={SHOWN}\""));
        frame(&mut ui, &format!("/ user={SHOWN}"));

        // Opened again it holds the same raw text, drawn cleaned.
        assert_eq!(ui.handle_event(&key(KeyCode::Char('/'))), Outcome::Consumed);
        assert_eq!(ui.app.input.as_deref(), Some("user=a\u{202e}b"));
        frame(&mut ui, &format!(" user={SHOWN}"));
    }

    /// In a host, the tab asks for an app, or says the app has no project.
    #[tokio::test]
    async fn an_empty_scope_says_what_to_do() {
        let client = Client::new("http://127.0.0.1:9", Some("t")).unwrap();
        let mut ui = Embedded::new(client, None);
        ui.set_window("1h");
        assert_eq!(ui.app.window, Window::Hour, "before the start: only the field");
        let mut term = Terminal::new(TestBackend::new(120, 36)).unwrap();
        term.draw(|f| ui.draw(f, f.area())).unwrap();
        let s = screen(&term);
        assert!(s.contains("Pick an app to see its errors") && s.contains("press p to choose"), "{s}");
        assert!(!s.contains("loading"), "nothing is loading: {s}");
        ui.set_scope(&Scope::App { slug: "work\u{202e}er".into(), project: None, services: vec!["w".into()] });
        term.draw(|f| ui.draw(f, f.area())).unwrap();
        let s = screen(&term);
        assert!(s.contains("has no project, so no errors here") && s.contains("gives it a DSN"), "{s}");
        assert!(!s.contains('\u{202e}'), "{s}");
        // The window a host sends that this side does not know changes nothing.
        ui.set_window("2d");
        assert_eq!(ui.app.window, Window::Hour);
    }

    #[tokio::test]
    async fn panicking_load_is_reported_not_lost() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        async fn boom() -> Resp {
            panic!("boom in fetch")
        }
        guarded(Kind::Stats, boom(), tx).await;
        match rx.recv().await {
            Some(Resp::Panicked { kind: Kind::Stats, msg }) => assert!(msg.contains("boom in fetch")),
            other => panic!("unexpected {other:?}"),
        }
        let mut app = App::new("http://x", None);
        app.on_response(Resp::Panicked { kind: Kind::Info, msg: "boom".into() });
        assert_eq!(app.fatal.as_deref(), Some("boom"));
    }
}
