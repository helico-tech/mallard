//! Read-only ad-hoc SQL (`POST /api/v1/sql`).
//!
//! Disabled by default and only available with API tokens configured. The
//! query is checked and run by `mallard_duck::sql`, shared with the Sentry
//! side (ADR 0005 here, mallard ADR 0011): one SELECT over the lake tables
//! (`spans`, `logs`, `metric_*`), no file or network access, no settings.
//!
//! Resource use (memory, CPU) is bounded by the query timeout and DuckDB's
//! memory limit only partly; treat SQL access as admin access.

use mallard_duck::duckdb::Connection;
use mallard_duck::sql::SqlError;
use odl_api::{SqlColumn, SqlResponse};

use super::{QResult, QueryError};
use crate::store::schema::TABLES;

/// Validate and run; returns at most `limit` rows.
pub fn run(conn: &Connection, schema: &str, query: &str, limit: u32) -> QResult<SqlResponse> {
    let tables: Vec<&str> = TABLES.iter().map(|t| t.name).collect();
    let r = mallard_duck::sql::run(conn, schema, &tables, query, limit).map_err(|e| match e {
        SqlError::Rejected(m) => QueryError::BadRequest(m),
        SqlError::Internal(e) => QueryError::Internal(e),
    })?;
    Ok(SqlResponse {
        columns: r.columns.into_iter().map(|(name, data_type)| SqlColumn { name, data_type }).collect(),
        rows: r.rows,
        truncated: r.truncated,
        elapsed_ms: r.elapsed_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard itself is tested in mallard-duck; here: the otel tables.
    #[test]
    fn lake_tables_are_allowed() {
        let c = Connection::open_in_memory().unwrap();
        let tables: Vec<&str> = TABLES.iter().map(|t| t.name).collect();
        let ok = |q: &str| mallard_duck::sql::validate(&c, "otel", &tables, q);
        assert!(ok("SELECT * FROM lake.otel.logs l JOIN spans s USING (trace_id)").is_ok());
        assert!(ok("SELECT * FROM metric_points").is_ok());
        assert!(ok("SELECT * FROM lake.sentry.events").is_err());
    }
}
