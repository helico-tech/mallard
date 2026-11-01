//! `mallard telemetry tui`: the component in a terminal of its own.

use std::time::Duration;

use anyhow::Result;
use crossterm::event;
use mallard_otel_client::Client;

use super::{Embedded, Outcome};

/// Run the TUI in this terminal until the user quits.
pub async fn run_standalone(client: Client, since: &str) -> Result<()> {
    let mut app = Embedded::with_since(client, since)?;
    // try_init installs a panic hook that restores the terminal.
    let mut terminal = ratatui::try_init()?;
    let res = event_loop(&mut terminal, &mut app).await;
    ratatui::restore();
    res
}

/// Async event loop: terminal events arrive through crossterm's EventStream,
/// so the runtime keeps running background loads (also on a current-thread
/// runtime).
async fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut Embedded) -> Result<()> {
    use futures::StreamExt;
    let mut events = event::EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        terminal.draw(|f| app.draw(f, f.area()))?;
        tokio::select! {
            _ = ticker.tick() => app.tick(),
            ev = events.next() => match ev {
                Some(Ok(ev)) => match app.handle_event(&ev) {
                    Outcome::Quit => return Ok(()),
                    Outcome::Jump(_) | Outcome::JumpEvent { .. } => app.flash_error("no error tracker connected"),
                    _ => {}
                },
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
        }
    }
}
