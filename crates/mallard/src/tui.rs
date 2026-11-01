//! `mallard tui`: hosts the TUIs of both libraries as tabs (ADR 0002).
//! F1 errors (sentry-ducklake), F2 telemetry (otel-ducklake). Jumps between
//! them go by trace id.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures::StreamExt;
use mallard_client::Client;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// What a hosted tab asked for.
enum Ask {
    Nothing,
    Quit,
    /// Show this trace in the telemetry tab.
    Trace(String),
    /// Show this trace's Sentry errors in the errors tab.
    Errors(String),
}

/// One library TUI, as the host sees it.
trait Pane {
    fn title(&self) -> &'static str;
    fn draw(&mut self, f: &mut Frame, area: Rect);
    fn handle(&mut self, ev: &Event) -> Option<Ask>; // None = ignored
    fn tick(&mut self);
    fn open(&mut self, trace_id: &str);
    /// An internal error (a panicked background load) after `tick`.
    fn take_fatal(&mut self) -> Option<String> {
        None
    }
}

struct Errors(sdl_cli::Embedded);

impl Pane for Errors {
    fn title(&self) -> &'static str {
        "Errors"
    }
    fn draw(&mut self, f: &mut Frame, area: Rect) {
        self.0.draw(f, area)
    }
    fn handle(&mut self, ev: &Event) -> Option<Ask> {
        match self.0.handle_event(ev) {
            sdl_cli::Outcome::Consumed => Some(Ask::Nothing),
            sdl_cli::Outcome::Ignored => None,
            sdl_cli::Outcome::Quit => Some(Ask::Quit),
            sdl_cli::Outcome::OpenTrace(t) => Some(Ask::Trace(t)),
            _ => Some(Ask::Nothing),
        }
    }
    fn tick(&mut self) {
        self.0.tick()
    }
    fn open(&mut self, trace_id: &str) {
        self.0.show_trace_errors(trace_id)
    }
    fn take_fatal(&mut self) -> Option<String> {
        self.0.take_fatal()
    }
}

struct Telemetry(odl_cli::tui::Embedded);

impl Pane for Telemetry {
    fn title(&self) -> &'static str {
        "Telemetry"
    }
    fn draw(&mut self, f: &mut Frame, area: Rect) {
        self.0.draw(f, area)
    }
    fn handle(&mut self, ev: &Event) -> Option<Ask> {
        use odl_cli::tui::Outcome;
        match self.0.handle_event(ev) {
            Outcome::Consumed => Some(Ask::Nothing),
            Outcome::Ignored => None,
            Outcome::Quit => Some(Ask::Quit),
            Outcome::OpenErrors(t) => Some(Ask::Errors(t)),
        }
    }
    fn tick(&mut self) {
        self.0.tick()
    }
    fn open(&mut self, trace_id: &str) {
        self.0.open_trace(trace_id)
    }
}

struct Host {
    tabs: Vec<Box<dyn Pane>>,
    active: usize,
    base_url: String,
}

const ERRORS: usize = 0;
const TELEMETRY: usize = 1;

impl Host {
    fn draw(&mut self, f: &mut Frame) {
        let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(f.area());
        let mut spans = vec![Span::styled(" mallard ", Style::new().fg(Color::Black).bg(Color::Indexed(141)))];
        for (i, t) in self.tabs.iter().enumerate() {
            let label = format!(" F{} {} ", i + 1, t.title());
            let style = if i == self.active {
                Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::new().fg(Color::DarkGray)
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(label, style));
        }
        spans.push(Span::styled(format!("   {}", self.base_url), Style::new().fg(Color::DarkGray)));
        f.render_widget(Paragraph::new(Line::from(spans)), bar);
        self.tabs[self.active].draw(f, body);
    }

    /// Returns false to quit.
    fn on_event(&mut self, ev: &Event) -> bool {
        if let Event::Key(k) = ev
            && k.kind != KeyEventKind::Release
            && let KeyCode::F(n) = k.code
            && (1..=self.tabs.len() as u8).contains(&n)
        {
            self.active = n as usize - 1;
            return true;
        }
        match self.tabs[self.active].handle(ev) {
            None | Some(Ask::Nothing) => true,
            Some(Ask::Quit) => false,
            Some(Ask::Trace(t)) => self.jump(TELEMETRY, &t),
            Some(Ask::Errors(t)) => self.jump(ERRORS, &t),
        }
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
        Ok(())
    }

    fn jump(&mut self, tab: usize, trace_id: &str) -> bool {
        if let Some(p) = self.tabs.get_mut(tab) {
            p.open(trace_id);
            self.active = tab;
        }
        true
    }
}

/// The otel TUI, with its "e: errors of this trace" jump enabled: the
/// errors tab can answer it.
fn telemetry_pane(client: &Client) -> odl_cli::tui::Embedded {
    let mut pane = odl_cli::tui::Embedded::new(client.otel.clone());
    pane.set_errors_enabled(true);
    pane
}

pub async fn run(client: &Client, project: Option<String>) -> Result<()> {
    // Fail fast with a clear message instead of an empty screen.
    client.sentry.info().await.map_err(|e| anyhow::anyhow!("cannot reach {}: {e}", client.base_url()))?;
    let tabs: Vec<Box<dyn Pane>> = vec![
        Box::new(Errors(sdl_cli::Embedded::new(client.sentry.clone(), project).with_hints(crate::ERRORS_HINTS))),
        Box::new(Telemetry(telemetry_pane(client))),
    ];
    let mut host = Host { tabs, active: ERRORS, base_url: client.base_url().to_string() };

    let mut term = ratatui::init();
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
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    struct Fake {
        opened: Vec<String>,
        title: &'static str,
        ask_on_x: fn() -> Ask,
        fatal: Option<String>,
    }
    impl Pane for Fake {
        fn title(&self) -> &'static str {
            self.title
        }
        fn draw(&mut self, f: &mut Frame, area: Rect) {
            f.render_widget(Paragraph::new(format!("{} body", self.title)), area)
        }
        fn handle(&mut self, ev: &Event) -> Option<Ask> {
            match ev {
                Event::Key(k) if k.code == KeyCode::Char('x') => Some((self.ask_on_x)()),
                Event::Key(k) if k.code == KeyCode::Char('q') => Some(Ask::Quit),
                _ => None,
            }
        }
        fn tick(&mut self) {}
        fn open(&mut self, trace_id: &str) {
            self.opened.push(trace_id.into())
        }
        fn take_fatal(&mut self) -> Option<String> {
            self.fatal.take()
        }
    }

    fn key(c: KeyCode) -> Event {
        Event::Key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    fn host() -> Host {
        Host {
            tabs: vec![
                Box::new(Fake { opened: vec![], title: "Errors", ask_on_x: || Ask::Trace("t1".into()), fatal: None }),
                Box::new(Fake {
                    opened: vec![],
                    title: "Telemetry",
                    ask_on_x: || Ask::Errors("t2".into()),
                    fatal: None,
                }),
            ],
            active: ERRORS,
            base_url: "http://x".into(),
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

    #[test]
    fn an_internal_error_in_a_tab_ends_the_tui() {
        let mut h = host();
        assert!(h.tick().is_ok());
        h.tabs[ERRORS] =
            Box::new(Fake { opened: vec![], title: "Errors", ask_on_x: || Ask::Nothing, fatal: Some("boom".into()) });
        let err = h.tick().unwrap_err().to_string();
        assert_eq!(err, "internal error in the Errors tab: boom");
    }

    #[tokio::test]
    async fn library_panes_draw_inside_their_area() {
        let c = Client::new("http://127.0.0.1:9", None).unwrap();
        let mut h = Host {
            tabs: vec![
                Box::new(Errors(sdl_cli::Embedded::new(c.sentry.clone(), None))),
                Box::new(Telemetry(telemetry_pane(&c))),
            ],
            active: ERRORS,
            base_url: c.base_url().into(),
        };
        h.tabs[0].tick();
        h.tabs[1].tick();
        for tab in [ERRORS, TELEMETRY] {
            h.active = tab;
            let s = screen(&mut h);
            assert!(s.starts_with(" mallard "), "top bar stays on tab {tab}: {s:?}");
        }
    }
}
