//! Storage statistics from the DuckLake catalog metadata.

use mallard_duck::duckdb::{Connection, params};
use mallard_otel_api::TableStats;

use super::QResult;
use crate::store::{CATALOG, schema::TABLES};

/// Live file counts/sizes per table and rows still inlined in the catalog.
pub fn tables(conn: &Connection, schema: &str) -> QResult<(i64, Vec<TableStats>)> {
    let meta = format!("__ducklake_metadata_{CATALOG}");
    let snapshot: i64 =
        conn.query_row(&format!("SELECT id::BIGINT FROM {CATALOG}.current_snapshot()"), [], |r| r.get(0))?;
    let sql = format!(
        "SELECT t.table_name, t.table_id, \
            coalesce(sum(f.record_count), 0)::BIGINT, count(f.data_file_id)::BIGINT, \
            coalesce(sum(f.file_size_bytes), 0)::BIGINT \
         FROM {meta}.ducklake_table t \
         JOIN {meta}.ducklake_schema s ON s.schema_id = t.schema_id AND s.end_snapshot IS NULL \
         LEFT JOIN {meta}.ducklake_data_file f ON f.table_id = t.table_id AND f.end_snapshot IS NULL \
         WHERE t.end_snapshot IS NULL AND s.schema_name = ? \
         GROUP BY 1, 2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let found: Vec<(String, i64, i64, i64, i64)> = stmt
        .query_map(params![schema], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for t in TABLES {
        let Some((_, id, rows, files, bytes)) = found.iter().find(|f| f.0 == t.name) else {
            continue;
        };
        out.push(TableStats {
            name: t.name.to_string(),
            rows: *rows,
            files: *files,
            bytes: *bytes,
            inlined_rows: inlined_rows(conn, &meta, *id).unwrap_or(0),
        });
    }
    Ok((snapshot, out))
}

fn inlined_rows(conn: &Connection, meta: &str, table_id: i64) -> mallard_duck::duckdb::Result<i64> {
    let mut stmt =
        conn.prepare(&format!("SELECT table_name FROM {meta}.ducklake_inlined_data_tables WHERE table_id = ?"))?;
    let names: Vec<String> = stmt.query_map(params![table_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut total = 0;
    for n in names {
        // Inlined rows that are still live (not yet flushed or deleted).
        let q =
            format!("SELECT count(*) FROM {meta}.\"{}\" WHERE _ducklake_end_snapshot IS NULL", n.replace('"', "\"\""));
        let c: i64 = conn
            .query_row(&q, [], |r| r.get(0))
            .or_else(|_| conn.query_row(&q.replace("_ducklake_end_snapshot", "end_snapshot"), [], |r| r.get(0)))?;
        total += c;
    }
    Ok(total)
}
