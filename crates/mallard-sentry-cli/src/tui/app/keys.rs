//! Key presses: what they change and which loads they start.

use super::*;
use mallard_term::tui::keys;

/// What a key press did: requests to send, and what the host should do.
type Step = (Vec<Req>, Outcome);

fn done(reqs: Vec<Req>) -> Step {
    (reqs, Outcome::Consumed)
}

fn ignored() -> Step {
    (vec![], Outcome::Ignored)
}

fn is_nav(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('j' | 'k' | 'g' | 'G' | ' ')
    )
}

impl App {
    /// Handle a key press: the requests it starts (already tracked) and
    /// what the host should do.
    pub(crate) fn on_key(&mut self, k: KeyEvent) -> Step {
        let (reqs, outcome) = self.key(k);
        (self.track(reqs), outcome)
    }

    fn key(&mut self, k: KeyEvent) -> Step {
        if matches!(k.code, KeyCode::F(1) | KeyCode::F(2)) {
            // Reserved for an embedding host.
            return ignored();
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            return (vec![], Outcome::Quit);
        }
        // AltGr arrives as ctrl+alt on Windows: let it type `@`, `{`, `\` in the search box.
        let altgr = k.modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT && matches!(k.code, KeyCode::Char(_));
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) && !(altgr && self.input.is_some()) {
            // Only plain keys run actions; ctrl-r must not resolve an issue.
            return match self.input.as_mut() {
                Some(input) if k.modifiers == KeyModifiers::CONTROL && k.code == KeyCode::Char('u') => {
                    input.clear();
                    self.suggest.edited("", 0, Instant::now());
                    done(vec![])
                }
                _ => ignored(),
            };
        }
        match self.overlay {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return done(vec![]);
            }
            Overlay::None => {}
        }
        if self.input.is_some() {
            return done(self.on_key_search(k));
        }
        match k.code {
            KeyCode::Char(keys::HELP) => {
                self.overlay = Overlay::Help;
                return done(vec![]);
            }
            KeyCode::Char(keys::REFRESH) => return done(self.refresh_all()),
            _ => {}
        }
        match self.view() {
            View::Issues => self.on_key_issues(k),
            View::Detail => self.on_key_detail(k),
            View::Trace => self.on_key_trace(k),
        }
    }

    /// Show the error events of a trace (searches all projects). Replaces a
    /// trace view in front; Esc returns to the screen before it.
    pub(crate) fn show_trace(&mut self, trace_id: &str) -> Vec<Req> {
        let t = Screen::Trace(TraceView { trace_id: trace_id.to_string(), ..Default::default() });
        if self.view() == View::Trace {
            self.screen = t;
        } else {
            self.push(t);
        }
        self.overlay = Overlay::None;
        self.input = None;
        self.suggest.close();
        self.track(vec![Req::Trace { trace_id: trace_id.to_string() }])
    }

    /// [`App::show_trace`], then open the event `event_id` when the list
    /// arrives (Esc returns to the list).
    pub(crate) fn show_trace_event(&mut self, trace_id: &str, event_id: &str) -> Vec<Req> {
        let reqs = self.show_trace(trace_id);
        if let Screen::Trace(t) = &mut self.screen {
            t.open_event = Some(event_id.to_string());
        }
        reqs
    }

    fn on_key_trace(&mut self, k: KeyEvent) -> Step {
        let Screen::Trace(t) = &mut self.screen else {
            return ignored();
        };
        if Self::move_selection(&mut t.state, t.events.len(), k.code) {
            return done(vec![]);
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.pop(),
            KeyCode::Enter => {
                if let Some(e) = t.state.selected().and_then(|i| t.events.get(i)).cloned() {
                    return done(self.open_event(&e));
                }
            }
            _ => return ignored(),
        }
        done(vec![])
    }

    /// Open the issue detail of `e` (switching project if needed) showing `e`.
    pub(super) fn open_event(&mut self, e: &EventSummary) -> Vec<Req> {
        let mut reqs = vec![];
        let slug = self
            .projects
            .iter()
            .find(|p| p.id == e.project_id)
            .map(|p| p.slug.clone())
            .unwrap_or_else(|| e.project_id.to_string());
        if self.project.as_deref() != Some(slug.as_str()) && self.project.as_deref() != Some(&e.project_id.to_string())
        {
            self.switch_project(slug.clone());
            reqs.extend(self.list_requests());
        }
        let project = self.project.clone().unwrap_or(slug);
        self.push(Screen::Detail(Box::new(Detail {
            id: e.issue_id.clone(),
            event_id: Some(e.event_id.clone()),
            tab: Some(Tab::Stack),
            ..Default::default()
        })));
        reqs.extend([
            Req::Issue { project: project.clone(), id: e.issue_id.clone(), window: self.window },
            Req::Events { project: project.clone(), issue: e.issue_id.clone(), before: None },
            Req::Event { project, issue: e.issue_id.clone(), event_id: e.event_id.clone() },
        ]);
        reqs
    }

    fn on_key_search(&mut self, k: KeyEvent) -> Vec<Req> {
        let now = Instant::now();
        let Some(input) = self.input.as_mut() else {
            return vec![];
        };
        match k.code {
            KeyCode::Esc => {
                self.input = None;
                self.suggest.close();
            }
            KeyCode::Enter => {
                self.query = self.input.take().unwrap_or_default().trim().to_string();
                self.suggest.close();
                return self.reload_list();
            }
            KeyCode::Tab => {
                if let Some((text, _)) = self.suggest.accept(now) {
                    *input = text;
                }
            }
            KeyCode::Down => self.suggest.select(1),
            KeyCode::Up => self.suggest.select(-1),
            KeyCode::Backspace => {
                input.pop();
                self.suggest.edited(input, input.chars().count(), now);
            }
            KeyCode::Char(c) => {
                input.push(c);
                self.suggest.edited(input, input.chars().count(), now);
            }
            _ => {}
        }
        vec![]
    }

    fn move_selection(state: &mut TableState, len: usize, code: KeyCode) -> bool {
        if len == 0 {
            state.select(None);
            return false;
        }
        let cur = state.selected().unwrap_or(0).min(len - 1);
        let next = match code {
            KeyCode::Down | KeyCode::Char('j') => (cur + 1).min(len - 1),
            KeyCode::Up | KeyCode::Char('k') => cur.saturating_sub(1),
            KeyCode::PageDown => (cur + PAGE).min(len - 1),
            KeyCode::PageUp => cur.saturating_sub(PAGE),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => len - 1,
            _ => return false,
        };
        state.select(Some(next));
        true
    }

    fn on_key_issues(&mut self, k: KeyEvent) -> Step {
        if Self::move_selection(&mut self.issues_state, self.issues.len(), k.code) {
            return done(self.more_issues());
        }
        done(match k.code {
            KeyCode::Char('q') => return (vec![], Outcome::Quit),
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.reload_list()
            }
            KeyCode::Enter => self.open_selected(),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.status = cycle(&StatusFilter::ALL, self.status, 1);
                self.reload_list()
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.status = cycle(&StatusFilter::ALL, self.status, -1);
                self.reload_list()
            }
            KeyCode::Char(c @ '1'..='4') => {
                self.status = StatusFilter::ALL[c as usize - '1' as usize];
                self.reload_list()
            }
            KeyCode::Char(keys::SEARCH) => {
                self.suggest.open(&self.query, self.query.chars().count(), Instant::now());
                self.input = Some(self.query.clone());
                vec![]
            }
            KeyCode::Char('d') if self.offers_day() => return (vec![], Outcome::SetWindow("24h")),
            KeyCode::Char('s') => {
                self.sort = cycle(&SortBy::ALL, self.sort, 1);
                self.reload_list()
            }
            // Writes are capitals, so a refresh (`r`) can never change data.
            KeyCode::Char('R') => self.set_selected_status(IssueStatus::Resolved),
            KeyCode::Char('I') => self.set_selected_status(IssueStatus::Ignored),
            KeyCode::Char('U') => self.set_selected_status(IssueStatus::Unresolved),
            _ => return ignored(),
        })
    }

    /// On the last row of a full list: read more, up to what the server gives.
    fn more_issues(&mut self) -> Vec<Req> {
        let last = self.issues_state.selected() == Some(self.issues.len().saturating_sub(1));
        if !last || (self.issues.len() as u32) < self.issue_limit {
            return vec![];
        }
        if self.issue_limit >= ISSUE_CAP {
            self.toast(format!("The first {ISSUE_CAP} issues: narrow the window or the search"), false);
            return vec![];
        }
        self.issue_limit = (self.issue_limit + ISSUE_PAGE).min(ISSUE_CAP);
        self.issues_key().map(Req::Issues).into_iter().collect()
    }

    /// Open an issue by id, in its project, also from another screen. With an event: that
    /// event, on the stack trace. Esc returns to the list.
    pub(crate) fn show_issue(&mut self, project: &str, issue: &str, event: Option<&str>) -> Vec<Req> {
        self.overlay = Overlay::None;
        self.input = None;
        self.suggest.close();
        let mut reqs = vec![];
        if !self.current_project().is_some_and(|p| p.slug == project) && self.project.as_deref() != Some(project) {
            self.switch_project(project.to_string());
            reqs.extend(self.list_requests());
        }
        self.push(Screen::Detail(Box::new(Detail {
            id: issue.to_string(),
            event_id: event.map(str::to_string),
            tab: event.map(|_| Tab::Stack),
            ..Default::default()
        })));
        reqs.extend([
            Req::Issue { project: project.to_string(), id: issue.to_string(), window: self.window },
            Req::Events { project: project.to_string(), issue: issue.to_string(), before: None },
        ]);
        if let Some(event_id) = event {
            reqs.push(Req::Event {
                project: project.to_string(),
                issue: issue.to_string(),
                event_id: event_id.to_string(),
            });
        }
        self.track(reqs)
    }

    fn open_selected(&mut self) -> Vec<Req> {
        let (Some(issue), Some(project)) = (self.selected_issue().cloned(), self.project.clone()) else {
            return vec![];
        };
        self.push(Screen::Detail(Box::new(Detail {
            id: issue.id.clone(),
            summary: Some(issue.clone()),
            ..Default::default()
        })));
        vec![
            Req::Issue { project: project.clone(), id: issue.id.clone(), window: self.window },
            Req::Events { project, issue: issue.id, before: None },
        ]
    }

    fn set_selected_status(&mut self, status: IssueStatus) -> Vec<Req> {
        let Some(id) = self.selected_issue().map(|i| i.id.clone()) else {
            return vec![];
        };
        self.set_status(id, status)
    }

    fn set_status(&mut self, id: String, status: IssueStatus) -> Vec<Req> {
        let Some(project) = self.project.clone() else {
            return vec![];
        };
        // Optimistic: show the new status right away.
        for i in self.issues.iter_mut().filter(|i| i.id == id) {
            i.status = status;
        }
        for d in self.details_mut().filter(|d| d.id == id) {
            if let Some(s) = d.summary.as_mut() {
                s.status = status;
            }
            if let Some(dd) = d.detail.as_mut() {
                dd.summary.status = status;
            }
        }
        vec![Req::SetStatus { project, id, status }]
    }

    fn on_key_detail(&mut self, k: KeyEvent) -> Step {
        let Screen::Detail(d) = &mut self.screen else {
            return ignored();
        };
        let tab = d.tab();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.pop();
                return done(self.list_requests());
            }
            KeyCode::Char(keys::JUMP) => match d.trace_id() {
                Some(t) => return (vec![], Outcome::Jump(t.to_string())),
                None => self.toast("this event has no trace id", false),
            },
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                d.tab = Some(Tab::ALL[(tab.index() + 1) % Tab::ALL.len()])
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                d.tab = Some(Tab::ALL[(tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()])
            }
            KeyCode::Char(c @ '1'..='5') => d.tab = Some(Tab::ALL[c as usize - '1' as usize]),
            KeyCode::Char('[') => return done(self.step_event(1)),
            KeyCode::Char(']') => return done(self.step_event(-1)),
            KeyCode::Char('a') => {
                d.in_app_only = !d.in_app_only;
                d.frame = d.default_frame();
            }
            KeyCode::Char('w') => {
                if d.has_raw() {
                    d.raw = !d.raw;
                    d.frame = d.default_frame();
                } else {
                    self.toast("this event has no raw (unsymbolicated) frames", false);
                }
            }
            KeyCode::Char(c @ ('R' | 'I' | 'U')) => {
                let status = match c {
                    'R' => IssueStatus::Resolved,
                    'I' => IssueStatus::Ignored,
                    _ => IssueStatus::Unresolved,
                };
                let id = d.id.clone();
                return done(self.set_status(id, status));
            }
            KeyCode::Enter if tab == Tab::Events => {
                if let (Some(i), Some(project)) = (d.events_state.selected(), self.project.clone()) {
                    d.tab = Some(Tab::Stack);
                    return done(d.show(i, project).into_iter().collect());
                }
            }
            code if is_nav(code) => return done(self.move_in_tab(code)),
            _ => return ignored(),
        }
        done(vec![])
    }

    fn move_in_tab(&mut self, code: KeyCode) -> Vec<Req> {
        let project = self.project.clone();
        let events_busy = self.in_flight(Kind::Events);
        let Some(d) = self.detail_mut() else {
            return vec![];
        };
        match d.tab() {
            Tab::Overview | Tab::Json => {
                let max = d.max_scroll;
                d.scroll = match code {
                    KeyCode::Down | KeyCode::Char('j') => (d.scroll + 1).min(max),
                    KeyCode::Up | KeyCode::Char('k') => d.scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => (d.scroll + PAGE * 2).min(max),
                    KeyCode::PageUp => d.scroll.saturating_sub(PAGE * 2),
                    KeyCode::Char('g') | KeyCode::Home => 0,
                    KeyCode::Char('G') | KeyCode::End => max,
                    _ => d.scroll,
                };
            }
            Tab::Stack => {
                let n = d.stack_frames().len();
                if n > 0 {
                    d.frame = match code {
                        KeyCode::Down | KeyCode::Char('j') => (d.frame + 1).min(n - 1),
                        KeyCode::Up | KeyCode::Char('k') => d.frame.saturating_sub(1),
                        KeyCode::PageDown => (d.frame + PAGE).min(n - 1),
                        KeyCode::PageUp => d.frame.saturating_sub(PAGE),
                        KeyCode::Char('g') | KeyCode::Home => 0,
                        KeyCode::Char('G') | KeyCode::End => n - 1,
                        _ => d.frame,
                    };
                }
            }
            Tab::Breadcrumbs => {
                Self::move_selection(&mut d.crumbs_state, d.crumbs.len(), code);
            }
            Tab::Events => {
                let n = d.events.len();
                Self::move_selection(&mut d.events_state, n, code);
                // Reached the end: fetch the next (older) page.
                if n > 0
                    && d.events_state.selected() == Some(n - 1)
                    && !events_busy
                    && let Some(project) = project
                {
                    return d.older_page(project).into_iter().collect();
                }
            }
        }
        vec![]
    }

    /// `delta` 1 = older, -1 = newer.
    fn step_event(&mut self, delta: isize) -> Vec<Req> {
        let Some(project) = self.project.clone() else {
            return vec![];
        };
        let events_busy = self.in_flight(Kind::Events);
        let Some(d) = self.detail_mut() else {
            return vec![];
        };
        let target = d.event_idx as isize + delta;
        if target < 0 {
            self.toast("already at the newest event", false);
            return vec![];
        }
        let target = target as usize;
        if target >= d.events.len() {
            if d.want_older || events_busy {
                // The older page is already on its way.
                return vec![];
            }
            if let Some(r) = d.older_page(project) {
                d.want_older = true;
                return vec![r];
            }
            self.toast("already at the oldest event", false);
            return vec![];
        }
        d.show(target, project).into_iter().collect()
    }
}
