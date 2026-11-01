//! `[storage]` and `[duckdb]` settings.

use std::collections::BTreeMap;

use mallard_common::ByteSize;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// DuckLake catalog connection. Examples:
    /// - `data/catalog.ducklake` (DuckDB file, single process)
    /// - `sqlite:data/catalog.sqlite`
    /// - `postgres:host=pg dbname=ducklake user=mallard` (password via `PGPASSWORD`)
    pub catalog: String,
    /// Where Parquet files go: local dir, `s3://`, `gs://`, `az://`/`abfss://`, `r2://`, ...
    pub data_path: String,
    /// Where Sentry sourcemap bundles go (mallard only): a local dir,
    /// `s3://`, `gs://` or `az://`. Empty = sourcemap upload is disabled.
    /// Credentials come from `secrets` (the entry whose scope covers it).
    pub artifacts_path: String,
    /// DuckDB secrets created before attaching (S3, GCS, Azure, R2 credentials).
    /// Each entry is passed to `CREATE SECRET` as key/value options.
    pub secrets: Vec<SecretConfig>,
    /// Extra DuckDB extensions to load (e.g. `azure`). `ducklake`, `httpfs`,
    /// `postgres`/`sqlite`/`mysql` are loaded automatically when needed.
    pub extensions: Vec<String>,
    /// Rows per insert below which DuckLake stores rows inside the catalog
    /// database instead of writing a tiny Parquet file. 0 disables inlining.
    pub data_inlining_row_limit: u64,
    /// Target Parquet file size for compaction, e.g. `128MB`.
    pub target_file_size: ByteSize,
    /// Parquet row group size in rows.
    pub row_group_size: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            catalog: "data/catalog.ducklake".into(),
            data_path: "data/lake/".into(),
            artifacts_path: String::new(),
            secrets: vec![],
            extensions: vec![],
            data_inlining_row_limit: 1000,
            target_file_size: ByteSize::mb(128),
            row_group_size: 122_880,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SecretConfig {
    /// Secret name (optional).
    #[serde(default)]
    pub name: Option<String>,
    /// Remaining keys are passed verbatim: `type`, `provider`, `key_id`, `secret`,
    /// `region`, `endpoint`, `url_style`, `use_ssl`, `scope`, `connection_string`, ...
    #[serde(flatten)]
    pub options: BTreeMap<String, toml::Value>,
}

impl SecretConfig {
    /// A string option, e.g. `type`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.options.get(key).and_then(toml::Value::as_str)
    }
}

/// Only the option names: values are credentials.
impl std::fmt::Debug for SecretConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretConfig").field("name", &self.name).field("options", &self.options.keys()).finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DuckDbConfig {
    /// DuckDB memory limit, e.g. `2GB`. Empty or unset = DuckDB default (80% of RAM).
    #[serde(with = "mallard_common::bytes::optional", skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<ByteSize>,
    /// Worker threads. 0 = number of cores.
    pub threads: usize,
    /// Spill directory for large queries. Empty = DuckDB default.
    pub temp_directory: String,
    /// Directory for downloaded extensions. Empty = `~/.duckdb/extensions`.
    pub extension_directory: String,
    /// Allow downloading extensions at start-up. Disable in air-gapped images
    /// that ship extensions pre-installed.
    pub autoinstall_extensions: bool,
}

impl Default for DuckDbConfig {
    fn default() -> Self {
        Self {
            memory_limit: None,
            threads: 0,
            temp_directory: String::new(),
            extension_directory: String::new(),
            autoinstall_extensions: true,
        }
    }
}

/// Catalog backend derived from `storage.catalog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogKind {
    DuckDb,
    Sqlite,
    Postgres,
    MySql,
}

impl CatalogKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            CatalogKind::DuckDb => "duckdb",
            CatalogKind::Sqlite => "sqlite",
            CatalogKind::Postgres => "postgres",
            CatalogKind::MySql => "mysql",
        }
    }
}

impl StorageConfig {
    pub fn catalog_kind(&self) -> CatalogKind {
        let c = self.catalog.trim();
        if c.starts_with("postgres:") || c.starts_with("postgresql:") {
            CatalogKind::Postgres
        } else if c.starts_with("sqlite:") {
            CatalogKind::Sqlite
        } else if c.starts_with("mysql:") {
            CatalogKind::MySql
        } else {
            CatalogKind::DuckDb
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_kinds() {
        let mut s = StorageConfig::default();
        assert_eq!(s.catalog_kind(), CatalogKind::DuckDb);
        for (c, k) in [
            ("postgres:host=x", CatalogKind::Postgres),
            ("postgresql:host=x", CatalogKind::Postgres),
            ("sqlite:x.db", CatalogKind::Sqlite),
            ("mysql:host=x", CatalogKind::MySql),
        ] {
            s.catalog = c.into();
            assert_eq!(s.catalog_kind(), k, "{c}");
        }
    }

    #[test]
    fn secret_debug_hides_values() {
        let s: SecretConfig = toml::from_str("type = \"s3\"\nsecret = \"hunter2\"").unwrap();
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(s.get("type"), Some("s3"));
    }
}
