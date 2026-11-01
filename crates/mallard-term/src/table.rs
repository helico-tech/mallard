//! The one table style of both CLIs: boxed on a terminal, plain when piped.

use std::io::IsTerminal;

use comfy_table::{Attribute, Cell, CellAlignment, ContentArrangement, Table, presets};

use crate::text::clean;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::strip_ansi;

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
