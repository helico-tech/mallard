//! Read-only ad-hoc SQL (`POST /api/v1/sql`).
//!
//! Disabled by default and only available with API tokens configured. Even
//! then, a query is checked with DuckDB's own parser before it runs:
//! - exactly one statement, and it must be a SELECT (`json_serialize_sql`
//!   rejects everything else);
//! - table references must be lake tables (`spans`, `logs`, `metric_*`,
//!   optionally qualified as `lake.<schema>.`) or CTEs defined in the query.
//!   This blocks replacement scans such as `FROM '/etc/passwd'`;
//! - table functions must be on a small allowlist (no `read_*`, `glob`,
//!   `ducklake_*`, `duckdb_*`, `query`, scanners, ...);
//! - scalar functions on a denylist (`getenv`, `current_setting`, ...) are refused.
//! - every table and CTE name must be a plain identifier (letters, digits,
//!   `_`): names that look like paths or URLs could otherwise reach DuckDB's
//!   replacement scans (file reads, HTTP requests);
//! - `SHOW`/`DESCRIBE`/`SUMMARIZE` references are refused.
//!
//! Resource use (memory, CPU) is bounded by the query timeout and DuckDB's
//! memory limit only partly; treat SQL access as admin access.

use std::collections::HashSet;

use duckdb::{Connection, params};
use odl_api::{SqlColumn, SqlResponse};
use serde_json::Value as J;

use super::{QResult, bad};
use crate::store::{CATALOG, schema::TABLES};

const ALLOWED_TABLE_FUNCTIONS: &[&str] = &["range", "generate_series", "unnest", "json_each", "json_tree"];

const DENIED_SCALAR_PREFIXES: &[&str] = &[
    "getenv",
    "current_setting",
    "getvariable",
    "setseed",
    "nextval",
    "currval",
    "read_",
    "ducklake",
    "duckdb_",
    "pragma_",
    "which_secret",
    "load_",
    "install_",
    "http",
    "postgres",
    "sqlite",
    "mysql",
    "parquet_",
    "query",
    "sniff",
    "glob",
    "system",
    "enable_",
    "disable_",
    // Allocate outside DuckDB's buffer manager (memory blow-up).
    "repeat",
    "lpad",
    "rpad",
];

fn plain_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn validate(conn: &Connection, schema: &str, query: &str) -> QResult<()> {
    let ast: String = conn.query_row("SELECT json_serialize_sql(?::VARCHAR)", params![query], |r| r.get(0))?;
    let ast: J = serde_json::from_str(&ast).map_err(|e| bad(format!("cannot parse query: {e}")))?;
    if ast["error"].as_bool().unwrap_or(true) {
        let msg = ast["error_message"].as_str().unwrap_or("invalid query");
        return Err(bad(format!("only a single read-only SELECT is allowed: {msg}")));
    }
    let stmts = ast["statements"].as_array().map(Vec::len).unwrap_or(0);
    if stmts != 1 {
        return Err(bad("exactly one SELECT statement is allowed"));
    }
    let mut ctes = HashSet::new();
    collect_ctes(&ast, &mut ctes);
    if let Some(bad_cte) = ctes.iter().find(|c| c.starts_with('\u{0}')) {
        return Err(bad(format!(
            "CTE name {:?} is not allowed; use letters, digits and _",
            bad_cte.trim_start_matches("\u{0}invalid:")
        )));
    }
    let tables: HashSet<&str> = TABLES.iter().map(|t| t.name).collect();
    check(&ast, schema, &tables, &ctes)
}

fn collect_ctes(n: &J, out: &mut HashSet<String>) {
    match n {
        J::Object(m) => {
            if let Some(J::Array(entries)) = m.get("cte_map").and_then(|c| c.get("map")) {
                for e in entries {
                    if let Some(k) = e["key"].as_str() {
                        // Non-identifier CTE names are refused in `check`.
                        if plain_identifier(k) {
                            out.insert(k.to_ascii_lowercase());
                        } else {
                            out.insert(format!("\u{0}invalid:{k}"));
                        }
                    }
                }
            }
            m.values().for_each(|v| collect_ctes(v, out));
        }
        J::Array(a) => a.iter().for_each(|v| collect_ctes(v, out)),
        _ => {}
    }
}

fn check(n: &J, schema: &str, tables: &HashSet<&str>, ctes: &HashSet<String>) -> QResult<()> {
    match n {
        J::Object(m) => {
            match m.get("type").and_then(J::as_str) {
                Some("SHOW_REF") => {
                    return Err(bad("SHOW, DESCRIBE and SUMMARIZE are not allowed"));
                }
                Some("BASE_TABLE") => {
                    let name = m["table_name"].as_str().unwrap_or("").to_ascii_lowercase();
                    if !plain_identifier(&name) {
                        return Err(bad(format!("table name {name:?} is not allowed; query the lake tables")));
                    }
                    let sch = m["schema_name"].as_str().unwrap_or("").to_ascii_lowercase();
                    let cat = m["catalog_name"].as_str().unwrap_or("").to_ascii_lowercase();
                    let is_cte = cat.is_empty() && sch.is_empty() && ctes.contains(&name);
                    let is_lake = tables.contains(name.as_str())
                        && (cat.is_empty() || cat == CATALOG)
                        && (sch.is_empty() || sch == schema.to_ascii_lowercase());
                    if !is_cte && !is_lake {
                        return Err(bad(format!(
                            "table {name:?} is not allowed; query the lake tables ({})",
                            tables.iter().copied().collect::<Vec<_>>().join(", ")
                        )));
                    }
                }
                Some("TABLE_FUNCTION") => {
                    let f = m["function"]["function_name"].as_str().unwrap_or("").to_ascii_lowercase();
                    if !ALLOWED_TABLE_FUNCTIONS.contains(&f.as_str()) {
                        return Err(bad(format!("table function {f:?} is not allowed")));
                    }
                }
                _ => {}
            }
            if let Some(f) = m.get("function_name").and_then(J::as_str) {
                let f = f.to_ascii_lowercase();
                if DENIED_SCALAR_PREFIXES.iter().any(|p| f.starts_with(p)) {
                    return Err(bad(format!("function {f:?} is not allowed")));
                }
            }
            for v in m.values() {
                check(v, schema, tables, ctes)?;
            }
        }
        J::Array(a) => {
            for v in a {
                check(v, schema, tables, ctes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Validate and run; returns at most `limit` rows.
pub fn run(conn: &Connection, schema: &str, query: &str, limit: u32) -> QResult<SqlResponse> {
    let t0 = std::time::Instant::now();
    let query = query.trim().trim_end_matches(';');
    validate(conn, schema, query)?;
    let mut columns = Vec::new();
    {
        let mut stmt = conn.prepare(&format!("DESCRIBE (\n{query}\n)")).map_err(user_err)?;
        let rows = stmt.query_map([], |r| Ok(SqlColumn { name: r.get(0)?, data_type: r.get(1)? }))?;
        for c in rows {
            columns.push(c.map_err(user_err)?);
        }
    }
    let sql = format!("SELECT json_array(*COLUMNS(*))::VARCHAR FROM (\n{query}\n) LIMIT {}", limit as u64 + 1);
    let mut stmt = conn.prepare(&sql).map_err(user_err)?;
    let mut rows = Vec::new();
    let mut truncated = false;
    for r in stmt.query_map([], |r| r.get::<_, String>(0)).map_err(user_err)? {
        if rows.len() == limit as usize {
            truncated = true;
            break;
        }
        let text = r.map_err(user_err)?;
        let v: J = serde_json::from_str(&text)
            .or_else(|_| serde_json::from_str(&non_finite_to_null(&text)))
            .map_err(|e| bad(format!("cannot encode row as JSON: {e}")))?;
        rows.push(v.as_array().cloned().unwrap_or_default());
    }
    Ok(SqlResponse { columns, rows, truncated, elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0 })
}

/// Errors in a user's query are their mistake (400), not ours. The message
/// is DuckDB's, without our wrapper statement.
fn user_err(e: duckdb::Error) -> super::QueryError {
    let msg = e.to_string();
    let msg = msg.split("\nLINE ").next().unwrap_or(&msg).to_string();
    if msg.contains("INTERRUPT") || msg.contains("Interrupted") {
        return super::QueryError::Internal(anyhow::anyhow!(msg));
    }
    bad(msg)
}

/// DuckDB writes NaN/Infinity as bare tokens; JSON has no such values.
fn non_finite_to_null(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escaped = false;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        let mut replaced = false;
        for tok in ["-Infinity", "Infinity", "NaN", "-nan", "nan", "-inf", "inf"] {
            if rest.starts_with(tok) {
                out.push_str("null");
                rest = &rest[tok.len()..];
                replaced = true;
                break;
            }
        }
        if !replaced {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nan_tokens() {
        assert_eq!(non_finite_to_null(r#"[NaN,"NaN",1,-Infinity]"#), r#"[null,"NaN",1,null]"#);
    }

    #[test]
    fn validation() {
        let c = Connection::open_in_memory().unwrap();
        let ok = |q: &str| validate(&c, "otel", q);
        assert!(ok("SELECT count(*) FROM spans").is_ok());
        assert!(ok("SELECT * FROM lake.otel.logs l JOIN spans s USING (trace_id)").is_ok());
        assert!(ok("WITH x AS (SELECT * FROM spans) SELECT * FROM x, range(3)").is_ok());
        assert!(ok("SELECT * FROM range(10)").is_ok());
        assert!(ok("DELETE FROM spans").is_err());
        assert!(ok("SELECT 1; SELECT 2").is_err());
        assert!(ok("SELECT * FROM '/etc/passwd'").is_err());
        assert!(ok("SELECT * FROM read_csv('/etc/passwd')").is_err());
        assert!(ok("SELECT * FROM ducklake_cleanup_old_files('lake')").is_err());
        assert!(ok("SELECT getenv('HOME')").is_err());
        assert!(ok("SELECT * FROM other.otel.spans").is_err());
        assert!(ok("SELECT * FROM duckdb_secrets()").is_err());
        assert!(ok("SELECT * FROM lake.otel.spans, (SELECT * FROM read_text('x'))").is_err());
        // CTE scoping tricks that would reach replacement scans (review finding)
        assert!(
            ok(r#"WITH "/etc/passwd.csv" AS (SELECT * FROM '/etc/passwd.csv') SELECT * FROM "/etc/passwd.csv""#)
                .is_err()
        );
        assert!(ok(r#"SELECT * FROM (WITH "/x.csv" AS (SELECT 1) SELECT 1), '/x.csv'"#).is_err());
        assert!(
            ok(r#"WITH "http://169.254.169.254/x?.csv" AS (SELECT 1) SELECT * FROM "http://169.254.169.254/x?.csv""#)
                .is_err()
        );
        assert!(ok("SHOW ALL TABLES").is_err());
        assert!(ok("SELECT length(repeat('x', 2000000000))").is_err());
        assert!(ok("WITH recent AS (SELECT * FROM spans) SELECT count(*) FROM recent").is_ok());
    }
}
