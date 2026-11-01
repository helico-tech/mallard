//! DuckDB and DuckLake set-up shared by both sides of mallard.
//!
//! This is the only crate that depends on `duckdb`: everything else uses
//! [`duckdb`] re-exported from here, so the workspace links one version.
//!
//! [`open_connection`] opens an in-memory DuckDB, applies `[duckdb]`,
//! loads extensions, creates the `[[storage.secrets]]` and attaches the
//! DuckLake catalog as [`CATALOG`]. It creates no schema or tables: each
//! side does that on a `try_clone()` of the connection (ADR 0001).
//!
//! [`sql`] checks and runs read-only ad-hoc SQL for both sides' SQL
//! endpoints (ADR 0011). [`try_lease`] elects the process that runs maintenance
//! (ADR 0021).

mod config;
mod lease;
pub mod sql;

use std::path::Path;

use anyhow::{Context, Result, bail};
pub use config::{CatalogKind, DuckDbConfig, SecretConfig, StorageConfig};
pub use duckdb;
use duckdb::Connection;
pub use lease::{LEASE, try_lease};
use tracing::{info, warn};

/// Name under which the lake is attached.
pub const CATALOG: &str = "lake";

/// Every extension some config may load. Container images install these
/// so the server runs without internet access. `json` and `parquet` are
/// compiled in.
pub const ALL_EXTENSIONS: &[&str] = &["ducklake", "httpfs", "aws", "azure", "postgres", "sqlite", "mysql"];

/// Quote a string literal for SQL.
pub fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A plain SQL identifier: ASCII letters, digits and `_`, not starting with a digit.
pub fn is_identifier(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Remove credentials from text (errors, catalog strings for logs). DuckDB's
/// postgres/mysql extensions echo the full connection string, including
/// `password=...`, in their errors.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let lower = text.to_ascii_lowercase();
    let mut i = 0;
    while i < text.len() {
        let hit = ["password=", "passwd=", "secret=", "key_id="]
            .iter()
            .filter_map(|k| lower[i..].find(k).map(|p| (i + p, k.len())))
            .min_by_key(|(p, _)| *p);
        let Some((pos, klen)) = hit else {
            out.push_str(&text[i..]);
            break;
        };
        out.push_str(&text[i..pos + klen]);
        let rest = &text[pos + klen..];
        let end = match rest.chars().next() {
            Some(q @ ('\'' | '"')) => rest[1..].find(q).map(|e| e + 2).unwrap_or(rest.len()),
            _ => rest.find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ';').unwrap_or(rest.len()),
        };
        out.push_str("***");
        i = pos + klen + end;
    }
    out
}

/// Open an in-memory DuckDB, apply `duckdb` settings, load extensions,
/// create secrets and attach the DuckLake catalog as [`CATALOG`].
pub fn open_connection(storage: &StorageConfig, duckdb: &DuckDbConfig) -> Result<Connection> {
    let conn = Connection::open_in_memory().context("opening DuckDB")?;
    configure(&conn, duckdb)?;
    load_extensions(&conn, storage, duckdb.autoinstall_extensions)?;
    create_secrets(&conn, storage)?;
    attach(&conn, storage)?;
    Ok(conn)
}

/// Download extensions into `duckdb.extension_directory` (see [`ALL_EXTENSIONS`]).
pub fn install_extensions(duckdb: &DuckDbConfig, names: &[impl AsRef<str>]) -> Result<()> {
    if !duckdb.extension_directory.is_empty() {
        std::fs::create_dir_all(&duckdb.extension_directory)
            .with_context(|| format!("creating {}", duckdb.extension_directory))?;
    }
    let conn = Connection::open_in_memory()?;
    configure(&conn, duckdb)?;
    for n in names {
        let n = n.as_ref();
        if !is_identifier(n) {
            bail!("invalid extension name {n:?}");
        }
        conn.execute_batch(&format!("INSTALL {n}")).with_context(|| format!("installing {n}"))?;
        info!(extension = %n, "installed");
    }
    Ok(())
}

/// Global DuckDB settings. Only for a connection this process owns.
fn configure(conn: &Connection, d: &DuckDbConfig) -> Result<()> {
    let mut sql = String::new();
    if d.threads > 0 {
        sql += &format!("SET threads = {};", d.threads);
    }
    if let Some(limit) = d.memory_limit {
        sql += &format!("SET memory_limit = '{}B';", limit.0);
    }
    if !d.temp_directory.is_empty() {
        sql += &format!("SET temp_directory = {};", sql_str(&d.temp_directory));
    }
    if !d.extension_directory.is_empty() {
        sql += &format!("SET extension_directory = {};", sql_str(&d.extension_directory));
    }
    if !d.autoinstall_extensions {
        sql += "SET autoinstall_known_extensions = false;";
    }
    // Neither side relies on insertion order (sorted tables sort
    // explicitly); this lets flushes and compaction stream within a
    // memory limit.
    sql += "SET preserve_insertion_order = false;";
    conn.execute_batch(&sql).context("configuring DuckDB")
}

/// Extensions needed for this storage config, in load order.
pub fn required_extensions(st: &StorageConfig) -> Vec<String> {
    let mut exts = vec!["ducklake".to_string()];
    let mut add = |e: &str| {
        if !exts.iter().any(|x| x == e) {
            exts.push(e.to_string());
        }
    };
    match st.catalog_kind() {
        CatalogKind::Postgres => add("postgres"),
        CatalogKind::Sqlite => add("sqlite"),
        CatalogKind::MySql => add("mysql"),
        CatalogKind::DuckDb => {}
    }
    let dp = st.data_path.to_ascii_lowercase();
    if ["s3://", "s3a://", "s3n://", "gs://", "gcs://", "r2://", "http://", "https://", "hf://"]
        .iter()
        .any(|p| dp.starts_with(p))
    {
        add("httpfs");
    }
    if ["az://", "azure://", "abfss://"].iter().any(|p| dp.starts_with(p)) {
        add("azure");
    }
    for s in &st.secrets {
        let ty = s.get("type").unwrap_or_default().to_ascii_lowercase();
        match ty.as_str() {
            "s3" | "gcs" | "r2" | "huggingface" => {
                add("httpfs");
                // IAM roles / IRSA / profiles are read by the aws extension.
                if s.get("provider") == Some("credential_chain") && ty != "huggingface" {
                    add("aws");
                }
            }
            "azure" => add("azure"),
            _ => {}
        }
    }
    for e in &st.extensions {
        add(e);
    }
    exts
}

fn load_extensions(conn: &Connection, st: &StorageConfig, autoinstall: bool) -> Result<()> {
    for e in required_extensions(st) {
        if !is_identifier(&e) {
            bail!("invalid extension name {e:?}");
        }
        if conn.execute_batch(&format!("LOAD {e}")).is_ok() {
            continue;
        }
        if !autoinstall {
            bail!("extension {e} is not installed and duckdb.autoinstall_extensions = false");
        }
        conn.execute_batch(&format!("INSTALL {e}; LOAD {e};"))
            .with_context(|| format!("installing DuckDB extension {e}"))?;
    }
    Ok(())
}

fn create_secrets(conn: &Connection, st: &StorageConfig) -> Result<()> {
    for (i, s) in st.secrets.iter().enumerate() {
        let name = s.name.clone().unwrap_or_else(|| format!("storage_secret_{i}"));
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
        conn.execute_batch(&format!("CREATE OR REPLACE SECRET {name} ({})", opts.join(", ")))
            .map_err(|e| anyhow::anyhow!(redact(&e.to_string())))
            .with_context(|| format!("creating secret {name}"))?;
    }
    Ok(())
}

/// Whether a failed commit lost against a concurrent one and is worth trying
/// again: a DuckLake transaction conflict, too many retries on the next
/// snapshot id (the catalog's primary key) under load, or a SQLite catalog
/// that another process has locked.
pub fn is_conflict(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}").to_ascii_lowercase();
    text.contains("conflict") || text.contains("maximum retry count") || text.contains("database is locked")
}

/// Create the parent directory of a local file path.
fn ensure_parent_dir(file: &str) -> Result<()> {
    match Path::new(file).parent() {
        Some(p) if !p.as_os_str().is_empty() => ensure_local_dir(&p.to_string_lossy()),
        _ => Ok(()),
    }
}

fn ensure_local_dir(path: &str) -> Result<()> {
    if path.contains("://") || path.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(path).with_context(|| format!("creating directory {path}"))
}

fn attach(conn: &Connection, st: &StorageConfig) -> Result<()> {
    match st.catalog_kind() {
        CatalogKind::DuckDb => ensure_parent_dir(&st.catalog)?,
        CatalogKind::Sqlite => ensure_parent_dir(st.catalog.trim_start_matches("sqlite:"))?,
        _ => {}
    }
    ensure_local_dir(&st.data_path)?;
    let sql = format!(
        "ATTACH IF NOT EXISTS {} AS {CATALOG} (DATA_PATH {})",
        sql_str(&format!("ducklake:{}", st.catalog)),
        sql_str(&st.data_path),
    );
    conn.execute_batch(&sql).map_err(|e| anyhow::anyhow!(redact(&e.to_string()))).with_context(|| {
        let hint = match st.catalog_kind() {
            CatalogKind::DuckDb | CatalogKind::Sqlite => {
                "; note: a DuckDB-file or SQLite catalog can be opened by only one process at a time"
            }
            _ => "",
        };
        format!("attaching DuckLake catalog ({}) with data path {}{hint}", st.catalog_kind().as_str(), st.data_path)
    })?;
    let opts = [
        ("parquet_compression", sql_str("zstd")),
        ("parquet_row_group_size", st.row_group_size.to_string()),
        ("target_file_size", format!("'{}B'", st.target_file_size.0)),
        ("data_inlining_row_limit", st.data_inlining_row_limit.to_string()),
    ];
    for (k, v) in opts {
        if let Err(e) = conn.execute_batch(&format!("CALL {CATALOG}.set_option({}, {v})", sql_str(k))) {
            warn!(option = k, error = %e, "could not set DuckLake option");
        }
    }
    info!(catalog = st.catalog_kind().as_str(), data_path = %st.data_path, "attached DuckLake");
    Ok(())
}

/// What an object-store client (e.g. the `object_store` crate) needs to
/// reach a location, taken from the `[[storage.secrets]]` entry that
/// covers it. Unset fields fall back to the client's own defaults
/// (environment, instance identity).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ObjectStoreOptions {
    /// Endpoint URL with scheme, e.g. `http://minio:9000`.
    pub endpoint: Option<String>,
    pub region: Option<String>,
    /// Path-style URLs (`url_style = "path"`).
    pub path_style: bool,
    pub key_id: Option<String>,
    pub secret: Option<String>,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for ObjectStoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectStoreOptions")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("path_style", &self.path_style)
            .field("key_id", &self.key_id.as_ref().map(|_| "***"))
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

/// [`ObjectStoreOptions`] for `url` (`s3://`, `gs://`, `r2://`): the secret
/// of the matching type with the longest `scope` that is a prefix of `url`
/// (a secret without scope matches everything), as DuckDB chooses it.
pub fn object_store_options(st: &StorageConfig, url: &str) -> ObjectStoreOptions {
    let scheme = url.split_once("://").map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default();
    let ty = match scheme.as_str() {
        "s3" | "s3a" | "s3n" => "s3",
        "gs" | "gcs" => "gcs",
        "r2" => "r2",
        _ => return ObjectStoreOptions::default(),
    };
    let scope_len = |s: &SecretConfig| -> Option<usize> {
        let scopes: Vec<&str> = match s.options.get("scope") {
            None => return Some(0),
            Some(toml::Value::String(one)) => vec![one],
            Some(toml::Value::Array(a)) => a.iter().filter_map(toml::Value::as_str).collect(),
            Some(_) => return None,
        };
        scopes.into_iter().filter(|p| url.starts_with(p.trim_end_matches('/'))).map(str::len).max()
    };
    let best = st
        .secrets
        .iter()
        .filter(|s| s.get("type").is_some_and(|t| t.eq_ignore_ascii_case(ty)))
        .filter_map(|s| scope_len(s).map(|n| (n, s)))
        .max_by_key(|(n, _)| *n)
        .map(|(_, s)| s);
    let Some(s) = best else { return ObjectStoreOptions::default() };
    let text = |k: &str| s.get(k).filter(|v| !v.is_empty()).map(str::to_string);
    let use_ssl = match s.options.get("use_ssl") {
        Some(toml::Value::Boolean(b)) => *b,
        Some(toml::Value::String(v)) => !v.eq_ignore_ascii_case("false"),
        _ => true,
    };
    ObjectStoreOptions {
        endpoint: text("endpoint")
            .map(|e| if e.contains("://") { e } else { format!("{}://{e}", if use_ssl { "https" } else { "http" }) }),
        region: text("region"),
        path_style: s.get("url_style").is_some_and(|v| v.eq_ignore_ascii_case("path")),
        key_id: text("key_id"),
        secret: text("secret"),
        session_token: text("session_token"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage(toml_text: &str) -> StorageConfig {
        toml::from_str(toml_text).unwrap()
    }

    #[test]
    fn extensions_for_config() {
        let mut st = StorageConfig::default();
        assert_eq!(required_extensions(&st), vec!["ducklake"]);
        st.catalog = "postgresql:host=x".into();
        st.data_path = "s3://b/".into();
        assert_eq!(required_extensions(&st), vec!["ducklake", "postgres", "httpfs"]);
        st.data_path = "abfss://c@acct.dfs.core.windows.net/x/".into();
        assert!(required_extensions(&st).contains(&"azure".to_string()));
        let st = storage(
            "data_path = \"s3://b/\"\n[[secrets]]\ntype = \"s3\"\nprovider = \"credential_chain\"\n\
             [[secrets]]\ntype = \"azure\"\nprovider = \"credential_chain\"",
        );
        assert_eq!(required_extensions(&st), vec!["ducklake", "httpfs", "aws", "azure"]);
    }

    #[test]
    fn redacts_credentials() {
        let e = r#"Unable to connect to Postgres at "host=pg user=odl password=SuperSecret123x dbname=d": refused"#;
        let r = redact(e);
        assert!(!r.contains("SuperSecret"), "{r}");
        assert!(r.contains("host=pg") && r.contains("dbname=d"));
        assert_eq!(redact("password='a b' x"), "password=*** x");
        assert_eq!(redact("postgres:host=db PASSWORD=x"), "postgres:host=db PASSWORD=***");
        assert_eq!(redact("KEY_ID=abc SECRET=\"d e\""), "KEY_ID=*** SECRET=***");
        assert_eq!(redact("no secrets here"), "no secrets here");
    }

    #[test]
    fn quoting_and_identifiers() {
        assert_eq!(sql_str("a'b"), "'a''b'");
        for ok in ["otel", "Sentry_2", "_x"] {
            assert!(is_identifier(ok), "{ok}");
        }
        for bad in ["", "2x", "a-b", "a;drop", "a b", "é"] {
            assert!(!is_identifier(bad), "{bad}");
        }
    }

    #[test]
    fn object_store_options_from_secrets() {
        let st = storage(
            r#"
            [[secrets]]
            type = "s3"
            key_id = "k"
            secret = "s"
            endpoint = "minio:9000"
            use_ssl = false
            url_style = "path"
            region = "eu-1"
            [[secrets]]
            type = "s3"
            scope = "s3://other"
            key_id = "k2"
            secret = "s2"
            "#,
        );
        let o = object_store_options(&st, "s3://mallard/artifacts");
        assert_eq!(o.endpoint.as_deref(), Some("http://minio:9000"));
        assert_eq!((o.key_id.as_deref(), o.region.as_deref(), o.path_style), (Some("k"), Some("eu-1"), true));
        assert!(!format!("{o:?}").contains("\"s\""));
        let o = object_store_options(&st, "s3://other/artifacts");
        assert_eq!(o.key_id.as_deref(), Some("k2"), "the longest matching scope wins");
        assert_eq!(o.endpoint, None);
        assert_eq!(object_store_options(&st, "gs://b/x"), ObjectStoreOptions::default());
        assert_eq!(object_store_options(&st, "/local/dir"), ObjectStoreOptions::default());
    }

    #[test]
    fn opens_a_local_lake() {
        let dir = tempfile::tempdir().unwrap();
        let st = StorageConfig {
            catalog: dir.path().join("a/catalog.ducklake").display().to_string(),
            data_path: format!("{}/lake/", dir.path().display()),
            ..Default::default()
        };
        let conn = open_connection(&st, &DuckDbConfig::default()).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM duckdb_databases() WHERE database_name = ?", [CATALOG], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
