//! `ctrl-k` in `mallard tui`: paste an id and land on it, as Cmd-K in the web UI. No I/O:
//! the host does the lookup and brings the answer to [`Find::answered`].

use crossterm::event::KeyCode;
use mallard_client::mallard_api::{FindId, FindMatch, FindResult};
use mallard_term::fmt::compact;
use mallard_term::text::{clean, truncate};
use mallard_term::tui::{centered, rounded, theme};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

/// What a key did.
#[derive(Debug, PartialEq)]
pub(super) enum FindKey {
    Stay,
    Close,
    /// Look up this id: lowercase hex, no dashes.
    Look(String),
    Land(Box<FindMatch>),
}

#[derive(Debug, PartialEq)]
enum State {
    Typing,
    /// Why the last Enter or lookup gave nothing to open.
    Said(String, Style),
    Looking(String),
    Found {
        matches: Vec<FindMatch>,
        warnings: Vec<String>,
        row: usize,
    },
}

pub(super) struct Find {
    pub input: String,
    state: State,
}

impl Find {
    pub fn new() -> Self {
        Find { input: String::new(), state: State::Typing }
    }

    pub fn clear(&mut self) {
        self.input.clear();
        self.state = State::Typing;
    }

    pub fn key(&mut self, code: KeyCode) -> FindKey {
        match code {
            KeyCode::Esc => return FindKey::Close,
            KeyCode::Enter => return self.enter(),
            KeyCode::Up | KeyCode::Down => {
                if let State::Found { matches, row, .. } = &mut self.state {
                    let last = matches.len() - 1;
                    *row = if code == KeyCode::Up { row.saturating_sub(1) } else { (*row + 1).min(last) };
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.state = State::Typing;
            }
            // Only what an id can hold, so what shows here is always plain text.
            KeyCode::Char(c) if c.is_ascii_hexdigit() || c == '-' => {
                self.input.push(c);
                self.state = State::Typing;
            }
            _ => {}
        }
        FindKey::Stay
    }

    fn enter(&mut self) -> FindKey {
        if let State::Found { matches, row, .. } = &self.state {
            return FindKey::Land(Box::new(matches[*row].clone()));
        }
        match FindId::parse(&self.input) {
            Some(FindId::Hex32(id) | FindId::Hex16(id)) => {
                self.state = State::Looking(id.clone());
                FindKey::Look(id)
            }
            None => {
                let why = "a trace or event id has 32 hex characters, an issue or span id 16".to_string();
                self.state = State::Said(why, theme::error());
                FindKey::Stay
            }
        }
    }

    /// The answer for `id`, unless the text has changed since. Returns what to land on:
    /// one match, and nothing that could not be read, so there is no other to choose.
    /// A failed read is never "nothing found" (issue 0143).
    pub fn answered(&mut self, id: &str, since: &str, answer: Result<Option<FindResult>, String>) -> Option<FindMatch> {
        if !matches!(&self.state, State::Looking(looking) if looking == id) {
            return None;
        }
        match answer {
            Ok(Some(mut found)) if found.matches.len() == 1 && found.warnings.is_empty() => found.matches.pop(),
            Ok(Some(found)) => {
                self.state = State::Found { matches: found.matches, warnings: found.warnings, row: 0 };
                None
            }
            Ok(None) => {
                self.state = State::Said(format!("Nothing found for {id} in the last {since}."), theme::dim());
                None
            }
            Err(e) => {
                self.state = State::Said(format!("Could not look this up: {}", clean(&e)), theme::error());
                None
            }
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let width = 76.min(area.width);
        let room = usize::from(width.saturating_sub(4));
        let mut lines = vec![self.input_line(room), Line::raw("")];
        match &self.state {
            State::Typing => {}
            State::Said(text, style) => lines.push(Line::from(Span::styled(truncate(text, room), *style))),
            State::Looking(_) => lines.push(Line::from(Span::styled("looking…", theme::faint()))),
            State::Found { matches, warnings, row } => {
                lines.extend(matches.iter().enumerate().map(|(i, m)| match_line(m, i == *row, room)));
                lines.extend(
                    warnings.iter().map(|w| Line::from(Span::styled(truncate(&clean(w), room), theme::error()))),
                );
            }
        }
        let hint = if matches!(self.state, State::Found { .. }) {
            " ⏎ open · ↑↓ choose · esc close "
        } else {
            " ⏎ look up · esc close "
        };
        let r = centered(area, width, (lines.len() as u16).saturating_add(2));
        let block = rounded()
            .title(Span::styled(" Find an id ", theme::title()))
            .title_bottom(Line::from(Span::styled(hint, theme::faint())).right_aligned());
        f.render_widget(Clear, r);
        f.render_widget(Paragraph::new(lines).block(block), r);
    }

    fn input_line(&self, room: usize) -> Line<'static> {
        if self.input.is_empty() {
            return Line::from(Span::styled("paste a trace, event, issue or span id", theme::faint()));
        }
        // The end stays in view, where the typing happens.
        let skip = self.input.len().saturating_sub(room.saturating_sub(1));
        Line::from(vec![
            Span::styled(self.input[skip..].to_string(), theme::text()),
            Span::styled("▏", theme::accent()),
        ])
    }
}

fn match_line(m: &FindMatch, selected: bool, room: usize) -> Line<'static> {
    let (title, sub) = match m {
        FindMatch::Trace { trace_id, spans, errors, logs, .. } => (
            format!("Trace {}", clean(trace_id)),
            format!(
                "{} spans, {} errors, {} logs",
                compact(*spans as f64),
                compact(*errors as f64),
                compact(*logs as f64)
            ),
        ),
        FindMatch::Span { name, service, .. } => (clean(name).into_owned(), format!("span in {}", clean(service))),
        FindMatch::Event { title, project, .. } => (clean(title).into_owned(), format!("event in {}", clean(project))),
        FindMatch::Issue { title, project, status, count, .. } => (
            clean(title).into_owned(),
            format!("issue in {}, {}, {} events", clean(project), clean(status), compact(*count as f64)),
        ),
    };
    let mark = if selected { "› " } else { "  " };
    let room = room.saturating_sub(2);
    let title = truncate(&title, room);
    let left = room.saturating_sub(title.chars().count() + 2);
    let style = if selected { theme::bold() } else { theme::text() };
    Line::from(vec![
        Span::styled(mark, theme::accent()),
        Span::styled(title, style),
        Span::raw("  "),
        Span::styled(truncate(&sub, left), theme::faint()),
    ])
}
