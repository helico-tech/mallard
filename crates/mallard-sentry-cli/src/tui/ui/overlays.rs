//! Help and the project switcher, drawn over the screen.

use super::*;

pub(super) fn render_help(f: &mut Frame, area: Rect, hosted: bool) {
    use mallard_term::tui::keys;
    let r = centered(area, 112, 21);
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(th::accent())
        .title(title_span("Keys"))
        .title_bottom(Line::from(Span::styled(" any key closes ", th::faint())).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let row_w = |k: &str, d: &str, w: usize| {
        Line::from(vec![Span::styled(format!("  {k:<w$}"), th::key()), Span::styled(d.to_string(), th::text())])
    };
    let row = |k: &str, d: &str| row_w(k, d, 14);
    // The keys every tab shares come first (mallard_term::tui::keys).
    let mut left = vec![section_line("Everywhere")];
    let picker = keys::APP_PICKER.to_string();
    left.extend(keys::SHARED.iter().filter(|(k, _)| hosted || *k != picker).map(|(k, d)| row_w(k, d, 22)));
    left.push(Line::raw(""));
    left.push(Line::from(Span::styled(format!("  {} Data refreshes every 5s.", keys::TIMES), th::faint())));
    let mut right = vec![
        section_line("Issues list"),
        row("tab ← → 1-4", "status tab"),
        row("s", "cycle sort"),
        row("/", "key=value is a tag; tab accepts"),
        row("R I U", "resolve / ignore / unresolve"),
        Line::raw(""),
        section_line("Issue detail"),
        row("tab ← → 1-5", "section"),
        row("[ ]", "older / newer event"),
        row("a", "in-app frames only"),
        row("w", "raw (minified) frames"),
        row("x", "this event's trace"),
        row("R I U", "resolve / ignore / unresolve"),
    ];
    if !hosted {
        right.insert(3, row("p", "switch project"));
    }
    let [l, rr] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(inner);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), rr);
}

pub(super) fn render_projects(f: &mut Frame, area: Rect, app: &App, selected: usize) {
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
            .row_highlight_style(th::selected())
            .highlight_symbol(Span::styled("▌", th::accent()))
            .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, inner, &mut state);
}
