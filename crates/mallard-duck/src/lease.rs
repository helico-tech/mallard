//! Which process runs maintenance when several share a catalog (ADR 0021).
//!
//! DuckLake has no unique keys, but a transaction that removes rows conflicts
//! with every concurrent write to that table, across processes. So replacing one
//! row elects one process, on every catalog. The lease saves work; safety must
//! not depend on it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use duckdb::{Connection, params};

use crate::is_conflict;

/// The key of the lease row in a side's `meta` table. The row must exist before
/// the first [`try_lease`]: a `DELETE` of no rows conflicts with nothing.
pub const LEASE: &str = "maintenance_lease";

/// Take or renew the lease for `lease_for`. `meta` is the full name of a table
/// `(key VARCHAR, value VARCHAR, updated_at TIMESTAMP)`. False when another
/// process holds it, or takes it at this moment: its `DELETE` of the row makes
/// the second commit conflict.
pub fn try_lease(conn: &Connection, meta: &str, holder: &str, lease_for: Duration) -> Result<bool> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as i64;
    let until = now.saturating_add(i64::try_from(lease_for.as_micros()).unwrap_or(i64::MAX));
    conn.execute_batch("BEGIN TRANSACTION")?;
    let attempt = (|| -> Result<bool> {
        let mut stmt = conn.prepare(&format!("SELECT value FROM {meta} WHERE key = ?"))?;
        let rows = stmt.query_map([LEASE], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let taken = rows
            .iter()
            .filter_map(|v| v.split_once(' '))
            .any(|(who, till)| who != holder && till.parse::<i64>().is_ok_and(|till| till > now));
        if taken {
            return Ok(false);
        }
        conn.execute(&format!("DELETE FROM {meta} WHERE key = ?"), [LEASE])?;
        conn.execute(
            &format!("INSERT INTO {meta} VALUES (?, ?, make_timestamp(?))"),
            params![LEASE, format!("{holder} {until}"), now],
        )?;
        conn.execute_batch("COMMIT")?;
        Ok(true)
    })();
    match attempt {
        Ok(true) => Ok(true),
        Ok(false) => {
            let _ = conn.execute_batch("ROLLBACK");
            Ok(false)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            if is_conflict(&e) { Ok(false) } else { Err(e) }
        }
    }
}
