//! The one table style of both CLIs: boxed on a terminal, plain when piped.

use std::io::IsTerminal;

use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table, presets};
use serde_json::Value;

use crate::color::dim;
use crate::fmt::ms;
use crate::text::{clean, json_safe};

/// Whether stdout is a terminal: boxed tables, sparklines and multi-line
/// cells. When piped, output is plain: one row per line, columns two
/// spaces apart, no box drawing (ADR 0008).
pub fn fancy() -> bool {
    std::io::stdout().is_terminal()
}

/// A table for stdout: [`styled`] with [`fancy`].
pub fn table(headers: &[&str]) -> Table {
    styled(headers, fancy())
}

/// `boxed`: rounded, condensed UTF-8 table with bold headers that fits the
/// terminal width. Else plain: no borders, columns two spaces apart, no
/// wrapping. Cell colours are always rendered; [`crate::color::Out`] strips
/// them when colour is off. Headers are cleaned (SQL column names come from
/// the server); cells must be cleaned by the caller.
pub fn styled(headers: &[&str], boxed: bool) -> Table {
    let mut t = Table::new();
    if boxed {
        t.load_style(presets::UTF8_FULL_CONDENSED.with_rounded_corners())
            .set_content_arrangement(ContentArrangement::Dynamic);
    } else {
        t.load_style(presets::NOTHING).set_content_arrangement(ContentArrangement::Disabled);
    }
    t.enforce_styling().set_header(headers.iter().map(|h| Cell::new(clean(h)).add_attribute(Attribute::Bold)));
    t
}

/// A right-aligned cell, for numbers.
pub fn right(s: impl ToString) -> Cell {
    Cell::new(s).set_alignment(CellAlignment::Right)
}

/// The text of a SQL answer (both sides' `sql` command): a table with the
/// columns and JSON rows, then a dim summary line. Server text is cleaned.
pub fn sql_result(columns: &[&str], rows: &[Vec<Value>], truncated: bool, elapsed_ms: f64) -> String {
    let mut t = table(columns);
    for row in rows {
        t.add_row(row.iter().map(|v| match v {
            Value::String(s) => Cell::new(clean(s)),
            Value::Null => Cell::new("NULL").fg(Color::DarkGrey),
            Value::Number(n) => right(n),
            // Nested values print as JSON: strings in it are escaped.
            other => Cell::new(json_safe(&other.to_string())),
        }));
    }
    let more = if truncated { " (truncated; raise --limit or select fewer columns)" } else { "" };
    format!("{t}\n{}", dim(&format!("{} rows{more} in {}", rows.len(), ms(elapsed_ms))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::strip_ansi;

    #[test]
    fn sql_result_prints_each_json_type() {
        let evil = "x\x1b[2J";
        let rows = vec![vec![Value::from(1), Value::from(evil), Value::Null, serde_json::json!([evil])]];
        let s = sql_result(&["n", "s", "none", evil], &rows, true, 2.5);
        let plain = strip_ansi(&s);
        assert!(!plain.chars().any(|c| c.is_control() && c != '\n'), "{s:?}");
        assert!(plain.contains("NULL"), "{plain}");
        assert!(plain.contains("1 rows (truncated; raise --limit or select fewer columns) in 2.5ms"), "{plain}");
        assert!(sql_result(&["n"], &[], false, 1.0).contains("0 rows in 1.0ms"));
    }

    #[test]
    fn rounded_with_bold_headers() {
        let mut t = styled(&["ID", "NAME"], true);
        t.add_row(vec![right(1), Cell::new("web")]);
        let s = t.to_string();
        assert!(s.contains("\x1b[1m"), "headers are bold: {s:?}");
        let plain = strip_ansi(&s);
        assert!(plain.starts_with('╭') && plain.contains("web"), "{plain}");
        let evil = table(&["a\x1b]0;x\x07"]).to_string();
        assert!(!strip_ansi(&evil).chars().any(|c| c.is_control() && c != '\n'), "headers are cleaned: {evil:?}");
    }

    #[test]
    fn plain_when_piped() {
        let mut t = styled(&["ID", "NAME"], false);
        t.add_row(vec![right(1), Cell::new("web")]);
        t.add_row(vec![right(22), Cell::new("a very long name that would wrap in a narrow terminal ".repeat(4))]);
        let plain = strip_ansi(&t.to_string());
        assert_eq!(plain.lines().count(), 3, "one line per row: {plain}");
        assert!(!plain.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)), "no box drawing: {plain}");
        assert!(plain.lines().nth(1).unwrap().contains("1  web"), "{plain}");
    }
}
