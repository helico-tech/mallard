//! DuckLake storage: attach, schema migration, connections.

pub mod maintenance;
pub mod schema;
pub mod writer;

use std::sync::Mutex;

use anyhow::{Context, Result, bail};
pub(crate) use mallard_duck::{CATALOG, redact, sql_str};
use mallard_duck::{CatalogKind, duckdb::Connection};
use tracing::{info, warn};

use crate::config::Config;
use schema::TABLES;

/// DuckLake schema that holds the tables.
pub const SCHEMA: &str = "otel";

/// The attached lake. Hands out connections that share one DuckDB instance.
pub struct Lake {
    root: Mutex<Connection>,
    pub schema: String,
    pub catalog_kind: CatalogKind,
    pub data_path: String,
    pub(crate) commit_lock: mallard_duck::CommitLock,
}

/// True when `e` is DuckLake's transaction conflict with a concurrent
/// writer (retrying helps). DuckDB reports it only in the message text.
pub(crate) fn is_conflict(e: &anyhow::Error) -> bool {
    mallard_duck::is_conflict(e)
}

/// The table of the maintenance lease, and its row: outside `TABLES`, because it
/// holds no telemetry (no retention, no compaction, not in the query API).
fn create_meta(conn: &Connection, schema: &str) -> Result<()> {
    let meta = format!("{CATALOG}.{schema}.meta");
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {meta} (key VARCHAR NOT NULL, value VARCHAR NOT NULL, updated_at TIMESTAMP NOT NULL)"
    ))?;
    let rows: i64 =
        conn.query_row(&format!("SELECT count(*) FROM {meta} WHERE key = ?"), [mallard_duck::LEASE], |r| r.get(0))?;
    if rows == 0 {
        conn.execute(&format!("INSERT INTO {meta} VALUES (?, '', now()::TIMESTAMP)"), [mallard_duck::LEASE])?;
    }
    Ok(())
}

impl Lake {
    pub fn open(cfg: &Config) -> Result<Self> {
        Self::with_connection(mallard_duck::open_connection(&cfg.storage, &cfg.duckdb)?, cfg)
    }

    /// Use a connection prepared by the embedding application. The DuckLake
    /// catalog must already be attached under the name [`CATALOG`] (`lake`).
    /// Only the schema and tables are created; DuckDB settings, extensions,
    /// secrets and DuckLake options are left to the caller.
    pub fn with_connection(conn: Connection, cfg: &Config) -> Result<Self> {
        let attached: i64 =
            conn.query_row("SELECT count(*) FROM duckdb_databases() WHERE database_name = ?", [CATALOG], |r| r.get(0))?;
        if attached == 0 {
            bail!("no database attached as {CATALOG:?}; ATTACH the DuckLake catalog AS {CATALOG} first");
        }
        let lake = Self {
            root: Mutex::new(conn),
            schema: SCHEMA.into(),
            catalog_kind: cfg.storage.catalog_kind(),
            data_path: cfg.storage.data_path.clone(),
            commit_lock: cfg.commit_lock.clone(),
        };
        lake.migrate()?;
        Ok(lake)
    }

    /// A new connection to the same database, with the lake schema as default.
    pub fn connect(&self) -> Result<Connection> {
        let c = self.root.lock().unwrap().try_clone()?;
        c.execute_batch(&format!("USE {CATALOG}.{}", self.schema))?;
        Ok(c)
    }

    /// Create schema and tables. Several replicas starting at once on a shared
    /// catalog race to create them; DuckLake then reports a transaction
    /// conflict for the loser. Every step is idempotent, so retry.
    fn migrate(&self) -> Result<()> {
        let mut attempt = 0u32;
        loop {
            match self.migrate_once() {
                Ok(()) => return Ok(()),
                Err(e) if attempt < 10 && is_conflict(&e) => {
                    attempt += 1;
                    warn!(attempt, error = %format!("{e:#}"), "schema migration conflicted with another writer; retrying");
                    std::thread::sleep(std::time::Duration::from_millis(100 * attempt as u64));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Create schema and tables; add columns that were appended to a table
    /// definition since the table was created.
    fn migrate_once(&self) -> Result<()> {
        let conn = self.root.lock().unwrap();
        conn.execute_batch(&format!("CREATE SCHEMA IF NOT EXISTS {CATALOG}.{}", self.schema))?;
        create_meta(&conn, &self.schema)?;
        for t in TABLES {
            conn.execute_batch(&t.create_sql(CATALOG, &self.schema))
                .with_context(|| format!("creating table {}", t.name))?;
            let mut stmt = conn.prepare(
                "SELECT column_name FROM duckdb_columns() \
                 WHERE database_name = ? AND schema_name = ? AND table_name = ? ORDER BY column_index",
            )?;
            let existing: Vec<String> =
                stmt.query_map([CATALOG, self.schema.as_str(), t.name], |r| r.get(0))?.collect::<Result<_, _>>()?;
            // Columns are matched by name: we only ever add missing ones.
            // Extra columns come from a newer version (rolling upgrade or
            // rollback); we leave them NULL.
            for (name, ty) in t.columns.iter() {
                if !existing.iter().any(|e| e == name) {
                    info!(table = t.name, column = name, "adding column");
                    conn.execute_batch(&format!(
                        "ALTER TABLE {CATALOG}.{}.{} ADD COLUMN {name} {ty}",
                        self.schema, t.name
                    ))?;
                }
            }
            let extra: Vec<&String> =
                existing.iter().filter(|e| !t.columns.iter().any(|c| c.0 == e.as_str())).collect();
            if !extra.is_empty() {
                warn!(table = t.name, columns = ?extra, "table has columns this version does not know; they stay NULL for rows written here");
            }
        }
        Ok(())
    }
}
