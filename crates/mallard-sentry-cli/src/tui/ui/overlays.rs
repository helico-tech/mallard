//! Help and the project switcher, drawn over the screen.

use super::*;

pub(super) fn render_help(f: &mut Frame, area: Rect) {
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
    left.extend(keys::SHARED.iter().map(|(k, d)| row_w(k, d, 22)));
    left.push(Line::raw(""));
    left.push(Line::from(Span::styled(format!("  {} Data refreshes every 5s.", keys::TIMES), th::faint())));
    let right = vec![
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
    let [l, rr] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(inner);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), rr);
}
