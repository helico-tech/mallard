//! `mallard schema`: the tables `mallard sql` can query, of both sides.
//! `side` says which: `errors` tables need `mallard sql --errors` (ADR 0011).

use anyhow::Result;
use comfy_table::{Cell, Color};
use mallard_term::output::List;
use mallard_term::table::table;
use mallard_term::text::clean;
use serde::Serialize;

use crate::{Run, described};

#[derive(Debug, Serialize)]
struct Table {
    side: &'static str,
    name: String,
    time_column: String,
    columns: Vec<Column>,
}

#[derive(Debug, Serialize)]
struct Column {
    name: String,
    #[serde(rename = "type")]
    data_type: String,
}

pub(crate) async fn run(r: &Run) -> Result<()> {
    let c = &r.client;
    let (telemetry, errors) = tokio::join!(c.otel.schema(), c.sentry.schema());
    let telemetry = telemetry.map_err(|e| described(e.into(), c.base_url()))?;
    let errors = errors.map_err(|e| described(e.into(), c.base_url()))?;
    let mut items: Vec<Table> = telemetry
        .tables
        .into_iter()
        .map(|t| Table {
            side: "telemetry",
            name: t.name,
            time_column: t.time_column,
            columns: t.columns.into_iter().map(|c| Column { name: c.name, data_type: c.data_type }).collect(),
        })
        .collect();
    items.extend(errors.tables.into_iter().map(|t| Table {
        side: "errors",
        name: t.name,
        time_column: t.time_column,
        columns: t.columns.into_iter().map(|c| Column { name: c.name, data_type: c.data_type }).collect(),
    }));
    let list = List::new(items);
    if r.output.text() {
        r.output.out.line(&text(&list.items))?;
        r.output.truncation_note(&list);
        Ok(())
    } else {
        r.output.list(&list)
    }
}

/// One row per column; the time column of each table is marked.
fn text(tables: &[Table]) -> String {
    let mut t = table(&["side", "table", "column", "type", ""]);
    for tb in tables {
        for c in &tb.columns {
            t.add_row(vec![
                Cell::new(tb.side),
                Cell::new(clean(&tb.name)).fg(Color::White),
                Cell::new(clean(&c.name)),
                Cell::new(clean(&c.data_type)),
                Cell::new(if c.name == tb.time_column { "time" } else { "" }).fg(Color::Cyan),
            ]);
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_marks_side_and_time_column_and_cleans_server_text() {
        let tables = [Table {
            side: "errors",
            name: "events".into(),
            time_column: "received_at".into(),
            columns: vec![
                Column { name: "received_at".into(), data_type: "TIMESTAMP".into() },
                Column { name: "x\x1b[2J".into(), data_type: "VARCHAR".into() },
            ],
        }];
        let s = mallard_term::color::strip_ansi(&text(&tables));
        let row = s.lines().find(|l| l.contains("received_at")).unwrap();
        assert!(row.contains("errors") && row.contains("events") && row.contains("time"), "{s}");
        assert!(!s.contains('\x1b'), "{s}");
    }
}
