//! `sdl tui`: interactive dashboard.
//!
//! [`Embedded`] is the whole TUI as a component without terminal setup, so
//! a host program (e.g. obs-ducklake) can draw it inside its own screen and
//! event loop. [`run`] is the standalone `sdl tui`: terminal setup, event
//! loop and panic-safe restore around `Embedded`.

mod app;
#[cfg(test)]
mod fixtures;
mod theme;
mod ui;

use crate::cli::{Hints, describe};
use app::{App, FetchError, Kind, Req, Resp};
use crossterm::event::{Event, KeyEventKind};
use futures::FutureExt;
use ratatui::Frame;
use ratatui::layout::Rect;
use sdl_client::Client;
use sdl_client::types::{EventQuery, WindowQuery};
use std::future::Future;
use std::io::IsTerminal;
use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

/// What the host should do after [`Embedded::handle_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The key was used.
    Consumed,
    /// The key was not used; the host may handle it.
    Ignored,
    /// The user asked to quit (`q` on the issue list, `ctrl-c`).
    Quit,
    /// The user pressed `t` on an event that has this trace id.
    OpenTrace(String),
}

/// The TUI as an embeddable component. Data loads run as tasks on the tokio
/// runtime that was current when [`Embedded::new`] was called; their results
/// are picked up in [`Embedded::tick`].
pub struct Embedded {
    app: App,
    client: Client,
    handle: Handle,
    tx: mpsc::UnboundedSender<Resp>,
    rx: mpsc::UnboundedReceiver<Resp>,
    started: bool,
}

impl Embedded {
    /// Must be called inside a tokio runtime context (e.g. from async code
    /// or under `Runtime::enter`); panics otherwise. The other methods can
    /// then be called from any thread.
    pub fn new(client: Client, project: Option<String>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = Handle::current();
        Embedded { app: App::new(client.base_url(), project), client, handle, tx, rx, started: false }
    }

    /// Name the host's command and env vars in hints (default [`Hints::SDL`]).
    pub fn with_hints(mut self, hints: Hints) -> Self {
        self.app.hints = hints;
        self
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
        let reqs = self.app.on_key(*k);
        self.dispatch(reqs);
        if self.app.quit {
            self.app.quit = false;
            return Outcome::Quit;
        }
        if let Some(t) = self.app.open_trace.take() {
            return Outcome::OpenTrace(t);
        }
        if self.app.ignored { Outcome::Ignored } else { Outcome::Consumed }
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

    /// Show the error events of this trace (all projects).
    pub fn show_trace_errors(&mut self, trace_id: &str) {
        self.ensure_started();
        let reqs = self.app.show_trace(trace_id);
        self.dispatch(reqs);
    }

    fn ensure_started(&mut self) {
        if !self.started {
            self.started = true;
            let reqs = self.app.start();
            self.dispatch(reqs);
        }
    }

    fn dispatch(&self, reqs: Vec<Req>) {
        for r in reqs {
            let kind = r.kind();
            self.handle.spawn(guarded(kind, fetch(self.client.clone(), r, self.app.hints), self.tx.clone()));
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

fn fetch_error(err: sdl_client::Error, url: &str, hints: &Hints) -> FetchError {
    // Network-level failures mean "offline"; a body we can't decode is a
    // version mismatch, not an outage.
    let offline = matches!(&err, sdl_client::Error::Http(h) if !h.is_decode());
    FetchError { message: describe(&err, url, hints), offline }
}

async fn fetch(c: Client, req: Req, hints: Hints) -> Resp {
    let url = c.base_url().to_string();
    let e = |err: sdl_client::Error| fetch_error(err, &url, &hints);
    match req {
        Req::Info => Resp::Info(c.info().await.map_err(e)),
        Req::Projects => Resp::Projects(c.projects().await.map_err(e)),
        Req::Issues(key) => {
            let r = c.issues(&key.project, &key.query()).await.map_err(e);
            Resp::Issues(key, r)
        }
        Req::Stats { project, window } => {
            let (since, interval) = app::WINDOWS[window];
            let q = WindowQuery { since: Some(since.into()), interval: Some(interval.into()) };
            let result = c.stats(&project, &q).await.map_err(e);
            Resp::Stats { project, window, result }
        }
        Req::Issue { project, id, window } => {
            let q = WindowQuery { since: Some(app::WINDOWS[window].0.into()), interval: None };
            let result = c.issue(&project, &id, &q).await.map_err(e);
            Resp::Issue { id, result }
        }
        Req::Events { project, issue, before } => {
            let append = before.is_some();
            let q = EventQuery { issue: Some(issue.clone()), limit: Some(50), before, ..Default::default() };
            let result = c.events(&project, &q).await.map_err(e);
            Resp::Events { issue, append, result }
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
    }
}

/// Standalone `sdl tui`.
pub async fn run(ctx: &crate::cli::Ctx) -> anyhow::Result<()> {
    use futures::StreamExt;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!("{} tui needs an interactive terminal (stdin and stdout must be a TTY)", ctx.hints.command);
    }
    // Fail fast with a friendly message instead of an empty dashboard.
    ctx.client.info().await.map_err(|e| ctx.err(e))?;
    let mut ui = Embedded::new(ctx.client.clone(), ctx.project.clone()).with_hints(ctx.hints);
    // `try_init` enters raw mode + alternate screen and installs a panic
    // hook that restores the terminal before printing the panic.
    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            ratatui::restore();
            anyhow::bail!("cannot set up the terminal: {e}");
        }
    };
    let mut events = crossterm::event::EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    let result: anyhow::Result<()> = async {
        loop {
            terminal.draw(|f| ui.draw(f, f.area()))?;
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(ev)) => match ui.handle_event(&ev) {
                        Outcome::Quit => break,
                        Outcome::OpenTrace(t) => ui.show_trace_errors(&t),
                        Outcome::Consumed | Outcome::Ignored => {}
                    },
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                _ = ticker.tick() => ui.tick(),
            }
            if let Some(msg) = ui.take_fatal() {
                // The panic hook has already restored the terminal; stop
                // drawing on the normal screen and exit with the message.
                anyhow::bail!("internal error in a background load: {msg}");
            }
        }
        Ok(())
    }
    .await;
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(c: KeyCode) -> Event {
        Event::Key(KeyEvent::from(c))
    }

    fn screen(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Drives `Embedded` through draw / handle_event / tick. No server runs
    /// on the port, so loads fail; the UI must stay responsive and show the
    /// error instead of hanging.
    #[tokio::test]
    async fn embedded_draw_handle_tick() {
        let client = Client::new("http://127.0.0.1:9", "t").unwrap();
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

        ui.show_trace_errors("b7644661850f40e69380d8c9ea253e84");
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
            Embedded::new(Client::new("http://127.0.0.1:9", "t").unwrap(), None)
        };
        // Not in a runtime context here: these spawn loads and must not panic.
        assert_eq!(ui.handle_event(&key(KeyCode::Char('R'))), Outcome::Consumed);
        ui.tick();
        ui.show_trace_errors("b7644661850f40e69380d8c9ea253e84");
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| ui.draw(f, f.area())).unwrap();
    }

    /// The host learns about a panicked load through `take_fatal`.
    #[tokio::test]
    async fn a_panicked_load_reaches_the_host() {
        let mut ui = Embedded::new(Client::new("http://127.0.0.1:9", "t").unwrap(), Some("web".into()));
        assert_eq!(ui.take_fatal(), None);
        ui.tx.send(Resp::Panicked { kind: Kind::Info, msg: "boom".into() }).unwrap();
        ui.tick();
        assert_eq!(ui.take_fatal().as_deref(), Some("boom"));
        assert_eq!(ui.take_fatal(), None, "reported once");
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
