//! `mallard tui`: hosts the TUIs of both libraries as tabs (ADR 0002).
//! F1 errors (sentry-ducklake), F2 telemetry (otel-ducklake). Jumps between
//! them go by trace id.

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures::StreamExt;
use mallard_client::Client;
use mallard_term::tui::{Outcome, theme};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// One library TUI, as the host sees it. Both libraries return the shared
/// [`Outcome`]; `Outcome::Jump(trace)` opens the trace on the other tab.
trait Pane {
    fn title(&self) -> &'static str;
    fn draw(&mut self, f: &mut Frame, area: Rect);
    fn handle(&mut self, ev: &Event) -> Outcome;
    fn tick(&mut self);
    fn open_trace(&mut self, trace_id: &str);
    /// An internal error (a panicked background load) after `tick`.
    fn take_fatal(&mut self) -> Option<String> {
        None
    }
}

impl Pane for sdl_cli::Embedded {
    fn title(&self) -> &'static str {
        "Errors"
    }
    fn draw(&mut self, f: &mut Frame, area: Rect) {
        sdl_cli::Embedded::draw(self, f, area)
    }
    fn handle(&mut self, ev: &Event) -> Outcome {
        self.handle_event(ev)
    }
    fn tick(&mut self) {
        sdl_cli::Embedded::tick(self)
    }
    fn open_trace(&mut self, trace_id: &str) {
        sdl_cli::Embedded::open_trace(self, trace_id)
    }
    fn take_fatal(&mut self) -> Option<String> {
        sdl_cli::Embedded::take_fatal(self)
    }
}

impl Pane for odl_cli::Embedded {
    fn title(&self) -> &'static str {
        "Telemetry"
    }
    fn draw(&mut self, f: &mut Frame, area: Rect) {
        odl_cli::Embedded::draw(self, f, area)
    }
    fn handle(&mut self, ev: &Event) -> Outcome {
        self.handle_event(ev)
    }
    fn tick(&mut self) {
        odl_cli::Embedded::tick(self)
    }
    fn open_trace(&mut self, trace_id: &str) {
        // An invalid id is shown in the tab itself.
        let _ = odl_cli::Embedded::open_trace(self, trace_id);
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
        spans.push(Span::styled(format!("   {}", mallard_term::text::clean(&self.base_url)), theme::faint()));
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
            Outcome::Consumed | Outcome::Ignored => true,
            Outcome::Quit => false,
            Outcome::Jump(t) => {
                // Two tabs: a jump always goes to the other one.
                let other = if self.active == ERRORS { TELEMETRY } else { ERRORS };
                self.jump(other, &t)
            }
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
            p.open_trace(trace_id);
            self.active = tab;
        }
        true
    }
}

/// The otel TUI, with its "e: errors of this trace" jump enabled: the
/// errors tab can answer it.
fn telemetry_pane(client: &Client) -> odl_cli::Embedded {
    let mut pane = odl_cli::Embedded::new(client.otel.clone());
    pane.set_errors_enabled(true);
    pane
}

pub async fn run(client: &Client, project: Option<String>) -> Result<()> {
    // Fail fast with a clear message instead of an empty screen.
    client.sentry.info().await.map_err(|e| anyhow::anyhow!("cannot reach {}: {e}", client.base_url()))?;
    let tabs: Vec<Box<dyn Pane>> = vec![
        Box::new(sdl_cli::Embedded::new(client.sentry.clone(), project).with_hints(crate::ERRORS_HINTS)),
        Box::new(telemetry_pane(client)),
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
        ask_on_x: fn() -> Outcome,
        fatal: Option<String>,
    }
    impl Pane for Fake {
        fn title(&self) -> &'static str {
            self.title
        }
        fn draw(&mut self, f: &mut Frame, area: Rect) {
            f.render_widget(Paragraph::new(format!("{} body", self.title)), area)
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
                Box::new(Fake {
                    opened: vec![],
                    title: "Errors",
                    ask_on_x: || Outcome::Jump("t1".into()),
                    fatal: None,
                }),
                Box::new(Fake {
                    opened: vec![],
                    title: "Telemetry",
                    ask_on_x: || Outcome::Jump("t2".into()),
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
        h.tabs[ERRORS] = Box::new(Fake {
            opened: vec![],
            title: "Errors",
            ask_on_x: || Outcome::Consumed,
            fatal: Some("boom".into()),
        });
        let err = h.tick().unwrap_err().to_string();
        assert_eq!(err, "internal error in the Errors tab: boom");
    }

    #[tokio::test]
    async fn library_panes_draw_inside_their_area() {
        let c = Client::new("http://127.0.0.1:9", None).unwrap();
        let mut h = Host {
            tabs: vec![Box::new(sdl_cli::Embedded::new(c.sentry.clone(), None)), Box::new(telemetry_pane(&c))],
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

    /// Review 2.O: one key map. `?` on either tab lists the same shared
    /// keys, both tabs use the one theme, and the bar stays on top.
    #[tokio::test]
    async fn help_on_both_tabs_lists_the_same_shared_keys() {
        let c = Client::new("http://127.0.0.1:9", None).unwrap();
        let mut h = Host {
            tabs: vec![Box::new(sdl_cli::Embedded::new(c.sentry.clone(), None)), Box::new(telemetry_pane(&c))],
            active: ERRORS,
            base_url: c.base_url().into(),
        };
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
