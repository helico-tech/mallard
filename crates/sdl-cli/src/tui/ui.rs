//! Rendering. Everything draws inside the given `area` only, so a host can
//! embed the TUI in part of its screen.

use super::app::*;
use super::theme::{self as th};
use crate::StatusFilter;
use crate::fmt;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Sparkline, Table, Wrap,
};
use sdl_client::types::IssueStatus;

/// `n` as a terminal coordinate, saturating instead of wrapping.
fn u16_sat(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

pub const MIN_WIDTH: u16 = 50;
pub const MIN_HEIGHT: u16 = 12;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render(f: &mut Frame, area: Rect, app: &mut App) {
    f.render_widget(Clear, area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        too_small(f, area);
        return;
    }
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(area);
    render_header(f, header, app);
    match app.view() {
        View::Issues => render_issues(f, body, app),
        View::Detail => render_detail(f, body, app),
        View::Trace => render_trace(f, body, app),
    }
    render_footer(f, footer, app);
    match app.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(f, area),
        Overlay::Projects { selected } => render_projects(f, area, app, selected),
    }
}

fn too_small(f: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from("Terminal too small".bold().fg(th::YELLOW)),
        Line::from(Span::styled(
            format!("need {MIN_WIDTH}×{MIN_HEIGHT}, have {}×{}", area.width, area.height),
            th::dim(),
        )),
    ]);
    let h = 2.min(area.height);
    let y = area.y + area.height.saturating_sub(h) / 2;
    f.render_widget(
        Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }),
        Rect { y, height: h, ..area },
    );
}

fn rounded<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(th::border()).title(title)
}

fn title_span(s: &str) -> Span<'static> {
    Span::styled(format!(" {s} "), th::bold())
}

/// A load's state in place of its content: the error (red) when it failed,
/// else the loading text.
fn load_state(error: Option<&str>, loading: &str) -> Paragraph<'static> {
    let line = match error {
        Some(e) => Line::from(vec![
            Span::styled(format!("✗ {e}"), Style::new().fg(th::RED)),
            Span::styled("  · R retry", th::dim()),
        ]),
        None => Line::from(Span::styled(loading.to_string(), th::dim())),
    };
    Paragraph::new(line).alignment(Alignment::Center).wrap(Wrap { trim: true })
}

pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn spinner(app: &App) -> &'static str {
    SPINNER[app.spinner % SPINNER.len()]
}

// ---------------------------------------------------------------- header

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(Block::new().style(Style::new().bg(th::PANEL)), area);
    let mut left = vec![Span::styled(" ◆ sentry-ducklake ", th::tab_active()), Span::raw(" ")];
    match app.current_project() {
        Some(p) => {
            left.push(Span::styled(p.name.clone(), th::bold()));
            left.push(Span::styled(
                format!("  {}{}", p.slug, p.platform.as_ref().map(|x| format!(" · {x}")).unwrap_or_default()),
                th::dim(),
            ));
        }
        None => left.push(Span::styled(app.project.clone().unwrap_or_else(|| "no project".into()), th::dim())),
    }
    let view = match app.view() {
        View::Issues => "",
        View::Detail => "  ›  issue",
        View::Trace => "  ›  trace",
    };
    left.push(Span::styled(view, th::faint()));

    let mut right: Vec<Span> = vec![];
    if app.loading() {
        right.push(Span::styled(format!("{} ", spinner(app)), th::accent()));
    }
    if let Some(i) = &app.info {
        let ratio = if i.queue_capacity == 0 { 0.0 } else { i.queue_depth as f64 / i.queue_capacity as f64 };
        let color = if ratio < 0.5 {
            th::GREEN
        } else if ratio < 0.8 {
            th::YELLOW
        } else {
            th::RED
        };
        let cells = 6usize;
        let filled = if i.queue_depth == 0 { 0 } else { ((ratio * cells as f64).ceil() as usize).clamp(1, cells) };
        right.push(Span::styled("queue ", th::dim()));
        right.push(Span::styled("■".repeat(filled), Style::new().fg(color)));
        right.push(Span::styled("·".repeat(cells - filled), th::faint()));
        right.push(Span::styled(
            format!(" {}/{}  ", fmt::compact(i.queue_depth), fmt::compact(i.queue_capacity)),
            th::text(),
        ));
    }
    if app.offline.is_some() {
        right.push(Span::styled("● offline  ", Style::new().fg(th::RED)));
    } else if app.info.is_some() {
        right.push(Span::styled("● live  ", Style::new().fg(th::GREEN)));
    }
    if area.width > 110 {
        right.push(Span::styled(format!("{}  ", app.server_url), th::faint()));
    }
    if let Some(i) = &app.info {
        right.push(Span::styled(format!("v{} ", i.version), th::dim()));
    }
    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
}

// ---------------------------------------------------------------- footer

fn hints(pairs: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut v = vec![Span::raw(" ")];
    for (k, d) in pairs {
        v.push(Span::styled(k.to_string(), th::key()));
        v.push(Span::styled(format!(" {d}  "), th::dim()));
    }
    v
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(input) = &app.input {
        Line::from(vec![
            Span::styled(" / ", th::tab_active()),
            Span::styled(format!(" {input}"), Style::new().fg(th::YELLOW)),
            Span::styled("█", Style::new().fg(th::YELLOW).add_modifier(Modifier::SLOW_BLINK)),
            Span::styled("   enter search · esc cancel · ctrl-u clear", th::dim()),
        ])
    } else if let Some(t) = &app.toast {
        let c = if t.error { th::RED } else { th::GREEN };
        Line::from(Span::styled(format!(" {}", t.text), Style::new().fg(c).add_modifier(Modifier::BOLD)))
    } else if let Some(e) = app.offline.as_ref().or(app.global_error.as_ref()) {
        Line::from(vec![
            Span::styled(format!(" ✗ {e}"), Style::new().fg(th::RED)),
            Span::styled("   R retry", th::dim()),
        ])
    } else {
        let pairs: &[(&str, &str)] = match app.view() {
            View::Issues => &[
                ("↑↓", "move"),
                ("⏎", "open"),
                ("tab", "status"),
                ("/", "search"),
                ("s", "sort"),
                ("t", "window"),
                ("p", "project"),
                ("r/i/u", "resolve/ignore/unresolve"),
                ("?", "help"),
                ("q", "quit"),
            ],
            View::Detail => &[
                ("tab", "section"),
                ("↑↓", "move"),
                ("[ ]", "older/newer event"),
                ("a", "in-app"),
                ("w", "raw"),
                ("t", "trace"),
                ("r/i/u", "status"),
                ("esc", "back"),
                ("?", "help"),
            ],
            View::Trace => &[("↑↓", "move"), ("⏎", "open event"), ("esc", "back"), ("?", "help")],
        };
        Line::from(hints(pairs))
    };
    f.render_widget(Paragraph::new(line), area);
}

// ---------------------------------------------------------------- issues view

fn render_issues(f: &mut Frame, area: Rect, app: &mut App) {
    let stats_h = if area.height >= 22 { 7 } else { 0 };
    let [stats, filters, table] =
        Layout::vertical([Constraint::Length(stats_h), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    if stats_h > 0 {
        render_stats(f, stats, app);
    }
    render_filters(f, filters, app);
    render_issue_table(f, table, app);
}

fn render_stats(f: &mut Frame, area: Rect, app: &App) {
    let window = app.window.since();
    let kpi_w = if area.width >= 100 { 56 } else { area.width };
    let [kpi, chart] = Layout::horizontal([Constraint::Length(kpi_w), Constraint::Min(0)]).areas(area);

    let block = rounded(vec![title_span("Overview"), Span::styled(format!("last {window} "), th::dim())]);
    let inner = block.inner(kpi);
    f.render_widget(block, kpi);
    let cols = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(inner);
    let s = app.stats.as_ref();
    let hours = (fmt::parse_duration(window).unwrap_or(86_400) as f64 / 3600.0).max(1.0);
    let kpis = [
        (
            "EVENTS",
            s.map(|s| s.total_events),
            th::ACCENT2,
            s.map(|s| format!("{:.1}/h", s.total_events as f64 / hours)),
        ),
        ("ISSUES", s.map(|s| s.unique_issues), th::ORANGE, Some("with events".to_string())),
        ("NEW", s.map(|s| s.new_issues), th::PINK, Some("first seen".to_string())),
        ("USERS", s.map(|s| s.affected_users), th::GREEN, Some("affected".to_string())),
    ];
    for (i, (label, value, color, sub)) in kpis.into_iter().enumerate() {
        let lines = vec![
            Line::from(Span::styled(label, th::dim())),
            Line::from(Span::styled(
                value.map(fmt::compact).unwrap_or_else(|| "–".into()),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(sub.unwrap_or_default(), th::faint())),
        ];
        let c = cols[i];
        let y = c.y + c.height.saturating_sub(3) / 2;
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), Rect { y, height: 3.min(c.height), ..c });
    }

    if chart.width < 10 {
        return;
    }
    let events: Vec<u64> = s.map(|s| s.buckets.iter().map(|b| b.events).collect()).unwrap_or_default();
    let peak = events.iter().copied().max().unwrap_or(0);
    let mut title = vec![title_span("Events"), Span::styled(format!("last {window} "), th::dim())];
    if let Some(s) = s {
        title.push(Span::styled(
            format!("· peak {} per {} ", fmt::compact(peak), fmt::duration(s.interval_seconds)),
            th::faint(),
        ));
    }
    let block = rounded(title);
    let inner = block.inner(chart);
    f.render_widget(block, chart);
    if inner.height < 2 {
        return;
    }
    let [plot, axis] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    if s.is_none() {
        let loading = format!("{} loading…", spinner(app));
        f.render_widget(load_state(app.stats_error.as_deref(), &loading).alignment(Alignment::Left), plot);
        return;
    }
    let data = fmt::resample(&events, plot.width as usize);
    f.render_widget(Sparkline::default().data(&data).style(Style::new().fg(th::ACCENT)), plot);
    let axis_line = Line::from(vec![
        Span::styled(format!("-{window}"), th::faint()),
        Span::raw(" ".repeat((axis.width as usize).saturating_sub(window.len() + 1 + 3))),
        Span::styled("now", th::faint()),
    ]);
    f.render_widget(Paragraph::new(axis_line), axis);
}

fn render_filters(f: &mut Frame, area: Rect, app: &App) {
    let mut tabs = vec![Span::raw(" ")];
    for status in StatusFilter::ALL {
        let s = status.as_str();
        let label = format!("{}{}", s[..1].to_uppercase(), &s[1..]);
        if status == app.status {
            let count = if app.issues_loaded { format!(" {}", app.issues.len()) } else { String::new() };
            tabs.push(Span::styled(format!(" {label}{count} "), th::tab_active()));
        } else {
            tabs.push(Span::styled(format!(" {label} "), th::tab_inactive()));
        }
        tabs.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(tabs)), area);

    let mut right = vec![];
    if !app.query.is_empty() {
        right.push(Span::styled("/ ", th::key()));
        right.push(Span::styled(format!("{} ", fmt::truncate(&app.query, 24)), Style::new().fg(th::YELLOW)));
        right.push(Span::styled("(esc clears)  ", th::faint()));
    }
    right.push(Span::styled("sort ", th::faint()));
    right.push(Span::styled(format!("{} ▾  ", app.sort.label()), th::text()));
    right.push(Span::styled("window ", th::faint()));
    right.push(Span::styled(format!("{} ", app.window.since()), th::text()));
    if area.width > 80 {
        f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
    }
}

fn status_span(s: IssueStatus) -> Span<'static> {
    Span::styled(s.as_str().to_string(), Style::new().fg(th::status_color(s.as_str())))
}

fn render_issue_table(f: &mut Frame, area: Rect, app: &mut App) {
    let mut title = vec![title_span("Issues"), Span::styled(format!("{} ", app.status.as_str()), th::dim())];
    if let Some(e) = app.issues_error.as_ref().filter(|_| !app.issues.is_empty()) {
        // The list is still shown; the failed refresh is noted in the title.
        title.push(Span::styled(format!("✗ refresh failed: {} ", fmt::truncate(e, 60)), Style::new().fg(th::RED)));
    }
    let block = rounded(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let window = app.window.since();
    if app.issues.is_empty() {
        let lines = if let Some(e) = &app.issues_error {
            vec![
                Line::from(Span::styled(format!("✗ {e}"), Style::new().fg(th::RED).bold())),
                Line::from(Span::styled("R retries", th::dim())),
            ]
        } else if app.projects_loaded && app.projects.is_empty() {
            vec![
                Line::from(Span::styled("No projects yet", th::bold())),
                Line::from(Span::styled(
                    format!("create one with: {} projects create <name>", app.hints.command),
                    th::dim(),
                )),
            ]
        } else if !app.issues_loaded {
            vec![Line::from(Span::styled(format!("{} loading issues…", spinner(app)), th::dim()))]
        } else {
            let what =
                if app.status == StatusFilter::All { String::new() } else { format!("{} ", app.status.as_str()) };
            let q = if app.query.is_empty() { String::new() } else { format!(" matching \"{}\"", app.query) };
            vec![
                Line::from(Span::styled(
                    format!("✓ No {what}issues{q} in the last {window}"),
                    Style::new().fg(th::GREEN).bold(),
                )),
                Line::from(Span::styled("t widens the time window · tab switches status · / searches", th::dim())),
            ]
        };
        let y = inner.y + inner.height.saturating_sub(u16_sat(lines.len())) / 2;
        f.render_widget(
            Paragraph::new(lines).alignment(Alignment::Center),
            Rect { y, height: 2.min(inner.height), ..inner },
        );
        return;
    }

    let w = inner.width;
    let show_users = w >= 90;
    let show_age = w >= 110;
    let show_trend = w >= 76;
    let show_status = w >= 64;
    let mut header = vec!["LEVEL", "ISSUE", "EVENTS"];
    let mut widths = vec![Constraint::Length(7), Constraint::Min(20), Constraint::Length(7)];
    if show_users {
        header.push("USERS");
        widths.push(Constraint::Length(6));
    }
    if show_trend {
        header.push("TREND");
        widths.push(Constraint::Length(13));
    }
    header.push("LAST SEEN");
    widths.push(Constraint::Length(9));
    if show_age {
        header.push("AGE");
        widths.push(Constraint::Length(5));
    }
    if show_status {
        header.push("STATUS");
        widths.push(Constraint::Length(10));
    }

    let now = app.now;
    let rows: Vec<Row> = app
        .issues
        .iter()
        .map(|i| {
            let mut cells = vec![
                Cell::from(Span::styled(th::level_label(&i.level), th::badge(&i.level))),
                Cell::from(Text::from(vec![
                    Line::from(Span::styled(i.title.clone(), th::bold())),
                    Line::from(Span::styled(i.culprit.clone().unwrap_or_default(), th::dim())),
                ])),
                Cell::from(Line::from(Span::styled(fmt::compact(i.count), th::text())).right_aligned()),
            ];
            if show_users {
                cells
                    .push(Cell::from(Line::from(Span::styled(fmt::compact(i.user_count), th::text())).right_aligned()));
            }
            if show_trend {
                cells.push(Cell::from(Span::styled(
                    fmt::sparkline(&fmt::resample(&i.trend, 12)),
                    Style::new().fg(th::ACCENT),
                )));
            }
            cells.push(Cell::from(Span::styled(fmt::relative_time(i.last_seen, now), th::text())));
            if show_age {
                cells.push(Cell::from(Span::styled(fmt::age(i.first_seen, now), th::dim())));
            }
            if show_status {
                cells.push(Cell::from(status_span(i.status)));
            }
            Row::new(cells).height(2)
        })
        .collect();
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(th::faint().add_modifier(Modifier::BOLD)).bottom_margin(0))
        .row_highlight_style(th::selected_row())
        .highlight_symbol(Span::styled("▌", th::accent()))
        .highlight_spacing(HighlightSpacing::Always)
        .column_spacing(1);
    f.render_stateful_widget(table, inner, &mut app.issues_state);
}

// ---------------------------------------------------------------- detail view

fn render_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let now = app.now;
    let window = app.window.since();
    let spin = spinner(app);
    let Screen::Detail(d) = &mut app.screen else { return };
    let [title, tabs, content] =
        Layout::vertical([Constraint::Length(4), Constraint::Length(1), Constraint::Min(0)]).areas(area);

    // Title card.
    let s = d.current_summary();
    let level = s.map(|s| s.level.as_str()).or(d.event.as_ref().map(|e| e.summary.level.as_str())).unwrap_or_default();
    let title_text = if d.title().is_empty() {
        d.event.as_ref().map(|e| e.summary.title.clone()).unwrap_or_default()
    } else {
        d.title().to_string()
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th::level_color(level)));
    let inner = block.inner(title);
    f.render_widget(block, title);
    let mut l1 = vec![
        Span::styled(th::level_label(level), th::badge(level)),
        Span::raw(" "),
        Span::styled(title_text, th::bold()),
    ];
    if let Some(e) = &d.error {
        l1.push(Span::styled(format!("  ✗ {e}"), Style::new().fg(th::RED)));
    } else if s.is_none() {
        l1.push(Span::styled(format!("  {spin}"), th::dim()));
    }
    let mut l2 = vec![Span::raw(" ")];
    if let Some(s) = s {
        if let Some(c) = s.culprit.as_ref().filter(|c| !c.is_empty()) {
            l2.push(Span::styled(format!("{c}  "), th::dim()));
        }
        l2.push(status_span(s.status));
        l2.push(Span::styled(
            format!(
                "  ·  {} events  ·  {} users  ·  last seen {}  ·  first seen {}  ·  {window}",
                fmt::compact(s.count),
                fmt::compact(s.user_count),
                fmt::relative_time(s.last_seen, now),
                fmt::relative_time(s.first_seen, now)
            ),
            th::dim(),
        ));
        l2.push(Span::styled(format!("  ·  {}", s.id), th::faint()));
    } else {
        l2.push(Span::styled(d.id.clone(), th::faint()));
    }
    f.render_widget(Paragraph::new(vec![Line::from(l1), Line::from(l2)]), inner);

    // Tab bar + event navigation.
    let mut tab_spans = vec![Span::raw(" ")];
    for (i, t) in Tab::ALL.iter().enumerate() {
        let label = match t {
            Tab::Events if d.events_loaded => {
                format!(" {} {} {} ", i + 1, t.title(), d.events.len())
            }
            Tab::Breadcrumbs if d.event.is_some() => {
                format!(" {} {} {} ", i + 1, t.title(), d.crumbs.len())
            }
            _ => format!(" {} {} ", i + 1, t.title()),
        };
        tab_spans.push(Span::styled(label, if *t == d.tab() { th::tab_active() } else { th::tab_inactive() }));
        tab_spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), tabs);
    let mut nav = vec![];
    if d.events_loaded && !d.events.is_empty() {
        let more = if d.next_cursor.is_some() { "+" } else { "" };
        nav.push(Span::styled("[ ", th::key()));
        nav.push(Span::styled(format!("event {}/{}{more}", d.event_idx + 1, d.events.len()), th::text()));
        if let Some(e) = &d.event {
            nav.push(Span::styled(
                format!(
                    " · {} · {}",
                    fmt::relative_time(e.summary.timestamp, now),
                    fmt::truncate(&e.summary.event_id, 8)
                ),
                th::dim(),
            ));
        } else {
            nav.push(Span::styled(" · loading…", th::dim()));
        }
        nav.push(Span::styled(" ] ", th::key()));
    }
    if tabs.width > 100 {
        f.render_widget(Paragraph::new(Line::from(nav).right_aligned()), tabs);
    }

    match d.tab() {
        Tab::Overview => render_overview(f, content, d, now, window),
        Tab::Stack => render_stack(f, content, d),
        Tab::Breadcrumbs => render_breadcrumbs(f, content, d),
        Tab::Events => render_events(f, content, d, now),
        Tab::Json => render_json(f, content, d),
    }
}

fn kv_line(key: &str, value: Span<'static>) -> Line<'static> {
    Line::from(vec![Span::styled(format!("  {}", fmt::pad(key, 14)), th::dim()), value])
}

fn section_line(title: &str) -> Line<'static> {
    Line::from(Span::styled(title.to_string(), Style::new().fg(th::ACCENT).add_modifier(Modifier::BOLD)))
}

fn render_overview(f: &mut Frame, area: Rect, d: &mut Detail, now: chrono::DateTime<chrono::Utc>, window: &str) {
    let block = rounded(title_span("Overview"));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let two_cols = inner.width >= 96;
    let col_w = if two_cols { inner.width / 2 } else { inner.width };

    let mut left: Vec<Line> = vec![section_line("Details")];
    let s = d.current_summary();
    let txt = |v: String| Span::styled(v, th::text());
    if let Some(s) = s {
        left.push(kv_line("Status", status_span(s.status)));
        left.push(kv_line("Level", Span::styled(s.level.clone(), Style::new().fg(th::level_color(&s.level)))));
        left.push(kv_line("Platform", txt(s.platform.clone())));
        left.push(kv_line("Events", txt(s.count.to_string())));
        left.push(kv_line("Users", txt(s.user_count.to_string())));
        left.push(kv_line(
            "First seen",
            txt(format!("{}  ({})", fmt::timestamp(s.first_seen), fmt::relative_time(s.first_seen, now))),
        ));
        left.push(kv_line(
            "Last seen",
            txt(format!("{}  ({})", fmt::timestamp(s.last_seen), fmt::relative_time(s.last_seen, now))),
        ));
        if let Some(t) = &s.exception_type {
            left.push(kv_line("Exception", txt(t.clone())));
        }
    }
    if let Some(dd) = &d.detail {
        left.push(kv_line("First release", txt(dd.first_release.clone().unwrap_or_else(|| "–".into()))));
        left.push(kv_line("Last release", txt(dd.last_release.clone().unwrap_or_else(|| "–".into()))));
    }
    if let Some(s) = s {
        left.push(Line::raw(""));
        left.push(section_line(&format!("Trend · last {window}")));
        let w = (col_w as usize).saturating_sub(4).clamp(8, 72);
        left.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(fmt::sparkline(&fmt::resample(&s.trend, w)), Style::new().fg(th::ACCENT)),
        ]));
    }
    if let Some(e) = &d.event {
        left.push(Line::raw(""));
        left.push(section_line("Shown event"));
        left.push(kv_line("Event", txt(e.summary.event_id.clone())));
        left.push(kv_line(
            "Time",
            txt(format!("{}  ({})", fmt::timestamp(e.summary.timestamp), fmt::relative_time(e.summary.timestamp, now))),
        ));
        for (k, v) in
            [("Environment", &e.summary.environment), ("Release", &e.summary.release), ("User", &e.summary.user)]
        {
            if let Some(v) = v {
                left.push(kv_line(k, txt(v.clone())));
            }
        }
        if let Some(t) = &e.summary.trace_id {
            left.push(kv_line("Trace", Span::styled(format!("{t}  (t: open)"), Style::new().fg(th::ACCENT2))));
        }
        for (k, v) in &d.contexts {
            left.push(kv_line(k, txt(v.clone())));
        }
        if let Some(r) = &d.request {
            let (method, url) = (r.method.as_deref().unwrap_or_default(), r.url.as_deref().unwrap_or_default());
            left.push(kv_line("Request", txt(format!("{method} {url}"))));
        }
    }
    if let Some(dd) = &d.detail {
        left.push(Line::raw(""));
        left.push(section_line("Activity"));
        if dd.activity.is_empty() {
            left.push(Line::from(Span::styled("  no status changes", th::faint())));
        }
        for a in &dd.activity {
            left.push(Line::from(vec![
                Span::styled(format!("  {}  ", fmt::timestamp(a.at)), th::dim()),
                status_span(a.status),
                Span::styled(format!("  {}", a.reason), th::faint()),
            ]));
        }
    }

    let mut right: Vec<Line> = vec![section_line("Tags")];
    match &d.detail {
        None => match &d.error {
            Some(e) => right.push(Line::from(Span::styled(format!("  ✗ {e}"), Style::new().fg(th::RED)))),
            None => right.push(Line::from(Span::styled("  loading…", th::dim()))),
        },
        Some(dd) if dd.tags.is_empty() => right.push(Line::from(Span::styled("  no tags", th::faint()))),
        Some(dd) => {
            let bar_w = (col_w as usize).saturating_sub(2 + 22 + 1 + 7 + 2).clamp(6, 40);
            for t in &dd.tags {
                right.push(Line::from(vec![
                    Span::styled(format!("  {}", t.key), th::bold()),
                    Span::styled(format!("  {} unique", t.unique), th::faint()),
                ]));
                let total: u64 = t.top.iter().map(|v| v.count).sum::<u64>().max(1);
                for v in &t.top {
                    let bar = fmt::hbar(v.count, total, bar_w);
                    let pad = bar_w.saturating_sub(bar.chars().count());
                    right.push(Line::from(vec![
                        Span::styled(format!("    {}", fmt::pad(&fmt::truncate(&v.value, 21), 22)), th::text()),
                        Span::styled(bar, Style::new().fg(th::ACCENT2)),
                        Span::styled("·".repeat(pad), th::faint()),
                        Span::styled(format!(" {:>5.1}%", v.count as f64 * 100.0 / total as f64), th::dim()),
                    ]));
                }
                right.push(Line::raw(""));
            }
        }
    }

    let total_lines = if two_cols { left.len().max(right.len()) } else { left.len() + 1 + right.len() };
    d.max_scroll = total_lines.saturating_sub(inner.height as usize);
    d.scroll = d.scroll.min(d.max_scroll);
    let scroll = (u16_sat(d.scroll), 0);
    if two_cols {
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
        f.render_widget(Paragraph::new(left).scroll(scroll), l);
        f.render_widget(Paragraph::new(right).scroll(scroll), r);
    } else {
        left.push(Line::raw(""));
        left.extend(right);
        f.render_widget(Paragraph::new(left).scroll(scroll), inner);
    }
}

fn render_stack(f: &mut Frame, area: Rect, d: &mut Detail) {
    let flag = |on: bool| {
        if on { Span::styled("on", Style::new().fg(th::GREEN)) } else { Span::styled("off", th::faint()) }
    };
    let mut title = vec![title_span("Stack trace"), Span::styled("newest first · ", th::faint())];
    title.extend([Span::styled("a", th::key()), Span::styled(" in-app only ", th::dim()), flag(d.in_app_only)]);
    if d.has_raw() {
        title.extend([Span::styled("  w", th::key()), Span::styled(" raw ", th::dim()), flag(d.raw)]);
    }
    title.push(Span::raw(" "));
    let block = rounded(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if d.event.is_none() {
        f.render_widget(load_state(d.event_error.as_deref(), "loading event…"), inner);
        return;
    }
    if d.exceptions.is_empty() {
        let msg = d.message.clone();
        let mut lines = vec![Line::from(Span::styled("This event has no stack trace.", th::dim()))];
        if let Some(m) = msg {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(m, th::text())));
        }
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: false }), inner);
        return;
    }

    let frames = d.stack_frames();
    let mut lines: Vec<Line> = vec![];
    let (mut sel_start, mut sel_end) = (0usize, 0usize);
    let width = inner.width as usize;
    let push_exc_header = |lines: &mut Vec<Line>, i: usize| {
        let e = &d.exceptions[i];
        if i > 0 {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled("Caused by", th::faint().add_modifier(Modifier::ITALIC))));
        }
        let mut spans = vec![Span::styled(e.headline(), Style::new().fg(th::RED).add_modifier(Modifier::BOLD))];
        if let Some(m) = e.mechanism_label() {
            spans.push(Span::styled(format!("  ({m})"), th::dim()));
        }
        lines.push(Line::from(spans));
    };
    // Frames are grouped by exception, in chain order; every exception gets
    // its header even when the in-app filter hides all its frames.
    let mut next_exc = 0;
    for (idx, sf) in frames.iter().enumerate() {
        while next_exc <= sf.exception {
            push_exc_header(&mut lines, next_exc);
            next_exc += 1;
        }
        let selected = idx == d.frame;
        if selected {
            sel_start = lines.len();
        }
        let fr = sf.frame;
        let marker = if selected { Span::styled("▶ ", th::accent()) } else { Span::raw("  ") };
        let dot =
            if fr.in_app { Span::styled("● ", Style::new().fg(th::YELLOW)) } else { Span::styled("  ", th::faint()) };
        let (fs, ls) = if fr.in_app { (th::bold(), Style::new().fg(th::ACCENT2)) } else { (th::dim(), th::faint()) };
        let mut line = Line::from(vec![
            marker,
            dot,
            Span::styled(fr.function_name().to_string(), fs),
            Span::styled("  ", th::faint()),
            Span::styled(
                fmt::truncate_left(&fr.location(), width.saturating_sub(6 + fmt::width(fr.function_name()))),
                ls,
            ),
        ]);
        if selected {
            line = line.style(Style::new().bg(th::SELECTED));
        }
        lines.push(line);
        if selected {
            if fr.context_line.is_some() {
                for (n, text, is_err) in fr.context() {
                    let num = n.map(|n| format!("{n:>6}")).unwrap_or_else(|| "      ".into());
                    let body = fmt::truncate(&text.replace('\t', "    "), width.saturating_sub(12));
                    if is_err {
                        let pad = width.saturating_sub(12 + fmt::width(&body));
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {num} → "),
                                Style::new().fg(th::YELLOW).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("{body}{}", " ".repeat(pad)),
                                Style::new()
                                    .fg(th::TEXT)
                                    .bg(ratatui::style::Color::Indexed(52))
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled(format!("  {num} │ "), th::faint()),
                            Span::styled(body, th::dim()),
                        ]));
                    }
                }
            } else {
                lines.push(Line::from(Span::styled("           no source context for this frame", th::faint())));
            }
            sel_end = lines.len().saturating_sub(1);
        }
    }
    for i in next_exc..d.exceptions.len() {
        push_exc_header(&mut lines, i);
    }
    if frames.is_empty() {
        lines.push(Line::raw(""));
        let hint = if d.in_app_only { "no in-app frames · press a to show all frames" } else { "no stack frames" };
        lines.push(Line::from(Span::styled(hint, th::dim())));
    }

    let h = inner.height as usize;
    if sel_start < d.stack_scroll {
        d.stack_scroll = sel_start.saturating_sub(1);
    } else if sel_end >= d.stack_scroll + h {
        d.stack_scroll = (sel_end + 1).saturating_sub(h).min(sel_start);
    }
    d.stack_scroll = d.stack_scroll.min(lines.len().saturating_sub(h));
    f.render_widget(Paragraph::new(lines).scroll((u16_sat(d.stack_scroll), 0)), inner);
}

fn render_breadcrumbs(f: &mut Frame, area: Rect, d: &mut Detail) {
    let block = rounded(vec![title_span("Breadcrumbs"), Span::styled("newest first ", th::faint())]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if d.event.is_none() {
        f.render_widget(load_state(d.event_error.as_deref(), "loading event…"), inner);
        return;
    }
    if d.crumbs.is_empty() {
        let t = "This event has no breadcrumbs.";
        f.render_widget(Paragraph::new(Span::styled(t, th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let rows: Vec<Row> = d
        .crumbs
        .iter()
        .map(|b| {
            Row::new(vec![
                Cell::from(Span::styled(
                    b.timestamp.map(|t| t.format("%H:%M:%S%.3f").to_string()).unwrap_or_default(),
                    th::dim(),
                )),
                Cell::from(Span::styled(b.level.clone(), Style::new().fg(th::level_color(&b.level)))),
                Cell::from(Span::styled(
                    b.category.clone().or(b.ty.clone()).unwrap_or_default(),
                    Style::new().fg(th::ACCENT2),
                )),
                Cell::from(Span::styled(b.text(), th::text())),
            ])
        })
        .collect();
    let table =
        Table::new(rows, [Constraint::Length(12), Constraint::Length(8), Constraint::Length(18), Constraint::Min(10)])
            .header(Row::new(["TIME", "LEVEL", "CATEGORY", "MESSAGE"]).style(th::faint().bold()))
            .row_highlight_style(th::selected_row())
            .highlight_symbol(Span::styled("▌", th::accent()))
            .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, inner, &mut d.crumbs_state);
}

fn render_events(f: &mut Frame, area: Rect, d: &mut Detail, now: chrono::DateTime<chrono::Utc>) {
    let more = if d.next_cursor.is_some() { "· more below " } else { "" };
    let block = rounded(vec![title_span("Events"), Span::styled(format!("newest first · ⏎ show {more}"), th::faint())]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !d.events_loaded {
        f.render_widget(load_state(d.events_error.as_deref(), "loading events…"), inner);
        return;
    }
    if d.events.is_empty() {
        let t = "No events in this issue.";
        f.render_widget(Paragraph::new(Span::styled(t, th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let shown = d.event_id.clone();
    let rows: Vec<Row> = d
        .events
        .iter()
        .map(|e| {
            let mark = if shown.as_deref() == Some(e.event_id.as_str()) {
                Span::styled("●", th::accent())
            } else {
                Span::raw(" ")
            };
            Row::new(vec![
                Cell::from(mark),
                Cell::from(Span::styled(fmt::relative_time(e.timestamp, now), th::text())),
                Cell::from(Span::styled(fmt::truncate(&e.event_id, 10), th::dim())),
                Cell::from(Span::styled(e.environment.clone().unwrap_or_default(), Style::new().fg(th::ACCENT2))),
                Cell::from(Span::styled(e.release.clone().unwrap_or_default(), th::text())),
                Cell::from(Span::styled(e.user.clone().unwrap_or_default(), th::dim())),
                Cell::from(Span::styled(e.message.clone().unwrap_or_else(|| e.title.clone()), th::text())),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(9),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(16),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["", "WHEN", "EVENT", "ENV", "RELEASE", "USER", "MESSAGE"]).style(th::faint().bold()))
    .row_highlight_style(th::selected_row())
    .highlight_symbol(Span::styled("▌", th::accent()))
    .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, inner, &mut d.events_state);
}

/// Very small JSON highlighter for pretty-printed (one key per line) JSON.
fn json_line(line: &str) -> Line<'static> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let mut spans = vec![Span::raw(indent.to_string())];
    let mut value = rest;
    if let Some(pos) = rest.find("\": ").filter(|_| rest.starts_with('"')) {
        spans.push(Span::styled(rest[..pos + 1].to_string(), Style::new().fg(th::ACCENT)));
        spans.push(Span::styled(": ", th::faint()));
        value = &rest[pos + 3..];
    }
    let (v, comma) = match value.strip_suffix(',') {
        Some(v) => (v, ","),
        None => (value, ""),
    };
    let style = if v.starts_with('"') {
        Style::new().fg(th::GREEN)
    } else if v == "true" || v == "false" || v == "null" {
        Style::new().fg(th::MAGENTA)
    } else if v.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        Style::new().fg(th::YELLOW)
    } else {
        th::faint()
    };
    spans.push(Span::styled(v.to_string(), style));
    spans.push(Span::styled(comma, th::faint()));
    Line::from(spans)
}

fn render_json(f: &mut Frame, area: Rect, d: &mut Detail) {
    let block = rounded(vec![title_span("JSON"), Span::styled("full event payload ", th::faint())]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if d.event.is_none() {
        f.render_widget(load_state(d.event_error.as_deref(), "loading event…"), inner);
        return;
    }
    let h = inner.height as usize;
    d.max_scroll = d.json.len().saturating_sub(h);
    d.scroll = d.scroll.min(d.max_scroll);
    let num_w = d.json.len().to_string().len();
    let lines: Vec<Line> = d
        .json
        .iter()
        .enumerate()
        .skip(d.scroll)
        .take(h)
        .map(|(i, l)| {
            let mut line = json_line(l);
            line.spans.insert(0, Span::styled(format!("{:>num_w$} ", i + 1), th::faint()));
            line
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------- trace view

fn render_trace(f: &mut Frame, area: Rect, app: &mut App) {
    let spin = spinner(app);
    let App { projects, screen, now, .. } = app;
    let now = *now;
    let Screen::Trace(t) = screen else { return };
    let block = rounded(vec![
        title_span("Trace"),
        Span::styled(format!("{} ", t.trace_id), Style::new().fg(th::ACCENT2)),
        Span::styled(if t.loaded { format!("· {} error events ", t.events.len()) } else { String::new() }, th::faint()),
    ]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !t.loaded {
        f.render_widget(load_state(t.error.as_deref(), &format!("{spin} loading trace…")), inner);
        return;
    }
    if t.events.is_empty() {
        let text = "No error events for this trace.";
        f.render_widget(Paragraph::new(Span::styled(text, th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let rows: Vec<Row> = t
        .events
        .iter()
        .map(|e| {
            let project = projects
                .iter()
                .find(|p| p.id == e.project_id)
                .map(|p| p.slug.clone())
                .unwrap_or_else(|| e.project_id.to_string());
            Row::new(vec![
                Cell::from(Span::styled(th::level_label(&e.level), th::badge(&e.level))),
                Cell::from(Span::styled(project, Style::new().fg(th::ACCENT2))),
                Cell::from(Span::styled(e.title.clone(), th::bold())),
                Cell::from(Span::styled(e.span_id.clone().unwrap_or_default(), th::faint())),
                Cell::from(Span::styled(fmt::relative_time(e.timestamp, now), th::text())),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(14),
            Constraint::Min(20),
            Constraint::Length(16),
            Constraint::Length(9),
        ],
    )
    .header(Row::new(["LEVEL", "PROJECT", "EVENT", "SPAN", "WHEN"]).style(th::faint().bold()))
    .row_highlight_style(th::selected_row())
    .highlight_symbol(Span::styled("▌", th::accent()))
    .highlight_spacing(HighlightSpacing::Always)
    .column_spacing(1);
    f.render_stateful_widget(table, inner, &mut t.state);
}

// ---------------------------------------------------------------- overlays

fn render_help(f: &mut Frame, area: Rect) {
    let r = centered(area, 76, 26);
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(th::accent())
        .title(title_span("Keys"))
        .title_bottom(Line::from(Span::styled(" any key closes ", th::faint())).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let row = |k: &str, d: &str| {
        Line::from(vec![Span::styled(format!("  {k:<12}"), th::key()), Span::styled(d.to_string(), th::text())])
    };
    let left = vec![
        section_line("Everywhere"),
        row("↑ ↓  j k", "move"),
        row("g G", "top / bottom"),
        row("PgUp PgDn", "page"),
        row("R", "refresh now"),
        row("?", "this help"),
        row("ctrl-c", "quit"),
        Line::raw(""),
        section_line("Issues list"),
        row("⏎", "open issue"),
        row("tab ← →", "status tab"),
        row("1-4", "unresolved/resolved/…"),
        row("/", "search title, culprit"),
        row("s", "cycle sort"),
        row("t T", "cycle time window"),
        row("p", "switch project"),
        row("q", "quit"),
    ];
    let right = vec![
        section_line("Issue detail"),
        row("tab ← →", "next / prev section"),
        row("1-5", "jump to section"),
        row("[ ]", "older / newer event"),
        row("a", "in-app frames only"),
        row("w", "raw (minified) frames"),
        row("t", "events of this trace"),
        row("⏎", "show event (Events)"),
        row("esc q", "back"),
        Line::raw(""),
        section_line("Status"),
        row("r", "resolve"),
        row("i", "ignore"),
        row("u", "unresolve"),
        Line::raw(""),
        Line::from(Span::styled("  Data refreshes every 5s.", th::faint())),
    ];
    let [l, rr] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), rr);
}

fn render_projects(f: &mut Frame, area: Rect, app: &App, selected: usize) {
    let h = u16_sat(app.projects.len()).saturating_add(4).clamp(6, area.height.saturating_sub(2));
    let r = centered(area, 60, h);
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(th::accent())
        .title(title_span("Projects"))
        .title_bottom(Line::from(Span::styled(" ⏎ select · esc close ", th::faint())).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    if app.projects.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("no projects", th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let current = app.current_project().map(|p| p.id);
    let rows: Vec<Row> = app
        .projects
        .iter()
        .map(|p| {
            let mark =
                if Some(p.id) == current { Span::styled("●", Style::new().fg(th::GREEN)) } else { Span::raw(" ") };
            Row::new(vec![
                Cell::from(mark),
                Cell::from(Span::styled(p.slug.clone(), th::bold())),
                Cell::from(Span::styled(p.name.clone(), th::text())),
                Cell::from(Span::styled(p.platform.clone().unwrap_or_default(), th::dim())),
            ])
        })
        .collect();
    let mut state = ratatui::widgets::TableState::default().with_selected(Some(selected));
    let table =
        Table::new(rows, [Constraint::Length(1), Constraint::Length(18), Constraint::Min(10), Constraint::Length(12)])
            .row_highlight_style(th::selected_row())
            .highlight_symbol(Span::styled("▌", th::accent()))
            .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, inner, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::fixtures::*;
    use crossterm::event::{KeyCode, KeyEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use sdl_client::types::IssueList;

    pub fn draw(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, f.area(), app)).unwrap();
        screen(&term)
    }

    #[test]
    fn json_lines_are_highlighted_by_kind() {
        let parts =
            |l: &str| json_line(l).spans.iter().map(|s| (s.content.to_string(), s.style.fg)).collect::<Vec<_>>();
        let p = parts(r#"    "level": "error","#);
        assert_eq!(p[0].0, "    ");
        assert_eq!(p[1], ("\"level\"".into(), Some(th::ACCENT)));
        assert_eq!(p[3], ("\"error\"".into(), Some(th::GREEN)));
        assert_eq!(p[4].0, ",");
        assert_eq!(parts(r#""n": -1.5"#)[3], ("-1.5".into(), Some(th::YELLOW)));
        assert_eq!(parts(r#""ok": true,"#)[3].1, Some(th::MAGENTA));
        assert_eq!(parts("null")[1].1, Some(th::MAGENTA));
        // A value line without a key, and structure lines.
        assert_eq!(parts(r#"  "x","#)[1], ("\"x\"".into(), Some(th::GREEN)));
        assert_eq!(parts("  },")[1].0, "}");
    }

    fn press(app: &mut App, c: KeyCode) {
        let _ = app.on_key(KeyEvent::from(c));
    }

    #[test]
    fn issues_view_shows_stats_table_and_header() {
        let mut app = loaded_app();
        let s = draw(&mut app, 140, 40);
        for want in [
            "sentry-ducklake",
            "Web App",
            "queue",
            "12/20k",
            "Unresolved 3",
            "EVENTS",
            "1.3k",
            "USERS",
            "TypeError: Cannot read properties of undefined",
            "app/checkout.js in parseConfig",
            "1.2k",
            "3m ago",
            "1d ago",
            " ERROR ",
            " FATAL ",
            "unresolved",
            "sort Last seen",
            "window 24h",
            "TREND",
        ] {
            assert!(s.contains(want), "missing {want:?} in\n{s}");
        }
        // A sparkline character is drawn in the trend column.
        assert!(s.contains('█'));
    }

    #[test]
    fn escape_sequences_from_events_never_reach_the_buffer() {
        let mut app = App::new("http://x", None);
        app.start();
        app.on_response(Resp::Projects(Ok(projects())));
        let mut list = issues();
        list[0].title = "evil\x1b]52;c;cHduZWQ=\x07 \x1b[2Jtitle\u{9b}31m".into();
        list[0].culprit = Some("cul\rprit\tx".into());
        let key = app.issues_key().unwrap();
        app.on_response(Resp::Issues(key, Ok(IssueList { issues: list, window_start: now(), window_end: now() })));
        app.now = now();
        let s = draw(&mut app, 140, 40);
        assert!(!s.chars().any(|c| c.is_control() && c != '\n'), "control character in buffer:\n{s}");
        assert!(s.contains("evil\u{FFFD}]52;c;cHduZWQ=\u{FFFD} \u{FFFD}[2Jtitle"), "{s}");
        assert!(s.contains("cul prit    x"), "{s}");
    }

    #[test]
    fn errors_render_in_place_and_offline_only_for_network_errors() {
        let mut app = App::new("http://x", Some("web".into()));
        app.start();
        app.on_response(Resp::Projects(Ok(projects())));
        let key = app.issues_key().unwrap();
        app.on_response(Resp::Issues(key, Err(FetchError::api("not found: project not found"))));
        app.on_response(Resp::Info(Ok(info())));
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("✗ not found: project not found"), "{s}");
        assert!(s.contains("● live") && !s.contains("offline"), "{s}");

        app.on_response(Resp::Stats {
            project: "web".into(),
            window: app.window,
            result: Err(FetchError::offline("cannot reach server")),
        });
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("● offline") && s.contains("cannot reach server"), "{s}");
        // Info succeeding clears "offline" but not the issue list error.
        app.on_response(Resp::Info(Ok(info())));
        assert!(app.offline.is_none());
        assert!(app.issues_error.is_some() && app.stats_error.is_some());
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("project not found") && !s.contains("offline"), "{s}");
    }

    #[test]
    fn no_projects_hint_names_the_host_command() {
        let mut app = App::new("http://x", None);
        app.hints.command = "mallard errors";
        app.start();
        app.on_response(Resp::Projects(Ok(vec![])));
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("create one with: mallard errors projects create <name>"), "{s}");
    }

    #[test]
    fn narrow_and_tiny_terminals() {
        let mut app = loaded_app();
        let s = draw(&mut app, 60, 16);
        assert!(s.contains("ERROR") && !s.contains("TREND"), "{s}");
        let s = draw(&mut app, 40, 10);
        assert!(s.contains("Terminal too small"), "{s}");
    }

    #[test]
    fn empty_and_search_states() {
        let mut app = loaded_app();
        press(&mut app, KeyCode::Char('/'));
        for c in "zzz".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("zzz") && s.contains("enter search"), "{s}");
        press(&mut app, KeyCode::Enter);
        let key = app.issues_key().unwrap();
        app.on_response(Resp::Issues(
            key,
            Ok(sdl_client::types::IssueList { issues: vec![], window_start: now(), window_end: now() }),
        ));
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("No unresolved issues matching \"zzz\" in the last 24h"), "{s}");
    }

    #[test]
    fn detail_tabs_render() {
        let mut app = detail_app();
        let s = draw(&mut app, 140, 45);
        for want in [
            "Error: inside span",
            "Overview",
            "Stack trace",
            "web@1.2.3",
            "browser",
            "Chrome 141",
            "event 1/2",
            "Activity",
            "node v24.22.1",
        ] {
            assert!(s.contains(want), "overview missing {want:?}\n{s}");
        }

        press(&mut app, KeyCode::Char('2'));
        let s = draw(&mut app, 140, 45);
        for want in [
            "Error: inside span",
            "Caused by",
            "TypeError: root cause",
            "parseConfig",
            "/app/smoke.mjs:11:84",
            "throw new Error",
            "line nine",
            "unhandled",
        ] {
            assert!(s.contains(want), "stack missing {want:?}\n{s}");
        }
        // In-app only hides node internals; raw shows the minified frame.
        assert!(s.contains("runMain"));
        press(&mut app, KeyCode::Char('a'));
        let s = draw(&mut app, 140, 45);
        assert!(!s.contains("runMain") && s.contains("parseConfig"), "{s}");
        press(&mut app, KeyCode::Char('w'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("/app/smoke.min.js:1:999"), "{s}");

        press(&mut app, KeyCode::Char('3'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("user logged in") && s.contains("GET /api/cart -> 500"), "{s}");

        press(&mut app, KeyCode::Char('4'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("production") && s.contains("id:user-42") && s.contains("1h ago"), "{s}");

        press(&mut app, KeyCode::Char('5'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("\"event_id\"") && s.contains("655354d4abd54c31a077367796352d8c"), "{s}");
    }

    #[test]
    fn overlays_and_trace_view() {
        let mut app = loaded_app();
        press(&mut app, KeyCode::Char('?'));
        let s = draw(&mut app, 120, 40);
        assert!(s.contains("Keys") && s.contains("older / newer event"), "{s}");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('p'));
        let s = draw(&mut app, 120, 40);
        assert!(s.contains("Projects") && s.contains("api"), "{s}");
        press(&mut app, KeyCode::Esc);

        app.show_trace("b7644661850f40e69380d8c9ea253e84");
        app.on_response(Resp::Trace { trace_id: "b7644661850f40e69380d8c9ea253e84".into(), result: Ok(event_list()) });
        let s = draw(&mut app, 120, 30);
        assert!(
            s.contains("Trace")
                && s.contains("b7644661850f40e69380d8c9ea253e84")
                && s.contains("2 error events")
                && s.contains("web"),
            "{s}"
        );
    }

    #[test]
    fn draws_only_inside_the_given_area() {
        let mut app = loaded_app();
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| {
            let a = Rect { x: 10, y: 5, width: 120, height: 30 };
            render(f, a, &mut app)
        })
        .unwrap();
        let buf = term.backend().buffer();
        for y in 0..40u16 {
            for x in 0..140u16 {
                let inside = (10..130).contains(&x) && (5..35).contains(&y);
                if !inside {
                    assert_eq!(buf[(x, y)].symbol(), " ", "drew outside area at {x},{y}");
                }
            }
        }
    }
}
