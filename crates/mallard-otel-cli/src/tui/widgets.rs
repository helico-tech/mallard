//! Shared building blocks: panels, KPI tiles, charts, histogram, helpers.

use std::ops::Range;

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Chart, Dataset, GraphType, Paragraph, Sparkline, Widget},
};

use super::theme;

pub use mallard_term::tui::{centered, hints};

/// Rounded panel with a styled title.
pub fn panel(title: &str, focused: bool) -> Block<'static> {
    mallard_term::tui::rounded()
        .border_style(if focused { theme::border_focus() } else { theme::border() })
        .title(Line::from(vec![Span::raw(" "), Span::styled(title.to_string(), theme::title()), Span::raw(" ")]))
}

/// Panel with a right-aligned secondary title (e.g. counts, loading state).
pub fn panel_with(title: &str, right: Line<'static>, focused: bool) -> Block<'static> {
    panel(title, focused).title_top(right.right_aligned())
}

/// Centered message inside an area (empty and error states).
pub fn placeholder(f: &mut Frame, area: Rect, msg: &str, style: Style) {
    if area.height == 0 {
        return;
    }
    let y = area.y + area.height / 2;
    let r = Rect { x: area.x, y, width: area.width, height: 1 };
    f.render_widget(Paragraph::new(Span::styled(msg.to_string(), style)).alignment(Alignment::Center), r);
}

/// `t` formatted in UTC, like every time in both TUIs and CLIs.
pub fn utc(t: DateTime<Utc>, fmt: &str) -> String {
    t.format(fmt).to_string()
}

/// Terminal display width of a string (wide CJK/emoji count as 2).
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Untrusted text cut to `width` terminal columns with an ellipsis: cleaned
/// first (control and bidi characters become visible escapes).
pub fn trunc(s: &str, width: usize) -> String {
    mallard_term::text::truncate(&mallard_term::text::clean(s), width)
}

/// A KPI tile: label, big value, optional sparkline underneath.
pub struct Tile<'a> {
    pub label: &'a str,
    pub value: String,
    pub color: Color,
    pub sub: String,
    pub spark: &'a [u64],
}

impl Tile<'_> {
    pub fn render(self, f: &mut Frame, area: Rect) {
        let block = mallard_term::tui::rounded();
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).split(inner);
        f.render_widget(
            Line::from(vec![
                Span::styled(self.label.to_uppercase(), theme::dim().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(self.sub, theme::faint()),
            ]),
            rows[0],
        );
        f.render_widget(Span::styled(self.value, Style::new().fg(self.color).add_modifier(Modifier::BOLD)), rows[1]);
        if rows[2].height > 0 && !self.spark.is_empty() {
            let n = rows[2].width as usize;
            let data = &self.spark[self.spark.len().saturating_sub(n)..];
            f.render_widget(Sparkline::default().data(data).style(Style::new().fg(self.color)), rows[2]);
        }
    }
}

/// One line series for [`time_chart`].
pub struct Series<'a> {
    pub name: String,
    pub color: Color,
    pub points: &'a [(f64, f64)],
}

/// Axis labels for epoch-second bounds.
pub fn time_labels(x0: f64, x1: f64) -> Vec<String> {
    let span = x1 - x0;
    let fmt = if span > 86_400.0 * 2.0 {
        "%m-%d %H:%M"
    } else if span > 3600.0 * 3.0 {
        "%H:%M"
    } else {
        "%H:%M:%S"
    };
    [x0, (x0 + x1) / 2.0, x1]
        .iter()
        .map(|t| DateTime::from_timestamp(*t as i64, 0).map(|d| utc(d, fmt)).unwrap_or_default())
        .collect()
}

/// Colored legend for a panel title: `━ name  ━ name`.
/// The legend of a chart with deploys on it.
pub fn legend_with_deploys(series: &[Series], marks: &[f64]) -> Line<'static> {
    let mut l = legend(series);
    if !marks.is_empty() {
        l.spans.push(Span::styled("┊ deploy ", theme::dim()));
    }
    l
}

pub fn legend(series: &[Series]) -> Line<'static> {
    let mut spans = vec![];
    for s in series {
        spans.push(Span::styled(" ━ ", Style::new().fg(s.color)));
        spans.push(Span::styled(s.name.clone(), theme::dim()));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Line chart over time with automatic y bounds. `x_bounds` (epoch seconds)
/// fixes the time axis to the query window so sparse data stays in place.
pub fn time_chart(
    f: &mut Frame,
    area: Rect,
    block: Block<'static>,
    series: &[Series],
    x_bounds: Option<[f64; 2]>,
    fmt_y: &dyn Fn(f64) -> String,
    marks: &[f64],
) {
    let all = series.iter().flat_map(|s| s.points.iter());
    let (mut x0, mut x1, mut y1) = (f64::MAX, f64::MIN, 0.0f64);
    let mut y0 = f64::MAX;
    for (x, y) in all {
        x0 = x0.min(*x);
        x1 = x1.max(*x);
        y0 = y0.min(*y);
        y1 = y1.max(*y);
    }
    if x0 > x1 {
        let inner = block.inner(area);
        f.render_widget(block, area);
        placeholder(f, inner, "no data in this window", theme::faint());
        return;
    }
    if let Some([a, b]) = x_bounds {
        x0 = x0.min(a);
        x1 = x1.max(b);
    }
    if x1 - x0 < 1.0 {
        x1 = x0 + 1.0;
    }
    let y0 = if y0 >= 0.0 { 0.0 } else { y0 * 1.1 };
    let y1 = if y1 <= y0 { y0 + 1.0 } else { y1 * 1.1 };
    // Deploys: a vertical line each, drawn first so the data stays on top.
    let lines: Vec<[(f64, f64); 2]> =
        marks.iter().filter(|m| (x0..=x1).contains(*m)).map(|m| [(*m, y0), (*m, y1)]).collect();
    let lines_style = Style::new().fg(theme::DIM);
    let datasets: Vec<Dataset> = lines
        .iter()
        .map(|l| {
            Dataset::default().marker(symbols::Marker::Braille).graph_type(GraphType::Line).style(lines_style).data(l)
        })
        .chain(series.iter().map(|s| {
            Dataset::default()
                .name(s.name.clone())
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(s.color))
                .data(s.points)
        }))
        .collect();
    let xl = time_labels(x0, x1);
    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(
            Axis::default()
                .bounds([x0, x1])
                .style(theme::faint())
                .labels(xl.into_iter().map(|l| Span::styled(l, theme::faint()))),
        )
        .y_axis(
            Axis::default()
                .bounds([y0, y1])
                .style(theme::faint())
                .labels([fmt_y(y0), fmt_y((y0 + y1) / 2.0), fmt_y(y1)].map(|l| Span::styled(l, theme::faint()))),
        )
        // Legends live in panel titles / tables so they never cover data.
        .legend_position(None);
    f.render_widget(chart, area);
}

/// Stacked vertical bars, one column per bucket, bottom-up segments.
pub struct StackedBars<'a> {
    /// Per bucket: segment values in drawing order (bottom first).
    pub buckets: &'a [Vec<u64>],
    pub colors: &'a [Color],
}

const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

impl Widget for StackedBars<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.buckets.is_empty() {
            return;
        }
        let max = self.buckets.iter().map(|b| b.iter().sum::<u64>()).max().unwrap_or(0).max(1);
        let cols = area.width as usize;
        // Resample buckets to the available width (merge or stretch).
        let n = self.buckets.len();
        let total_eighths = area.height as u64 * 8;
        for col in 0..cols {
            let a = col * n / cols;
            let b = (((col + 1) * n) / cols).max(a + 1).min(n);
            let mut seg = vec![0u64; self.colors.len()];
            for bucket in &self.buckets[a..b] {
                for (i, v) in bucket.iter().enumerate().take(seg.len()) {
                    seg[i] += v;
                }
            }
            let merged = (b - a) as u64;
            // cumulative heights in eighths of a cell
            let mut cum = 0u64;
            let mut bounds = Vec::with_capacity(seg.len());
            for v in &seg {
                cum += v;
                let h = if cum == 0 {
                    0
                } else {
                    ((cum as f64 / merged as f64) / max as f64 * total_eighths as f64).round().max(1.0) as u64
                };
                bounds.push(h.min(total_eighths));
            }
            for row in 0..area.height as u64 {
                let lo = row * 8;
                let hi = lo + 8;
                let top = bounds.last().copied().unwrap_or(0);
                if top <= lo {
                    break;
                }
                // Color of the segment covering the top filled eighth of
                // this cell (segment i covers bounds[i-1]..bounds[i]).
                let filled_to = top.min(hi);
                let fill = (filled_to - lo) as usize;
                let idx = bounds.iter().position(|b| *b >= filled_to).unwrap_or(0);
                let color = self.colors[idx];
                let y = area.y + area.height - 1 - row as u16;
                let x = area.x + col as u16;
                buf[(x, y)].set_symbol(EIGHTHS[fill]).set_fg(color);
            }
        }
    }
}

/// Keep the scroll offset so that rows fill the visible area: ratatui keeps
/// an old offset when the area grows, which would hide rows above.
pub fn fit_offset(offset: &mut usize, selected: Option<usize>, len: usize, visible: u16) {
    let visible = visible as usize;
    if visible == 0 {
        return;
    }
    let max_offset = len.saturating_sub(visible);
    if *offset > max_offset {
        *offset = max_offset;
    }
    if let Some(sel) = selected {
        if sel < *offset {
            *offset = sel;
        } else if sel >= *offset + visible {
            *offset = sel + 1 - visible;
        }
    }
}

/// The part of a list of `len` rows that fits in `height`: fits `offset`
/// (the list's scroll state) to the selection and returns the visible range
/// and the selection within it. Build rows only for this range and render
/// them with a fresh state: lists can be long and `draw` runs often.
pub fn visible(offset: &mut usize, selected: Option<usize>, len: usize, height: u16) -> (Range<usize>, Option<usize>) {
    fit_offset(offset, selected, len, height);
    let start = (*offset).min(len);
    let end = (start + height as usize).min(len);
    (start..end, selected.filter(|s| (start..end).contains(s)).map(|s| s - start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates() {
        assert_eq!(trunc("hello", 10), "hello");
        assert_eq!(trunc("hello world", 6), "hello…");
        assert_eq!(trunc("a\nb", 5), "a b");
        assert_eq!(trunc("x", 0), "");
        // Review 2.N: bidi overrides are not passed through either.
        assert!(!trunc("a\u{202e}b", 20).contains('\u{202e}'));
    }

    #[test]
    fn chart_labels_are_utc() {
        // 2026-01-01 10:00:00 UTC, whatever the local time zone is.
        let x = 1_767_261_600.0;
        assert_eq!(time_labels(x, x + 600.0)[0], "10:00:00");
    }

    #[test]
    fn offsets_fit() {
        let mut o = 5;
        fit_offset(&mut o, Some(2), 3, 10);
        assert_eq!(o, 0);
        let mut o = 0;
        fit_offset(&mut o, Some(30), 100, 10);
        assert_eq!(o, 21);
    }

    #[test]
    fn stacked_bars_render() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 3));
        let buckets = vec![vec![1, 1], vec![0, 0], vec![2, 4]];
        StackedBars { buckets: &buckets, colors: &[Color::Red, Color::Blue] }.render(buf.area, &mut buf);
        // tallest column is full height
        assert_eq!(buf[(3, 0)].symbol(), "█");
    }

    // Review finding 32: a cell takes the color of the segment at its top.
    #[test]
    fn stacked_bars_color_the_top_segment() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        // 6/8 red at the bottom, 2/8 blue on top: the cell's top is blue.
        let buckets = vec![vec![6, 2]];
        StackedBars { buckets: &buckets, colors: &[Color::Red, Color::Blue] }.render(buf.area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "█");
        assert_eq!(buf[(0, 0)].fg, Color::Blue);
    }
}
