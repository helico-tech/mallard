//! `mallard tui`: hosts the TUIs of both libraries as tabs (ADR 0002).
//! F1 errors (mallard-sentry-cli), F2 telemetry (mallard-otel-cli). Jumps between
//! them go by trace id. The host holds what is one for all tabs: the app (`p`, the
//! picker) and the time window (`t`, `T`). The tabs return those keys as `Ignored`.
//! `ctrl-k` opens [`find`], above whatever a tab shows.

mod find;
mod picker;

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use find::{Find, FindKey};
use futures::StreamExt;
use mallard_client::Client;
use mallard_client::mallard_api::{AppList, AppsQuery, FindMatch, FindQuery, FindResult};
use mallard_term::tui::{Outcome, Scope, keys, theme};
use picker::{PickerKey, Shared};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// One library TUI, as the host sees it. Both libraries return the shared
/// [`Outcome`]; `Outcome::Jump(trace)` opens the trace on the other tab,
/// `Outcome::JumpEvent` one event of it.
trait Pane {
    fn title(&self) -> &'static str;
    fn draw(&mut self, f: &mut Frame, area: Rect);
    fn handle(&mut self, ev: &Event) -> Outcome;
    fn tick(&mut self);
    fn open_trace(&mut self, trace_id: &str);
    /// One event of the trace; a tab without events shows the trace.
    fn open_trace_event(&mut self, trace_id: &str, _event_id: &str) {
        self.open_trace(trace_id)
    }
    /// A trace found by id, near `time`, on `span` if given. Only the telemetry tab has it.
    fn open_trace_at(&mut self, _trace_id: &str, _time: DateTime<Utc>, _span: Option<&str>) {}
    /// An issue found by id; with an event, that event. Only the errors tab has it.
    fn open_issue(&mut self, _project: &str, _issue: &str, _event: Option<&str>) {}
    /// An internal error (a panicked background load) after `tick`.
    fn take_fatal(&mut self) -> Option<String> {
        None
    }
    /// The one window of all tabs.
    fn set_window(&mut self, _since: &str) {}
    /// The chosen app.
    fn set_scope(&mut self, _scope: &Scope) {}
    /// The project shown, which can change by itself when an event of another opens.
    fn project(&self) -> Option<String> {
        None
    }
    /// The longest window the server of this tab allows, once it has said so.
    fn max_window_secs(&self) -> Option<u64> {
        None
    }
}

impl Pane for mallard_sentry_cli::Embedded {
    fn title(&self) -> &'static str {
        "Errors"
    }
    fn draw(&mut self, f: &mut Frame, area: Rect) {
        mallard_sentry_cli::Embedded::draw(self, f, area)
    }
    fn handle(&mut self, ev: &Event) -> Outcome {
        self.handle_event(ev)
    }
    fn tick(&mut self) {
        mallard_sentry_cli::Embedded::tick(self)
    }
    fn open_trace(&mut self, trace_id: &str) {
        mallard_sentry_cli::Embedded::open_trace(self, trace_id)
    }
    fn open_trace_event(&mut self, trace_id: &str, event_id: &str) {
        mallard_sentry_cli::Embedded::open_trace_event(self, trace_id, event_id)
    }
    fn open_issue(&mut self, project: &str, issue: &str, event: Option<&str>) {
        mallard_sentry_cli::Embedded::open_issue(self, project, issue, event)
    }
    fn take_fatal(&mut self) -> Option<String> {
        mallard_sentry_cli::Embedded::take_fatal(self)
    }
    fn set_window(&mut self, since: &str) {
        mallard_sentry_cli::Embedded::set_window(self, since)
    }
    fn set_scope(&mut self, scope: &Scope) {
        mallard_sentry_cli::Embedded::set_scope(self, scope)
    }
    fn project(&self) -> Option<String> {
        mallard_sentry_cli::Embedded::project(self).map(str::to_string)
    }
    fn max_window_secs(&self) -> Option<u64> {
        mallard_sentry_cli::Embedded::max_window_secs(self)
    }
}

impl Pane for mallard_otel_cli::Embedded {
    fn title(&self) -> &'static str {
        "Telemetry"
    }
    fn draw(&mut self, f: &mut Frame, area: Rect) {
        mallard_otel_cli::Embedded::draw(self, f, area)
    }
    fn handle(&mut self, ev: &Event) -> Outcome {
        self.handle_event(ev)
    }
    fn tick(&mut self) {
        mallard_otel_cli::Embedded::tick(self)
    }
    fn open_trace(&mut self, trace_id: &str) {
        // An invalid id is shown in the tab itself.
        let _ = mallard_otel_cli::Embedded::open_trace(self, trace_id);
    }
    fn open_trace_at(&mut self, trace_id: &str, time: DateTime<Utc>, span: Option<&str>) {
        let _ = mallard_otel_cli::Embedded::open_trace_at(self, trace_id, Some(time), span);
    }
    fn set_window(&mut self, since: &str) {
        // The presets are valid windows.
        let _ = mallard_otel_cli::Embedded::set_since(self, since);
    }
    fn set_scope(&mut self, scope: &Scope) {
        mallard_otel_cli::Embedded::set_scope(self, scope)
    }
    fn max_window_secs(&self) -> Option<u64> {
        mallard_otel_cli::Embedded::max_window_secs(self)
    }
}

/// How often the list of apps is read again, also after a failed read.
const APPS_EVERY: Duration = Duration::from_secs(30);
const APPS_RETRY: Duration = Duration::from_secs(5);

/// One read of the list: its window, and the answer or the message.
type AppsAnswer = (&'static str, Result<AppList, String>);

/// One lookup of `ctrl-k`: the id, its window, and the answer or the message.
type FindAnswer = (String, &'static str, Result<Option<FindResult>, String>);

struct Host {
    tabs: Vec<Box<dyn Pane>>,
    active: usize,
    base_url: String,
    /// `None` in tests that feed the list by hand.
    client: Option<Client>,
    shared: Shared,
    /// What the errors tab showed last: a change the host did not ask for is followed.
    last_project: Option<String>,
    apps_tx: std::sync::mpsc::Sender<AppsAnswer>,
    apps_rx: std::sync::mpsc::Receiver<AppsAnswer>,
    apps_task: Option<tokio::task::AbortHandle>,
    /// When to read the list next.
    apps_at: Option<Instant>,
    /// `ctrl-k`, while it is open.
    find: Option<Find>,
    find_tx: std::sync::mpsc::Sender<FindAnswer>,
    find_rx: std::sync::mpsc::Receiver<FindAnswer>,
    find_task: Option<tokio::task::AbortHandle>,
}

const ERRORS: usize = 0;
const TELEMETRY: usize = 1;

impl Host {
    /// `wanted` is the first choice of the picker (`-p`, `--app`).
    fn new(tabs: Vec<Box<dyn Pane>>, base_url: String, client: Option<Client>, wanted: Option<String>) -> Self {
        let (apps_tx, apps_rx) = std::sync::mpsc::channel();
        let (find_tx, find_rx) = std::sync::mpsc::channel();
        Host {
            tabs,
            active: ERRORS,
            base_url,
            client,
            shared: Shared::new(wanted),
            last_project: None,
            apps_tx,
            apps_rx,
            apps_task: None,
            apps_at: Some(Instant::now()),
            find: None,
            find_tx,
            find_rx,
            find_task: None,
        }
    }

    /// Read the list in the background. A newer read replaces one still running.
    fn read_apps(&mut self) {
        let (Some(client), Ok(rt)) = (self.client.clone(), tokio::runtime::Handle::try_current()) else {
            return;
        };
        self.apps_at = Some(Instant::now() + APPS_EVERY);
        let since = self.shared.since();
        let tx = self.apps_tx.clone();
        let base = self.base_url.clone();
        let task = rt.spawn(async move {
            let q = AppsQuery { since: Some(since.to_string()), ..Default::default() };
            let answer = client.apps(&q).await.map_err(|e| crate::classify(&e, &base).to_string());
            let _ = tx.send((since, answer));
        });
        if let Some(old) = self.apps_task.replace(task.abort_handle()) {
            old.abort();
        }
    }

    fn apply_apps(&mut self, since: &str, answer: Result<AppList, String>) {
        if since != self.shared.since() {
            return;
        }
        match answer {
            Ok(list) => {
                self.shared.loaded(list);
                self.apply_scope();
            }
            Err(e) => {
                self.shared.error = Some(e);
                self.apps_at = Some(Instant::now() + APPS_RETRY);
            }
        }
    }

    /// Scope every tab to the choice. The tabs do nothing when it is the same.
    fn apply_scope(&mut self) {
        if let Some(scope) = self.shared.scope() {
            for t in &mut self.tabs {
                t.set_scope(&scope);
            }
        }
    }

    /// The window changed: tell every tab. Which services have data depends on it.
    fn apply_window(&mut self) {
        let since = self.shared.since();
        for t in &mut self.tabs {
            t.set_window(since);
        }
        self.read_apps();
    }

    /// When a tab went to another project by itself, the picker goes along.
    fn follow_project(&mut self) {
        let p = self.tabs.get(ERRORS).and_then(|t| t.project());
        if p != self.last_project {
            self.last_project = p.clone();
            if p.is_some_and(|p| self.shared.follow(&p)) {
                self.apply_scope();
            }
        }
    }

    /// A key no tab used.
    fn host_key(&mut self, ev: &Event) {
        let Event::Key(k) = ev else { return };
        if k.kind == KeyEventKind::Release || k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return;
        }
        match k.code {
            KeyCode::Char(keys::APP_PICKER) => {
                self.shared.open_picker();
                self.read_apps();
            }
            KeyCode::Char(keys::WINDOW_NEXT) => {
                self.shared.cycle_window(true);
                self.apply_window();
            }
            KeyCode::Char(keys::WINDOW_PREV) => {
                self.shared.cycle_window(false);
                self.apply_window();
            }
            _ => {}
        }
    }

    fn picker_event(&mut self, ev: &Event) -> bool {
        let Event::Key(k) = ev else { return true };
        if k.kind == KeyEventKind::Release {
            return true;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            return k.code != KeyCode::Char('c');
        }
        if self.shared.picker_key(k.code) == PickerKey::Chose {
            self.apply_scope();
            self.last_project = self.tabs.get(ERRORS).and_then(|t| t.project());
        }
        true
    }

    /// A key while `ctrl-k` is open: the tabs get none. Returns false to quit.
    fn find_event(&mut self, ev: &Event) -> bool {
        let (Event::Key(k), Some(find)) = (ev, &mut self.find) else { return true };
        if k.kind == KeyEventKind::Release {
            return true;
        }
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            match k.code {
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return false,
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => find.clear(),
                KeyCode::Char(keys::FIND_CTRL) if k.modifiers.contains(KeyModifiers::CONTROL) => self.close_find(),
                _ => {}
            }
            return true;
        }
        match find.key(k.code) {
            FindKey::Stay => {}
            FindKey::Close => self.close_find(),
            FindKey::Look(id) => self.look_up(id),
            FindKey::Land(m) => self.land(&m),
        }
        true
    }

    fn close_find(&mut self) {
        self.find = None;
        if let Some(task) = self.find_task.take() {
            task.abort();
        }
    }

    /// Look in the longest window there is, as the web UI: an id says what it wants.
    fn look_up(&mut self, id: String) {
        let (Some(client), Ok(rt)) = (self.client.clone(), tokio::runtime::Handle::try_current()) else {
            return;
        };
        let since = self.shared.longest_window();
        let tx = self.find_tx.clone();
        let base = self.base_url.clone();
        let task = rt.spawn(async move {
            let q = FindQuery { id: id.clone(), project: None, from: None, to: None, since: Some(since.to_string()) };
            let answer = client.find(&q).await.map_err(|e| crate::classify(&e, &base).to_string());
            let _ = tx.send((id, since, answer));
        });
        if let Some(old) = self.find_task.replace(task.abort_handle()) {
            old.abort();
        }
    }

    fn apply_find(&mut self, id: &str, since: &str, answer: Result<Option<FindResult>, String>) {
        if let Some(m) = self.find.as_mut().and_then(|find| find.answered(id, since, answer)) {
            self.land(&m);
        }
    }

    /// Open what was found, on the tab of its side.
    fn land(&mut self, m: &FindMatch) {
        self.close_find();
        match m {
            FindMatch::Trace { trace_id, time, .. } => {
                self.tabs[TELEMETRY].open_trace_at(trace_id, *time, None);
                self.active = TELEMETRY;
            }
            FindMatch::Span { trace_id, span_id, time, .. } => {
                self.tabs[TELEMETRY].open_trace_at(trace_id, *time, Some(span_id));
                self.active = TELEMETRY;
            }
            FindMatch::Event { project, issue_id, event_id, .. } => {
                self.tabs[ERRORS].open_issue(project, issue_id, Some(event_id));
                self.active = ERRORS;
            }
            FindMatch::Issue { project, issue_id, .. } => {
                self.tabs[ERRORS].open_issue(project, issue_id, None);
                self.active = ERRORS;
            }
        }
        self.follow_project();
    }

    fn draw(&mut self, f: &mut Frame) {
        let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(f.area());
        let mut spans = vec![Span::styled(" mallard ", theme::tab_active())];
        for (i, t) in self.tabs.iter().enumerate() {
            let label = format!(" F{} {} ", i + 1, t.title());
            let style = if i == self.active {
                Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                theme::tab_inactive()
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(label, style));
        }
        spans.push(Span::styled("   p ", theme::key()));
        spans.push(Span::styled(self.shared.label(), theme::accent()));
        spans.push(Span::styled("  t ", theme::key()));
        spans.push(Span::styled(format!("last {}", self.shared.since()), theme::accent()));
        spans.push(Span::styled(format!("   {}", mallard_term::text::clean(&self.base_url)), theme::faint()));
        f.render_widget(Paragraph::new(Line::from(spans)), bar);
        self.tabs[self.active].draw(f, body);
        self.shared.render(f, body);
        if let Some(find) = &self.find {
            find.render(f, body);
        }
    }

    /// Returns false to quit.
    fn on_event(&mut self, ev: &Event) -> bool {
        if self.shared.picker.is_some() {
            return self.picker_event(ev);
        }
        if self.find.is_some() {
            return self.find_event(ev);
        }
        // Before the tabs: it works on every screen, also while a filter is being typed.
        if let Event::Key(k) = ev
            && is_find_key(k)
        {
            self.find = Some(Find::new());
            return true;
        }
        if let Event::Key(k) = ev
            && k.kind != KeyEventKind::Release
            && let KeyCode::F(n) = k.code
            && (1..=self.tabs.len() as u8).contains(&n)
        {
            self.active = n as usize - 1;
            return true;
        }
        let go_on = match self.tabs[self.active].handle(ev) {
            Outcome::Consumed => true,
            Outcome::Ignored => {
                self.host_key(ev);
                true
            }
            Outcome::Quit => false,
            Outcome::Jump(t) => self.jump(&t, None),
            Outcome::JumpEvent { trace_id, event_id } => self.jump(&trace_id, Some(&event_id)),
            Outcome::SetWindow(since) => {
                let secs = mallard_term::tui::WINDOW_PRESETS.iter().find(|(n, _)| *n == since).map_or(0, |(_, s)| *s);
                if self.shared.window_at_most(secs) {
                    self.apply_window();
                }
                true
            }
        };
        self.follow_project();
        go_on
    }

    /// Tick every tab (hidden ones keep loading). An internal error in a
    /// tab ends the TUI: its panic hook has already restored the terminal.
    fn tick(&mut self) -> Result<()> {
        for t in &mut self.tabs {
            t.tick();
            if let Some(msg) = t.take_fatal() {
                anyhow::bail!("internal error in the {} tab: {msg}", t.title());
            }
        }
        while let Ok((since, answer)) = self.apps_rx.try_recv() {
            self.apply_apps(since, answer);
        }
        while let Ok((id, since, answer)) = self.find_rx.try_recv() {
            self.apply_find(&id, since, answer);
        }
        // The smaller of what the sides report: a preset one refuses is no choice.
        let limit = self.tabs.iter().filter_map(|t| t.max_window_secs()).min();
        if self.shared.set_limit(limit) {
            self.apply_window();
        }
        self.follow_project();
        if self.apps_at.is_some_and(|t| Instant::now() >= t) {
            self.read_apps();
        }
        Ok(())
    }

    /// Two tabs: a jump always goes to the other one.
    fn jump(&mut self, trace_id: &str, event_id: Option<&str>) -> bool {
        let other = if self.active == ERRORS { TELEMETRY } else { ERRORS };
        if let Some(p) = self.tabs.get_mut(other) {
            match event_id {
                Some(e) => p.open_trace_event(trace_id, e),
                None => p.open_trace(trace_id),
            }
            self.active = other;
        }
        true
    }
}

fn is_find_key(k: &KeyEvent) -> bool {
    k.kind != KeyEventKind::Release && k.code == KeyCode::Char(keys::FIND_CTRL) && k.modifiers == KeyModifiers::CONTROL
}

/// The otel TUI, with its "x: errors of this trace" jump enabled (the
/// errors tab can answer it) and Sentry errors marked on traces and spans.
fn telemetry_pane(client: &Client) -> mallard_otel_cli::Embedded {
    let mut pane = mallard_otel_cli::Embedded::new(client.otel.clone());
    pane.set_errors_enabled(true);
    pane.set_error_lookup(std::sync::Arc::new(crate::errors::SentryErrors(client.clone())));
    pane
}

/// The errors TUI, with no choice of its own: the host's app and window.
fn errors_pane(client: &Client, project: Option<String>, since: &str) -> mallard_sentry_cli::Embedded {
    let mut pane = mallard_sentry_cli::Embedded::new(client.sentry.clone(), project);
    pane.set_window(since);
    pane
}

fn host(client: &Client, project: Option<String>) -> Host {
    let shared = Shared::new(None);
    let tabs: Vec<Box<dyn Pane>> =
        vec![Box::new(errors_pane(client, project.clone(), shared.since())), Box::new(telemetry_pane(client))];
    Host::new(tabs, client.base_url().to_string(), Some(client.clone()), project)
}

pub async fn run(client: &Client, project: Option<String>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!("mallard tui needs an interactive terminal (stdin and stdout must be a TTY)");
    }
    // Fail fast with a clear message instead of an empty screen.
    client.sentry.info().await.map_err(|e| crate::described(mallard_client::Error::Sentry(e), client.base_url()))?;
    let mut host = host(client, project);

    let mut term = ratatui::try_init().map_err(|e| {
        ratatui::restore();
        anyhow::anyhow!("cannot set up the terminal: {e}")
    })?;
    let result = event_loop(&mut term, &mut host).await;
    ratatui::restore();
    result
}

async fn event_loop(term: &mut ratatui::DefaultTerminal, host: &mut Host) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        term.draw(|f| host.draw(f))?;
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(ev)) => if !host.on_event(&ev) { return Ok(()) },
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
            _ = tick.tick() => host.tick()?,
        }
    }
}

#[cfg(test)]
#[path = "tui_screenshots.rs"]
mod screenshots;

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
    type Project = std::sync::Arc<std::sync::Mutex<Option<String>>>;

    struct Fake {
        opened: Vec<String>,
        title: &'static str,
        ask_on_x: fn() -> Outcome,
        fatal: Option<String>,
        seen: Seen,
        project: Project,
    }
    impl Pane for Fake {
        fn title(&self) -> &'static str {
            self.title
        }
        fn draw(&mut self, f: &mut Frame, area: Rect) {
            f.render_widget(Paragraph::new(format!("{} body {}", self.title, self.opened.join(","))), area)
        }
        fn handle(&mut self, ev: &Event) -> Outcome {
            match ev {
                Event::Key(k) if k.code == KeyCode::Char('x') => (self.ask_on_x)(),
                Event::Key(k) if k.code == KeyCode::Char('q') => Outcome::Quit,
                _ => Outcome::Ignored,
            }
        }
        fn tick(&mut self) {}
        fn open_trace(&mut self, trace_id: &str) {
            self.opened.push(trace_id.into())
        }
        fn open_trace_event(&mut self, trace_id: &str, event_id: &str) {
            self.opened.push(format!("{trace_id}/{event_id}"))
        }
        fn open_trace_at(&mut self, trace_id: &str, time: DateTime<Utc>, span: Option<&str>) {
            self.opened.push(format!("{trace_id}@{} {span:?}", time.timestamp()))
        }
        fn open_issue(&mut self, project: &str, issue: &str, event: Option<&str>) {
            self.opened.push(format!("{project}/{issue} {event:?}"))
        }
        fn take_fatal(&mut self) -> Option<String> {
            self.fatal.take()
        }
        fn set_window(&mut self, since: &str) {
            self.seen.lock().unwrap().push(format!("{} window {since}", self.title));
        }
        fn set_scope(&mut self, scope: &Scope) {
            let what = match scope {
                Scope::All => "all".to_string(),
                Scope::App { slug, project, services } => format!("{slug} {project:?} {services:?}"),
            };
            self.seen.lock().unwrap().push(format!("{} scope {what}", self.title));
        }
        fn project(&self) -> Option<String> {
            self.project.lock().unwrap().clone()
        }
        fn max_window_secs(&self) -> Option<u64> {
            (self.title == "Telemetry").then_some(7 * 86_400)
        }
    }

    fn key(c: KeyCode) -> Event {
        Event::Key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    fn fake(title: &'static str, ask_on_x: fn() -> Outcome) -> Fake {
        Fake { opened: vec![], title, ask_on_x, fatal: None, seen: Default::default(), project: Default::default() }
    }

    fn host() -> Host {
        let tabs: Vec<Box<dyn Pane>> = vec![
            Box::new(fake("Errors", || Outcome::Jump("t1".into()))),
            Box::new(fake("Telemetry", || Outcome::Jump("t2".into()))),
        ];
        Host::new(tabs, "http://x".into(), None, None)
    }

    /// A host whose tabs write what they are told into `seen`, and whose errors tab
    /// shows the project in `project`.
    fn watched() -> (Host, Seen, Project) {
        let (seen, project): (Seen, Project) = Default::default();
        let mut errors = fake("Errors", || Outcome::SetWindow("24h"));
        (errors.seen, errors.project) = (seen.clone(), project.clone());
        let mut telemetry = fake("Telemetry", || Outcome::Jump("t2".into()));
        telemetry.seen = seen.clone();
        let mut h = host();
        h.tabs = vec![Box::new(errors), Box::new(telemetry)];
        (h, seen, project)
    }

    fn apps() -> AppList {
        use mallard_client::mallard_api::App;
        let app = |slug: &str, project, services: &[&str]| App {
            slug: slug.into(),
            name: slug.into(),
            project,
            services: services.iter().map(|s| s.to_string()).collect(),
        };
        AppList {
            apps: vec![
                app("api", Some(1), &["API", "api"]),
                app("web", Some(2), &[]),
                app("worker", None, &["worker"]),
            ],
            warnings: vec![],
        }
    }

    fn screen(h: &mut Host) -> String {
        let mut t = Terminal::new(TestBackend::new(60, 3)).unwrap();
        t.draw(|f| h.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn function_keys_switch_tabs() {
        let mut h = host();
        assert!(screen(&mut h).contains("Errors body"));
        assert!(h.on_event(&key(KeyCode::F(2))));
        assert!(screen(&mut h).contains("Telemetry body"));
        assert!(h.on_event(&key(KeyCode::F(9))), "unknown F-key is passed on, not a crash");
        assert_eq!(h.active, TELEMETRY);
    }

    #[test]
    fn jumps_carry_the_trace_id() {
        let mut h = host();
        h.on_event(&key(KeyCode::Char('x')));
        assert_eq!(h.active, TELEMETRY);
        h.on_event(&key(KeyCode::Char('x')));
        assert_eq!(h.active, ERRORS);
        assert!(!h.on_event(&key(KeyCode::Char('q'))), "q quits");
    }

    /// `x` on a span with a Sentry event opens that event on the errors tab.
    #[test]
    fn event_jumps_carry_the_event_id() {
        let mut h = host();
        h.active = TELEMETRY;
        h.tabs[TELEMETRY] =
            Box::new(fake("Telemetry", || Outcome::JumpEvent { trace_id: "t2".into(), event_id: "e1".into() }));
        h.on_event(&key(KeyCode::Char('x')));
        assert_eq!(h.active, ERRORS);
        assert!(screen(&mut h).contains("Errors body t2/e1"), "{}", screen(&mut h));
        h.on_event(&key(KeyCode::Char('x')));
        assert_eq!(h.active, TELEMETRY, "a plain jump back");
        assert!(screen(&mut h).contains("Telemetry body t1"), "{}", screen(&mut h));
    }

    #[test]
    fn an_internal_error_in_a_tab_ends_the_tui() {
        let mut h = host();
        assert!(h.tick().is_ok());
        h.tabs[ERRORS] = Box::new(Fake { fatal: Some("boom".into()), ..fake("Errors", || Outcome::Consumed) });
        let err = h.tick().unwrap_err().to_string();
        assert_eq!(err, "internal error in the Errors tab: boom");
    }

    fn host_of(c: &Client) -> Host {
        super::host(c, None)
    }

    /// Keys the tabs leave alone are the host's: one window, one picker.
    #[test]
    fn t_changes_the_window_of_every_tab() {
        let (mut h, seen, _) = watched();
        h.on_event(&key(KeyCode::Char('t')));
        assert_eq!(*seen.lock().unwrap(), ["Errors window 6h", "Telemetry window 6h"]);
        seen.lock().unwrap().clear();
        h.on_event(&key(KeyCode::F(2)));
        h.on_event(&key(KeyCode::Char('T')));
        assert_eq!(*seen.lock().unwrap(), ["Errors window 1h", "Telemetry window 1h"], "from either tab");
        assert!(screen(&mut h).contains("last 1h"));
    }

    #[test]
    fn the_limit_is_the_smaller_of_what_the_tabs_report() {
        let (mut h, seen, _) = watched();
        h.tick().unwrap();
        assert!(seen.lock().unwrap().is_empty(), "1h is fine");
        for _ in 0..3 {
            h.on_event(&key(KeyCode::Char('t')));
        }
        assert_eq!(h.shared.since(), "7d", "up to the telemetry tab's limit");
        h.on_event(&key(KeyCode::Char('t')));
        assert_eq!(h.shared.since(), "5m", "14d and 30d are cut");
    }

    #[test]
    fn the_empty_errors_tab_can_jump_to_a_day() {
        let (mut h, seen, _) = watched();
        h.on_event(&key(KeyCode::Char('x')));
        assert_eq!(*seen.lock().unwrap(), ["Errors window 24h", "Telemetry window 24h"]);
        assert_eq!(h.active, ERRORS, "stays on the tab");
    }

    #[test]
    fn the_picker_scopes_both_tabs() {
        let (mut h, seen, _) = watched();
        h.apply_apps("1h", Ok(apps()));
        assert_eq!(*seen.lock().unwrap(), ["Errors scope all", "Telemetry scope all"], "All apps is the default");
        seen.lock().unwrap().clear();
        assert!(screen(&mut h).contains("All apps"));
        h.on_event(&key(KeyCode::Char('p')));
        assert!(h.shared.picker.is_some());
        // While it is open the tab gets no keys.
        h.on_event(&key(KeyCode::Char('x')));
        assert_eq!(h.active, ERRORS);
        h.on_event(&key(KeyCode::Down));
        h.on_event(&key(KeyCode::Enter));
        assert_eq!(
            *seen.lock().unwrap(),
            [r#"Errors scope api Some("api") ["API", "api"]"#, r#"Telemetry scope api Some("api") ["API", "api"]"#]
        );
        assert!(h.shared.picker.is_none());
        h.on_event(&key(KeyCode::Char('p')));
        h.on_event(&key(KeyCode::Esc));
        assert_eq!(seen.lock().unwrap().len(), 2, "esc changes nothing");
        let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        h.on_event(&key(KeyCode::Char('p')));
        assert!(!h.on_event(&ctrl_c), "ctrl-c quits from the picker too");
    }

    const TRACE_ID: &str = "0123456789abcdef0123456789abcdef";

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// `ctrl-k`, the trace id and Enter; the host now waits for the answer.
    fn looking(h: &mut Host) {
        h.on_event(&ctrl('k'));
        for c in TRACE_ID.chars() {
            h.on_event(&key(KeyCode::Char(c)));
        }
        h.on_event(&key(KeyCode::Enter));
    }

    fn big_screen(h: &mut Host) -> String {
        let mut t = Terminal::new(TestBackend::new(90, 14)).unwrap();
        t.draw(|f| h.draw(f)).unwrap();
        t.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    fn found_trace() -> FindMatch {
        FindMatch::Trace {
            trace_id: TRACE_ID.into(),
            time: DateTime::from_timestamp(1000, 0).unwrap(),
            spans: 3,
            errors: 1,
            logs: 2,
        }
    }

    fn found_event() -> FindMatch {
        FindMatch::Event {
            event_id: TRACE_ID.into(),
            issue_id: "4803bf4f1add995d".into(),
            project: "api".into(),
            project_id: 1,
            title: "Boom\x1b[31m".into(),
            trace_id: None,
        }
    }

    #[test]
    fn one_match_lands_on_it_on_the_tab_of_its_side() {
        let mut h = host();
        looking(&mut h);
        assert_eq!(h.active, ERRORS, "not before the answer");
        h.apply_find(TRACE_ID, "30d", Ok(Some(FindResult { matches: vec![found_trace()], warnings: vec![] })));
        assert_eq!(h.active, TELEMETRY);
        assert!(h.find.is_none());
        assert!(screen(&mut h).contains(&format!("Telemetry body {TRACE_ID}@1000 None")), "{}", screen(&mut h));

        looking(&mut h);
        h.apply_find(TRACE_ID, "30d", Ok(Some(FindResult { matches: vec![found_event()], warnings: vec![] })));
        assert_eq!(h.active, ERRORS);
        assert!(screen(&mut h).contains(r#"Errors body api/4803bf4f1add995d Some("0123"#), "{}", screen(&mut h));
    }

    #[test]
    fn a_span_opens_its_trace_on_that_span() {
        let mut h = host();
        h.on_event(&ctrl('k'));
        for c in "0123456789abcdef".chars() {
            h.on_event(&key(KeyCode::Char(c)));
        }
        h.on_event(&key(KeyCode::Enter));
        let span = FindMatch::Span {
            span_id: "0123456789abcdef".into(),
            trace_id: TRACE_ID.into(),
            service: "api".into(),
            name: "op".into(),
            time: DateTime::from_timestamp(1000, 0).unwrap(),
        };
        h.apply_find("0123456789abcdef", "30d", Ok(Some(FindResult { matches: vec![span], warnings: vec![] })));
        assert_eq!(h.active, TELEMETRY);
        let mut big = big_screen(&mut h);
        big.retain(|c| c != ' ');
        assert!(big.contains(r#"@1000Some("0123456789abcdef")"#), "{big}");
    }

    #[test]
    fn several_matches_are_a_choice() {
        let mut h = host();
        looking(&mut h);
        h.apply_find(
            TRACE_ID,
            "30d",
            Ok(Some(FindResult { matches: vec![found_trace(), found_event()], warnings: vec![] })),
        );
        assert!(h.find.is_some());
        let s = big_screen(&mut h);
        assert!(s.contains("3 spans, 1 errors, 2 logs") && s.contains("event in api"), "{s}");
        assert!(!s.contains('\x1b'), "server text is cleaned: {s}");
        h.on_event(&key(KeyCode::Down));
        h.on_event(&key(KeyCode::Enter));
        assert_eq!(h.active, ERRORS);
        assert!(h.find.is_none());
    }

    /// Something could not be read, so one match may not be the only one: say so, and do
    /// not go there at once.
    #[test]
    fn a_warning_is_shown_before_landing() {
        let mut h = host();
        looking(&mut h);
        h.apply_find(
            TRACE_ID,
            "30d",
            Ok(Some(FindResult { matches: vec![found_trace()], warnings: vec!["errors could not be read".into()] })),
        );
        assert!(h.find.is_some());
        assert!(big_screen(&mut h).contains("errors could not be read"));
        h.on_event(&key(KeyCode::Enter));
        assert_eq!(h.active, TELEMETRY);
    }

    /// A failed read is not "nothing found" (issue 0143).
    #[test]
    fn not_found_and_a_failure_read_differently() {
        let mut h = host();
        looking(&mut h);
        h.apply_find(TRACE_ID, "30d", Ok(None));
        let s = big_screen(&mut h);
        assert!(s.contains("Nothing found") && s.contains("last 30d"), "{s}");
        h.on_event(&key(KeyCode::Enter));
        h.apply_find(TRACE_ID, "30d", Err("server is busy".into()));
        let s = big_screen(&mut h);
        assert!(s.contains("Could not look this up: server is busy") && !s.contains("Nothing found"), "{s}");
    }

    #[test]
    fn an_answer_for_older_text_is_dropped() {
        let mut h = host();
        looking(&mut h);
        h.on_event(&key(KeyCode::Backspace));
        h.apply_find(TRACE_ID, "30d", Ok(Some(FindResult { matches: vec![found_trace()], warnings: vec![] })));
        assert_eq!(h.active, ERRORS);
        assert!(h.find.is_some());
    }

    #[test]
    fn a_wrong_length_says_what_an_id_is() {
        let mut h = host();
        h.on_event(&ctrl('k'));
        h.on_event(&key(KeyCode::Char('a')));
        h.on_event(&key(KeyCode::Enter));
        assert!(big_screen(&mut h).contains("32 hex characters"), "{}", big_screen(&mut h));
    }

    /// While it is open the tab gets no keys; the keys of a text box work.
    #[test]
    fn find_holds_the_keys_until_it_closes() {
        let mut h = host();
        h.on_event(&ctrl('k'));
        h.on_event(&key(KeyCode::Char('x')));
        assert_eq!(h.active, ERRORS, "x is no jump here, and no text: not hex");
        h.on_event(&key(KeyCode::Char('a')));
        h.on_event(&ctrl('u'));
        assert_eq!(h.find.as_ref().unwrap().input, "");
        h.on_event(&ctrl('a'));
        assert_eq!(h.find.as_ref().unwrap().input, "", "a modified key is no text");
        h.on_event(&key(KeyCode::F(2)));
        assert_eq!(h.active, ERRORS);
        h.on_event(&key(KeyCode::Esc));
        assert!(h.find.is_none());
        h.on_event(&ctrl('k'));
        h.on_event(&ctrl('k'));
        assert!(h.find.is_none(), "the same key closes it");
        h.on_event(&ctrl('k'));
        assert!(!h.on_event(&ctrl('c')), "ctrl-c quits from here too");
    }

    /// Ctrl-p is no picker: only plain keys are the host's.
    #[test]
    fn host_keys_are_plain_keys() {
        let (mut h, seen, _) = watched();
        h.on_event(&Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)));
        h.on_event(&Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT)));
        assert!(h.shared.picker.is_none() && seen.lock().unwrap().is_empty());
    }

    /// An event of another project opened on the errors tab: the picker says so.
    #[test]
    fn the_picker_follows_the_errors_tab() {
        let (mut h, seen, project) = watched();
        h.apply_apps("1h", Ok(apps()));
        seen.lock().unwrap().clear();
        *project.lock().unwrap() = Some("web".into());
        h.tick().unwrap();
        assert_eq!(h.shared.label(), "web");
        assert!(seen.lock().unwrap().iter().any(|s| s.starts_with("Telemetry scope web")), "{seen:?}");
    }

    #[test]
    fn a_failed_read_leaves_the_choice_alone() {
        let (mut h, seen, _) = watched();
        h.apply_apps("1h", Ok(apps()));
        h.on_event(&key(KeyCode::Char('p')));
        h.on_event(&key(KeyCode::Down));
        h.on_event(&key(KeyCode::Enter));
        seen.lock().unwrap().clear();
        h.apply_apps("1h", Err("cannot reach".into()));
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(h.shared.label(), "api");
        h.apply_apps("24h", Ok(AppList { apps: vec![], warnings: vec![] }));
        assert_eq!(h.shared.apps.len(), 3, "an answer for another window is ignored");
    }

    #[tokio::test]
    async fn library_panes_draw_inside_their_area() {
        let c = Client::new("http://127.0.0.1:9", None).unwrap();
        let mut h = host_of(&c);
        h.tabs[0].tick();
        h.tabs[1].tick();
        for tab in [ERRORS, TELEMETRY] {
            h.active = tab;
            let s = screen(&mut h);
            assert!(s.starts_with(" mallard "), "top bar stays on tab {tab}: {s:?}");
        }
    }

    /// Review 2.O: one key map. `?` on either tab lists the same shared
    /// keys, both tabs use the one theme, and the bar stays on top.
    #[tokio::test]
    async fn help_on_both_tabs_lists_the_same_shared_keys() {
        let c = Client::new("http://127.0.0.1:9", None).unwrap();
        let mut h = host_of(&c);
        for tab in [KeyCode::F(1), KeyCode::F(2)] {
            h.on_event(&key(tab));
            h.on_event(&key(KeyCode::Char('?')));
            let mut t = Terminal::new(TestBackend::new(140, 50)).unwrap();
            t.draw(|f| h.draw(f)).unwrap();
            let s: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            for (_, what) in mallard_term::tui::keys::SHARED {
                assert!(s.contains(what), "{tab:?}: missing {what:?}");
            }
            assert!(s.contains("All times are UTC."), "{tab:?}");
            h.on_event(&key(KeyCode::Esc));
        }
    }
}
