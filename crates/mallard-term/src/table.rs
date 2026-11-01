//! The one table style of both CLIs.

use comfy_table::{Attribute, Cell, CellAlignment, ContentArrangement, Table, presets};

/// Rounded, condensed UTF-8 table with bold headers that fits the terminal
/// width. Cell colours are always rendered; [`crate::color::Out`] strips
/// them when colour is off.
pub fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_style(presets::UTF8_FULL_CONDENSED.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .enforce_styling()
        .set_header(headers.iter().map(|h| Cell::new(h).add_attribute(Attribute::Bold)));
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
        let mut t = table(&["ID", "NAME"]);
        t.add_row(vec![right(1), Cell::new("web")]);
        let s = t.to_string();
        assert!(s.contains("\x1b[1m"), "headers are bold: {s:?}");
        let plain = strip_ansi(&s);
        assert!(plain.starts_with('╭') && plain.contains("web"), "{plain}");
    }
}
