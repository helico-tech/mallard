//! The issue list: stats panel, filter bar and the table.

use super::*;

pub(super) fn render_issues(f: &mut Frame, area: Rect, app: &mut App) {
    // With no project there are no stats to load.
    let stats_h = if area.height >= 22 && app.project.is_some() { 7 } else { 0 };
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
    if !app.deploys.is_empty() {
        title.push(Span::styled("┊ deploy ", th::dim()));
    }
    if let Some(e) = &app.deploys_error {
        title.push(Span::styled(format!("deploys: {e} "), th::error()));
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
    if let Some((first, last)) = s.and_then(|s| Some((s.buckets.first()?, s.buckets.last()?))) {
        let end = last.start.timestamp() as f64 + s.map_or(0.0, |s| s.interval_seconds as f64);
        let cols = mallard_term::tui::mark_columns(&app.deploys, first.start.timestamp() as f64, end, plot.width);
        mallard_term::tui::draw_marks(f.buffer_mut(), plot, &cols, th::dim());
    }
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
        right.push(Span::styled(
            format!("{} ", fmt::truncate(&fmt::clean(&app.query), 24)),
            Style::new().fg(th::YELLOW),
        ));
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

fn render_issue_table(f: &mut Frame, area: Rect, app: &mut App) {
    let mut title = vec![title_span("Issues"), Span::styled(format!("{} ", app.status.as_str()), th::dim())];
    if app.list_paused() {
        title.push(Span::styled("paused, r refreshes ", th::dim()));
    }
    if let Some(e) = app.issues_error.as_ref().filter(|_| !app.issues.is_empty()) {
        // The list is still shown; the failed refresh is noted in the title.
        title.push(Span::styled(format!("✗ refresh failed: {} ", fmt::truncate(e, 60)), Style::new().fg(th::RED)));
    }
    let block = rounded(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let window = app.window.since();
    if app.issues.is_empty() {
        let lines = if let Some(slug) = &app.no_project {
            let slug = fmt::clean(slug);
            vec![
                Line::from(Span::styled(format!("App {slug} has no project, so no errors here"), th::bold())),
                Line::from(Span::styled(
                    format!("{} setup {slug} gives it a DSN · p picks another app", app.hints.command),
                    th::dim(),
                )),
            ]
        } else if app.project.is_none() && app.issues_error.is_none() {
            vec![
                Line::from(Span::styled("Pick an app to see its errors", th::bold())),
                Line::from(Span::styled("errors are per project: press p to choose", th::dim())),
            ]
        } else if let Some(e) = &app.issues_error {
            vec![
                Line::from(Span::styled(format!("✗ {e}"), Style::new().fg(th::RED).bold())),
                Line::from(Span::styled("R retries", th::dim())),
            ]
        } else if !app.issues_loaded {
            vec![Line::from(Span::styled(format!("{} loading issues…", spinner(app)), th::dim()))]
        } else {
            let what =
                if app.status == StatusFilter::All { String::new() } else { format!("{} ", app.status.as_str()) };
            let q =
                if app.query.is_empty() { String::new() } else { format!(" matching \"{}\"", fmt::clean(&app.query)) };
            vec![
                Line::from(Span::styled(
                    format!("✓ No {what}issues{q} in the last {window}"),
                    Style::new().fg(th::GREEN).bold(),
                )),
                Line::from(Span::styled(
                    if app.offers_day() {
                        "d: show last 24h · t widens the time window · tab switches status · / searches"
                    } else {
                        "t widens the time window · tab switches status · / searches"
                    },
                    th::dim(),
                )),
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
                Cell::from(Span::styled(th::level_label(&i.level), th::level_badge(&i.level))),
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
        .row_highlight_style(th::selected())
        .highlight_symbol(Span::styled("▌", th::accent()))
        .highlight_spacing(HighlightSpacing::Always)
        .column_spacing(1);
    f.render_stateful_widget(table, inner, &mut app.issues_state);
}
