//! What `mallard tui` holds for all its tabs: the chosen app and the time window, and
//! the picker. No I/O: the list comes in through [`Shared::loaded`], from the host.

use crossterm::event::KeyCode;
use mallard_client::mallard_api::{App, AppList};
use mallard_term::text::{clean, truncate};
use mallard_term::tui::{Scope, centered, rounded, theme, window_presets};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::{Line, Span},
    widgets::{Cell, Clear, Row, Table, TableState},
};

/// Telemetry's and the errors tab's default were 1h and 24h; one window starts at 1h.
const DEFAULT_WINDOW: &str = "1h";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Selected {
    All,
    App(String),
    /// A project or app by slug or id (`-p`, `--app`, an opened event), until the list
    /// says which app that is. The tabs are left alone: `-p` works without the list.
    Wanted(String),
}

pub(super) struct Shared {
    pub apps: Vec<App>,
    loaded: bool,
    pub warnings: Vec<String>,
    /// The last read of the list failed.
    pub error: Option<String>,
    pub selected: Selected,
    /// The picker is open on this row; row 0 is "All apps".
    pub picker: Option<usize>,
    windows: Vec<(&'static str, u64)>,
    win: usize,
    limit: Option<u64>,
}

/// What a key did in the picker.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PickerKey {
    /// Nothing the tabs care about.
    Stay,
    /// Another choice: scope the tabs.
    Chose,
}

impl Shared {
    pub fn new(wanted: Option<String>) -> Self {
        let windows = window_presets(None);
        let win = windows.iter().position(|(n, _)| *n == DEFAULT_WINDOW).unwrap_or(0);
        Shared {
            apps: vec![],
            loaded: false,
            warnings: vec![],
            error: None,
            selected: wanted.map_or(Selected::All, Selected::Wanted),
            picker: None,
            windows,
            win,
            limit: None,
        }
    }

    pub fn since(&self) -> &'static str {
        self.windows[self.win].0
    }

    /// What the tabs should show; `None` while a wanted app is not known.
    pub fn scope(&self) -> Option<Scope> {
        match &self.selected {
            Selected::All => Some(Scope::All),
            Selected::Wanted(_) => None,
            Selected::App(slug) => {
                // Not listed (any more): an app with only services, none in this window.
                let app = self.apps.iter().find(|a| a.slug == *slug);
                Some(Scope::App {
                    slug: slug.clone(),
                    project: app.filter(|a| a.project.is_some()).map(|a| a.slug.clone()),
                    services: app.map_or_else(Vec::new, |a| a.services.clone()),
                })
            }
        }
    }

    /// The list was read.
    pub fn loaded(&mut self, list: AppList) {
        self.apps = list.apps;
        self.warnings = list.warnings;
        self.error = None;
        self.loaded = true;
        if let Selected::Wanted(w) = &self.selected
            && let Some(slug) = self.slug_of(w)
        {
            self.selected = Selected::App(slug);
        }
        if let Some(row) = self.picker {
            self.picker = Some(row.min(self.apps.len()));
        }
    }

    fn slug_of(&self, project: &str) -> Option<String> {
        self.apps
            .iter()
            .find(|a| a.slug == project || a.project.is_some_and(|id| id.to_string() == project))
            .map(|a| a.slug.clone())
    }

    /// A tab went to this project by itself (an event of another project): the picker
    /// follows, so that it never disagrees with what is shown. True when the choice changed.
    pub fn follow(&mut self, project: &str) -> bool {
        let next = match self.slug_of(project) {
            Some(slug) => Selected::App(slug),
            None if !self.loaded => Selected::Wanted(project.to_string()),
            None => return false,
        };
        let changed = next != self.selected && !matches!(next, Selected::Wanted(_));
        if changed || !self.loaded {
            self.selected = next;
        }
        changed
    }

    /// The largest windows the servers allow are cut when they report it. True when the
    /// current window had to change.
    pub fn set_limit(&mut self, limit: Option<u64>) -> bool {
        if limit == self.limit {
            return false;
        }
        self.limit = limit;
        let current = self.windows[self.win];
        self.windows = window_presets(limit);
        match self.windows.iter().position(|w| *w == current) {
            Some(i) => {
                self.win = i;
                false
            }
            None => {
                self.win = self.windows.len() - 1;
                true
            }
        }
    }

    pub fn cycle_window(&mut self, forward: bool) {
        let n = self.windows.len();
        self.win = if forward { (self.win + 1) % n } else { (self.win + n - 1) % n };
    }

    /// The window `secs` long, or the longest one under it. True when it changed.
    pub fn window_at_most(&mut self, secs: u64) -> bool {
        let i = self.windows.iter().rposition(|(_, s)| *s <= secs).unwrap_or(0);
        let changed = i != self.win;
        self.win = i;
        changed
    }

    /// The word in the top bar.
    pub fn label(&self) -> String {
        match &self.selected {
            Selected::All => "All apps".to_string(),
            Selected::App(slug) | Selected::Wanted(slug) => clean(slug).into_owned(),
        }
    }

    pub fn open_picker(&mut self) {
        let row = match &self.selected {
            Selected::App(slug) | Selected::Wanted(slug) => {
                self.apps.iter().position(|a| a.slug == *slug).map_or(0, |i| i + 1)
            }
            Selected::All => 0,
        };
        self.picker = Some(row);
    }

    pub fn picker_key(&mut self, code: KeyCode) -> PickerKey {
        let Some(row) = self.picker else { return PickerKey::Stay };
        let last = self.apps.len();
        match code {
            KeyCode::Esc | KeyCode::Char('q' | 'p') => self.picker = None,
            KeyCode::Down | KeyCode::Char('j') => self.picker = Some((row + 1).min(last)),
            KeyCode::Up | KeyCode::Char('k') => self.picker = Some(row.saturating_sub(1)),
            KeyCode::PageDown => self.picker = Some((row + 10).min(last)),
            KeyCode::PageUp => self.picker = Some(row.saturating_sub(10)),
            KeyCode::Home | KeyCode::Char('g') => self.picker = Some(0),
            KeyCode::End | KeyCode::Char('G') => self.picker = Some(last),
            KeyCode::Enter => {
                self.picker = None;
                self.selected = match row.checked_sub(1).and_then(|i| self.apps.get(i)) {
                    Some(a) => Selected::App(a.slug.clone()),
                    None => Selected::All,
                };
                return PickerKey::Chose;
            }
            _ => {}
        }
        PickerKey::Stay
    }

    /// The picker over `area`: which side each app has, so a missing one is no surprise.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let Some(row) = self.picker else { return };
        let note = self.error.iter().chain(&self.warnings).next().map(|w| clean(w).into_owned());
        let height = (self.apps.len() as u16).saturating_add(3 + u16::from(note.is_some())).clamp(5, area.height);
        let r = centered(area, 76, height);
        f.render_widget(Clear, r);
        let mut block = rounded().title(Span::styled(" Apps ", theme::title()));
        block = block.title_bottom(Line::from(Span::styled(" ⏎ choose · esc close ", theme::faint())).right_aligned());
        if !self.loaded && self.error.is_none() {
            block = block.title_bottom(Line::from(Span::styled(" loading… ", theme::faint())));
        }
        let inner = block.inner(r);
        f.render_widget(block, r);
        let mark = |on: bool| Cell::from(if on { "●" } else { " " }).style(theme::accent());
        let mut rows = vec![Row::new(vec![
            mark(self.selected == Selected::All),
            Cell::from("All apps").style(theme::bold()),
            Cell::from("every service; errors need an app").style(theme::dim()),
        ])];
        rows.extend(self.apps.iter().map(|a| {
            let on = matches!(&self.selected, Selected::App(s) if *s == a.slug);
            let sides = match (a.project.is_some(), a.services.is_empty()) {
                (true, false) => "errors, telemetry".to_string(),
                (true, true) => "errors, no telemetry in this window".to_string(),
                _ => "telemetry, no project".to_string(),
            };
            let name = if a.name == a.slug { String::new() } else { format!("  {}", clean(&a.name)) };
            Row::new(vec![
                mark(on),
                Cell::from(truncate(&format!("{}{name}", clean(&a.slug)), 34)).style(theme::text()),
                Cell::from(sides).style(theme::dim()),
            ])
        }));
        let mut state = TableState::default().with_selected(Some(row));
        let mut body = inner;
        if let Some(note) = note {
            body.height = body.height.saturating_sub(1);
            let line = Rect { y: inner.bottom().saturating_sub(1), height: 1.min(inner.height), ..inner };
            let text = truncate(&note, line.width as usize);
            f.render_widget(Span::styled(text, theme::faint()), line);
        }
        let table = Table::new(rows, [Constraint::Length(1), Constraint::Length(34), Constraint::Min(10)])
            .row_highlight_style(theme::selected());
        f.render_stateful_widget(table, body, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(slug: &str, project: Option<u32>, services: &[&str]) -> App {
        App {
            slug: slug.into(),
            name: slug.into(),
            project,
            services: services.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn list() -> AppList {
        AppList {
            apps: vec![
                app("checkout", Some(1), &["Checkout", "checkout"]),
                app("web", Some(2), &[]),
                app("worker", None, &["worker"]),
            ],
            warnings: vec![],
        }
    }

    #[test]
    fn all_apps_is_the_default_and_the_first_row() {
        let mut s = Shared::new(None);
        assert_eq!(s.scope(), Some(Scope::All));
        assert_eq!(s.label(), "All apps");
        s.loaded(list());
        s.open_picker();
        assert_eq!(s.picker, Some(0));
        assert_eq!(s.picker_key(KeyCode::Down), PickerKey::Stay);
        assert_eq!(s.picker_key(KeyCode::Enter), PickerKey::Chose);
        assert_eq!(s.picker, None);
        assert_eq!(
            s.scope(),
            Some(Scope::App {
                slug: "checkout".into(),
                project: Some("checkout".into()),
                services: vec!["Checkout".into(), "checkout".into()],
            })
        );
        s.open_picker();
        assert_eq!(s.picker, Some(1), "opens on the choice");
        s.picker_key(KeyCode::Char('g'));
        s.picker_key(KeyCode::Enter);
        assert_eq!(s.scope(), Some(Scope::All));
        s.open_picker();
        assert_eq!(s.picker_key(KeyCode::Esc), PickerKey::Stay);
        assert_eq!(s.scope(), Some(Scope::All), "esc changes nothing");
    }

    #[test]
    fn a_project_flag_is_the_first_choice_by_slug_or_id() {
        for wanted in ["web", "2"] {
            let mut s = Shared::new(Some(wanted.into()));
            assert_eq!(s.scope(), None, "the tabs keep -p until the list is read");
            s.loaded(list());
            assert_eq!(s.selected, Selected::App("web".into()));
            let Some(Scope::App { project, services, .. }) = s.scope() else { panic!() };
            assert_eq!((project.as_deref(), services.len()), (Some("web"), 0));
        }
        let mut s = Shared::new(Some("nope".into()));
        s.loaded(list());
        assert_eq!(s.scope(), None, "unknown: the errors tab says so itself");
        assert_eq!(s.label(), "nope");
    }

    #[test]
    fn each_side_of_an_app() {
        let mut s = Shared::new(Some("worker".into()));
        s.loaded(list());
        let Some(Scope::App { project, services, .. }) = s.scope() else { panic!() };
        assert_eq!((project, services.len()), (None, 1));
        // The window got shorter and the app, without a project, left the list.
        s.loaded(AppList { apps: vec![], warnings: vec![] });
        let Some(Scope::App { slug, project, services }) = s.scope() else { panic!() };
        assert_eq!((slug.as_str(), project, services.len()), ("worker", None, 0));
    }

    #[test]
    fn the_picker_follows_a_tab() {
        let mut s = Shared::new(None);
        s.loaded(list());
        assert!(s.follow("web"));
        assert_eq!(s.selected, Selected::App("web".into()));
        assert!(!s.follow("2"), "the same app");
        assert!(!s.follow("gone"));
        assert_eq!(s.selected, Selected::App("web".into()));
        let mut s = Shared::new(None);
        assert!(!s.follow("web"), "not known yet");
        assert_eq!(s.selected, Selected::Wanted("web".into()));
        s.loaded(list());
        assert_eq!(s.selected, Selected::App("web".into()));
    }

    #[test]
    fn one_window_cut_to_the_smaller_limit() {
        let mut s = Shared::new(None);
        assert_eq!(s.since(), "1h", "the default");
        s.cycle_window(false);
        assert_eq!(s.since(), "15m");
        s.cycle_window(true);
        s.cycle_window(true);
        assert_eq!(s.since(), "6h");
        assert!(!s.set_limit(Some(31 * 86_400)));
        assert_eq!(s.windows.last().unwrap().0, "30d");
        assert_eq!(s.since(), "6h", "stays");
        assert!(s.set_limit(Some(3600)), "6h is refused now");
        assert_eq!(s.since(), "1h");
        assert_eq!(s.windows.len(), 3);
        s.cycle_window(true);
        assert_eq!(s.since(), "5m", "wraps inside what is allowed");
    }

    #[test]
    fn the_day_jump_falls_back_inside_the_limit() {
        let mut s = Shared::new(None);
        assert!(s.window_at_most(86_400));
        assert_eq!(s.since(), "24h");
        assert!(!s.window_at_most(86_400));
        let mut s = Shared::new(None);
        s.set_limit(Some(6 * 3600));
        assert!(s.window_at_most(86_400));
        assert_eq!(s.since(), "6h");
    }

    #[test]
    fn server_text_is_cleaned_in_the_picker() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut s = Shared::new(None);
        let mut l = list();
        l.apps[0].slug = "evil\u{202e}x".into();
        l.apps[0].name = "n\x1b[2J".into();
        l.warnings = vec!["telemetry: down\x1b[2J".into()];
        s.loaded(l);
        s.open_picker();
        let mut t = Terminal::new(TestBackend::new(90, 12)).unwrap();
        t.draw(|f| s.render(f, f.area())).unwrap();
        let screen: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(!screen.contains('\u{202e}') && !screen.contains('\x1b'), "{screen}");
        assert!(screen.contains("All apps") && screen.contains("telemetry: down"), "{screen}");
        assert!(screen.contains("telemetry, no project"), "{screen}");
    }
}
