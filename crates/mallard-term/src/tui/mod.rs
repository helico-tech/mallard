//! What both TUIs share: the theme, a few widgets, the keys that mean the
//! same on every tab, and the [`Outcome`] a component returns to its host.

pub mod suggest;
pub mod theme;

use ratatui::{
    layout::Rect,
    text::Span,
    widgets::{Block, BorderType, Borders},
};

/// What the host should do after a component handled an event.
///
/// Not `#[non_exhaustive]`: the hosts live in this workspace, and their
/// exhaustive `match` makes the compiler point at every place that must
/// handle a new variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Outcome {
    /// The component used the event.
    Consumed,
    /// Not used; the host may handle it.
    Ignored,
    /// The user asked to quit.
    Quit,
    /// The user pressed [`keys::JUMP`]: show this trace id on the other
    /// side (errors ↔ telemetry).
    Jump(String),
    /// [`keys::JUMP`] on a span with an error event: show this event of
    /// this trace on the errors side.
    JumpEvent { trace_id: String, event_id: String },
}

/// What the host's app picker has chosen; each tab scopes itself to its side of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Scope {
    /// Telemetry shows every service; errors, per project, ask for an app (ADR 0028).
    #[default]
    All,
    App {
        slug: String,
        /// The project's slug. `None`: the app has no errors side.
        project: Option<String>,
        /// The service names with data in the window. Empty: no telemetry side.
        services: Vec<String>,
    },
}

/// The windows of one time range across tabs: the union of what each tab had. The web UI
/// has the same list (story 0004/04), so keep them alike.
pub const WINDOW_PRESETS: [(&str, u64); 8] = [
    ("5m", 300),
    ("15m", 900),
    ("1h", 3600),
    ("6h", 6 * 3600),
    ("24h", 86_400),
    ("7d", 7 * 86_400),
    ("14d", 14 * 86_400),
    ("30d", 30 * 86_400),
];

/// The presets no side refuses: `limit` is the smallest `max_window_secs` reported. Never
/// empty, so there is always a window to show.
pub fn window_presets(limit: Option<u64>) -> Vec<(&'static str, u64)> {
    let fits: Vec<_> = WINDOW_PRESETS.iter().copied().filter(|(_, s)| limit.is_none_or(|l| *s <= l)).collect();
    if fits.is_empty() { WINDOW_PRESETS[..1].to_vec() } else { fits }
}

/// Keys that mean the same on every tab and every screen. Each component
/// uses these constants in its key handler and shows [`keys::SHARED`] at the
/// top of its help. Keys not listed here belong to one screen (they are in
/// that screen's hints and help). F1 and F2 belong to the host.
pub mod keys {
    pub const HELP: char = '?';
    pub const REFRESH: char = 'r';
    pub const WINDOW_NEXT: char = 't';
    pub const WINDOW_PREV: char = 'T';
    pub const SEARCH: char = '/';
    pub const JUMP: char = 'x';
    /// The app picker, on every screen. The tabs on their own have no picker (the errors
    /// tab has a list of projects under this key), so they leave it out of their help.
    pub const APP_PICKER: char = 'p';

    /// The shared keys as (keys, what they do), for help screens and docs.
    pub const SHARED: &[(&str, &str)] = &[
        ("↑↓ j k  pgup pgdn  g G", "move"),
        ("↵", "open"),
        ("esc q", "back (q on top: quit)"),
        ("tab  1-9", "tabs of this screen"),
        ("/", "filter or search"),
        ("t T", "next / prev time window"),
        ("p", "choose the app"),
        ("r", "refresh now"),
        ("x", "same trace, other tab"),
        ("?", "help"),
        ("ctrl-c", "quit"),
    ];

    /// Shown under the keys in help screens.
    pub const TIMES: &str = "All times are UTC.";
}

/// A `w`×`h` rectangle in the middle of `area` (clipped to it).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// A block with rounded borders in the border colour.
pub fn rounded() -> Block<'static> {
    Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(theme::border())
}

/// Footer hints: `k desc  k desc`.
pub fn hints(pairs: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut v = vec![Span::raw(" ")];
    for (k, d) in pairs {
        v.push(Span::styled(k.to_string(), theme::key()));
        v.push(Span::styled(format!(" {d}  "), theme::dim()));
    }
    v
}

/// The frame of a loading spinner for a counter that goes up each tick.
pub fn spinner(tick: usize) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[tick % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_keys_are_listed_once() {
        let mut seen = std::collections::HashSet::new();
        for (k, _) in keys::SHARED {
            for part in k.split_whitespace() {
                assert!(seen.insert(part), "{part} is listed twice");
            }
        }
        for c in [
            keys::HELP,
            keys::REFRESH,
            keys::WINDOW_NEXT,
            keys::WINDOW_PREV,
            keys::SEARCH,
            keys::JUMP,
            keys::APP_PICKER,
        ] {
            assert!(seen.contains(c.to_string().as_str()), "{c} is not in SHARED");
        }
    }

    #[test]
    fn presets_are_cut_to_the_smaller_limit() {
        assert_eq!(window_presets(None).len(), 8);
        let names = |l| window_presets(Some(l)).iter().map(|(n, _)| *n).collect::<Vec<_>>();
        assert_eq!(names(31 * 86_400).last(), Some(&"30d"));
        assert_eq!(names(7 * 86_400).last(), Some(&"7d"));
        assert_eq!(names(3600), ["5m", "15m", "1h"]);
        assert_eq!(names(60), ["5m"], "never empty");
    }

    #[test]
    fn centered_stays_inside() {
        let a = Rect::new(10, 5, 20, 10);
        assert_eq!(centered(a, 10, 4), Rect::new(15, 8, 10, 4));
        assert_eq!(centered(a, 50, 50), a);
    }
}
