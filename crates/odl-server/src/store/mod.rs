//! DuckLake storage: attach, schema migration, connections.

pub mod maintenance;
pub mod schema;
pub mod writer;

use std::{path::Path, sync::Mutex};

use anyhow::{Context, Result, bail};
use duckdb::Connection;
use tracing::{info, warn};

use crate::config::{CatalogKind, Config, is_identifier, parse_bytes};
use schema::{TABLES, TableDef};

/// Name under which the lake is attached in DuckDB.
pub const CATALOG: &str = "lake";

/// The attached lake. Hands out connections that share one DuckDB instance.
pub struct Lake {
    root: Mutex<Connection>,
    pub schema: String,
    pub catalog_kind: CatalogKind,
    pub data_path: String,
}

/// Quote a string literal for SQL.
pub fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

impl Lake {
    pub fn open(cfg: &Config) -> Result<Self> {
        Self::with_connection(open_connection(cfg)?, cfg)
    }

    /// Use a connection prepared by the embedding application. The DuckLake
    /// catalog must already be attached under the name [`CATALOG`] (`lake`).
    /// Only the schema and tables are created; DuckDB settings, extensions,
    /// secrets and DuckLake options are left to the caller.
    pub fn with_connection(conn: Connection, cfg: &Config) -> Result<Self> {
        let attached: i64 = conn.query_row(
            "SELECT count(*) FROM duckdb_databases() WHERE database_name = ?",
            [CATALOG],
            |r| r.get(0),
        )?;
        if attached == 0 {
            bail!(
                "no database attached as {CATALOG:?}; ATTACH the DuckLake catalog AS {CATALOG} first"
            );
        }
        let lake = Self {
            root: Mutex::new(conn),
            schema: cfg.storage.schema.clone(),
            catalog_kind: cfg.storage.catalog_kind(),
            data_path: cfg.storage.data_path.clone(),
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

    pub fn table(&self, t: &TableDef) -> String {
        format!("{CATALOG}.{}.{}", self.schema, t.name)
    }

    /// Create schema and tables; add columns that were appended to a table
    /// definition since the table was created.
    fn migrate(&self) -> Result<()> {
        let conn = self.root.lock().unwrap();
        conn.execute_batch(&format!(
            "CREATE SCHEMA IF NOT EXISTS {CATALOG}.{}",
            self.schema
        ))?;
        for t in TABLES {
            conn.execute_batch(&t.create_sql(CATALOG, &self.schema))
                .with_context(|| format!("creating table {}", t.name))?;
            let mut stmt = conn.prepare(
                "SELECT column_name FROM duckdb_columns() \
                 WHERE database_name = ? AND schema_name = ? AND table_name = ? ORDER BY column_index",
            )?;
            let existing: Vec<String> = stmt
                .query_map([CATALOG, self.schema.as_str(), t.name], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for (i, (name, ty)) in t.columns.iter().enumerate() {
                match existing.get(i) {
                    Some(e) if e == name => {}
                    Some(e) => bail!(
                        "table {} column {i} is {e:?} but the schema expects {name:?}; \
                         the table was changed outside otel-ducklake",
                        t.name
                    ),
                    None => {
                        info!(table = t.name, column = name, "adding column");
                        conn.execute_batch(&format!(
                            "ALTER TABLE {CATALOG}.{}.{} ADD COLUMN {name} {ty}",
                            self.schema, t.name
                        ))?;
                    }
                }
            }
            if existing.len() > t.columns.len() {
                bail!(
                    "table {} has more columns than this version knows; refusing to write",
                    t.name
                );
            }
        }
        Ok(())
    }
}

/// Open an in-memory DuckDB, apply `[duckdb]` settings, load extensions,
/// create secrets and attach the DuckLake catalog as [`CATALOG`]. Creates no
/// schema or tables. Embedders can share the returned connection (via
/// `try_clone`) between several applications using different schemas.
pub fn open_connection(cfg: &Config) -> Result<Connection> {
    let conn = Connection::open_in_memory().context("opening DuckDB")?;
    configure_duckdb(&conn, cfg)?;
    load_extensions(&conn, cfg)?;
    create_secrets(&conn, cfg)?;
    attach(&conn, cfg)?;
    Ok(conn)
}

/// Install (download) extensions into the configured extension directory.
pub fn install_extensions(cfg: &Config, names: &[String]) -> Result<()> {
    let conn = Connection::open_in_memory()?;
    configure_duckdb(&conn, cfg)?;
    for n in names {
        if !is_identifier(n) {
            bail!("invalid extension name {n:?}");
        }
        conn.execute_batch(&format!("INSTALL {n}"))
            .with_context(|| format!("installing {n}"))?;
        info!(extension = %n, "installed");
    }
    Ok(())
}

fn configure_duckdb(conn: &Connection, cfg: &Config) -> Result<()> {
    let d = &cfg.duckdb;
    let mut sql = String::new();
    if d.threads > 0 {
        sql += &format!("SET threads = {};", d.threads);
    }
    if !d.memory_limit.is_empty() {
        sql += &format!(
            "SET memory_limit = {};",
            sql_str(&format!("{}B", parse_bytes(&d.memory_limit)?))
        );
    }
    if !d.temp_directory.is_empty() {
        sql += &format!("SET temp_directory = {};", sql_str(&d.temp_directory));
    }
    if !d.extension_directory.is_empty() {
        sql += &format!(
            "SET extension_directory = {};",
            sql_str(&d.extension_directory)
        );
    }
    if !d.autoinstall_extensions {
        sql += "SET autoinstall_known_extensions = false;";
    }
    // Telemetry tables are append-mostly; keep insertion order cheap.
    sql += "SET preserve_insertion_order = false;";
    conn.execute_batch(&sql).context("configuring DuckDB")
}

/// Extensions needed for this config, in load order.
pub fn required_extensions(cfg: &Config) -> Vec<String> {
    let mut exts = vec!["ducklake".to_string()];
    match cfg.storage.catalog_kind() {
        CatalogKind::Postgres => exts.push("postgres".into()),
        CatalogKind::Sqlite => exts.push("sqlite".into()),
        CatalogKind::MySql => exts.push("mysql".into()),
        CatalogKind::DuckDb => {}
    }
    let dp = cfg.storage.data_path.to_ascii_lowercase();
    if [
        "s3://", "s3a://", "s3n://", "gs://", "gcs://", "r2://", "http://", "https://", "hf://",
    ]
    .iter()
    .any(|p| dp.starts_with(p))
    {
        exts.push("httpfs".into());
    }
    if ["az://", "azure://", "abfss://"]
        .iter()
        .any(|p| dp.starts_with(p))
    {
        exts.push("azure".into());
    }
    for s in &cfg.storage.secrets {
        if let Some(toml::Value::String(t)) = s.options.get("type") {
            match t.to_ascii_lowercase().as_str() {
                "s3" | "gcs" | "r2" | "huggingface" if !exts.contains(&"httpfs".to_string()) => {
                    exts.push("httpfs".into())
                }
                "azure" if !exts.contains(&"azure".to_string()) => exts.push("azure".into()),
                _ => {}
            }
        }
        if matches!(s.options.get("provider"), Some(toml::Value::String(p)) if p == "credential_chain")
            && !exts.contains(&"aws".to_string())
        {
            exts.push("aws".into());
        }
    }
    for e in &cfg.storage.extensions {
        if !exts.contains(e) {
            exts.push(e.clone());
        }
    }
    exts
}

fn load_extensions(conn: &Connection, cfg: &Config) -> Result<()> {
    for e in required_extensions(cfg) {
        if !is_identifier(&e) {
            bail!("invalid extension name {e:?}");
        }
        if conn.execute_batch(&format!("LOAD {e}")).is_ok() {
            continue;
        }
        if !cfg.duckdb.autoinstall_extensions {
            bail!("extension {e} is not installed and duckdb.autoinstall_extensions = false");
        }
        conn.execute_batch(&format!("INSTALL {e}; LOAD {e};"))
            .with_context(|| format!("installing DuckDB extension {e}"))?;
    }
    Ok(())
}

fn create_secrets(conn: &Connection, cfg: &Config) -> Result<()> {
    for (i, s) in cfg.storage.secrets.iter().enumerate() {
        let name = s.name.clone().unwrap_or_else(|| format!("odl_secret_{i}"));
        if !is_identifier(&name) {
            bail!("storage.secrets[{i}].name must be an identifier");
        }
        let mut opts = Vec::new();
        for (k, v) in &s.options {
            if !is_identifier(k) {
                bail!("storage.secrets[{i}]: invalid option name {k:?}");
            }
            let val = match v {
                toml::Value::String(s) if k == "type" || k == "provider" => {
                    if !is_identifier(s) {
                        bail!("storage.secrets[{i}].{k} must be an identifier");
                    }
                    s.clone()
                }
                toml::Value::String(s) => sql_str(s),
                toml::Value::Boolean(b) => b.to_string(),
                toml::Value::Integer(n) => n.to_string(),
                toml::Value::Float(n) => n.to_string(),
                toml::Value::Array(a) => {
                    let items: Result<Vec<String>> = a
                        .iter()
                        .map(|x| match x {
                            toml::Value::String(s) => Ok(sql_str(s)),
                            _ => bail!("storage.secrets[{i}].{k}: only string lists are supported"),
                        })
                        .collect();
                    format!("[{}]", items?.join(", "))
                }
                _ => bail!("storage.secrets[{i}].{k}: unsupported value type"),
            };
            opts.push(format!("{} {val}", k.to_ascii_uppercase()));
        }
        conn.execute_batch(&format!(
            "CREATE OR REPLACE SECRET {name} ({})",
            opts.join(", ")
        ))
        .with_context(|| format!("creating secret {name}"))?;
    }
    Ok(())
}

fn ensure_local_dir(path: &str) -> Result<()> {
    if path.contains("://") || path.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(path).with_context(|| format!("creating directory {path}"))
}

fn attach(conn: &Connection, cfg: &Config) -> Result<()> {
    let st = &cfg.storage;
    match st.catalog_kind() {
        CatalogKind::DuckDb => {
            if let Some(parent) = Path::new(&st.catalog).parent() {
                ensure_local_dir(&parent.to_string_lossy())?;
            }
        }
        CatalogKind::Sqlite => {
            let file = st.catalog.trim_start_matches("sqlite:");
            if let Some(parent) = Path::new(file).parent() {
                ensure_local_dir(&parent.to_string_lossy())?;
            }
        }
        _ => {}
    }
    ensure_local_dir(&st.data_path)?;
    let sql = format!(
        "ATTACH IF NOT EXISTS {} AS {CATALOG} (DATA_PATH {})",
        sql_str(&format!("ducklake:{}", st.catalog)),
        sql_str(&st.data_path),
    );
    conn.execute_batch(&sql).with_context(|| {
        format!(
            "attaching DuckLake catalog ({}) with data path {}",
            st.catalog_kind().as_str(),
            st.data_path
        )
    })?;
    let opts = [
        ("parquet_compression", sql_str("zstd")),
        ("parquet_row_group_size", st.row_group_size.to_string()),
        (
            "target_file_size",
            sql_str(&format!("{}B", parse_bytes(&st.target_file_size)?)),
        ),
        (
            "data_inlining_row_limit",
            st.data_inlining_row_limit.to_string(),
        ),
    ];
    for (k, v) in opts {
        if let Err(e) =
            conn.execute_batch(&format!("CALL {CATALOG}.set_option({}, {v})", sql_str(k)))
        {
            warn!(option = k, error = %e, "could not set DuckLake option");
        }
    }
    info!(
        catalog = st.catalog_kind().as_str(),
        data_path = %st.data_path,
        "attached DuckLake"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_for_config() {
        let mut cfg = Config::default();
        assert_eq!(required_extensions(&cfg), vec!["ducklake"]);
        cfg.storage.catalog = "postgres:host=x".into();
        cfg.storage.data_path = "s3://b/".into();
        assert_eq!(
            required_extensions(&cfg),
            vec!["ducklake", "postgres", "httpfs"]
        );
        cfg.storage.data_path = "abfss://c@acct.dfs.core.windows.net/x/".into();
        assert!(required_extensions(&cfg).contains(&"azure".to_string()));
    }

    #[test]
    fn quotes() {
        assert_eq!(sql_str("a'b"), "'a''b'");
    }
}
