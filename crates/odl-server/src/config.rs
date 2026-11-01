//! Server configuration.
//!
//! Loaded from a TOML file (optional) and then overridden by environment
//! variables with the `ODL_` prefix, using `__` as section separator, e.g.
//! `ODL_STORAGE__DATA_PATH=s3://bucket/otel/`.
//! Values of the form `${NAME}` are replaced by the environment variable NAME,
//! so secrets can stay out of the file.

use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub ingest: IngestConfig,
    pub retention: RetentionConfig,
    pub maintenance: MaintenanceConfig,
    pub query: QueryConfig,
    pub auth: AuthConfig,
    pub duckdb: DuckDbConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// OTLP/gRPC listen address.
    pub otlp_grpc_addr: String,
    /// OTLP/HTTP listen address.
    pub otlp_http_addr: String,
    /// Query API (OpenAPI) listen address.
    pub api_addr: String,
    /// Log format: `text` or `json`.
    pub log_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// DuckLake catalog connection. Examples:
    /// - `data/catalog.ducklake` (DuckDB file, single process)
    /// - `sqlite:data/catalog.sqlite`
    /// - `postgres:host=pg dbname=ducklake user=odl password=${PGPASSWORD}`
    pub catalog: String,
    /// Where Parquet files go: local dir, `s3://`, `gs://`, `az://`/`abfss://`, `r2://`, ...
    pub data_path: String,
    /// DuckDB schema inside the lake that holds the tables.
    pub schema: String,
    /// DuckDB secrets created before attaching (S3, GCS, Azure, R2 credentials).
    /// Each entry is passed to `CREATE SECRET` as key/value options.
    pub secrets: Vec<SecretConfig>,
    /// Extra DuckDB extensions to load (e.g. `azure`). `ducklake`, `httpfs`,
    /// `postgres_scanner`/`sqlite_scanner` are loaded automatically when needed.
    pub extensions: Vec<String>,
    /// Rows per insert below which DuckLake stores rows inside the catalog
    /// database instead of writing a tiny Parquet file. 0 disables inlining.
    pub data_inlining_row_limit: u64,
    /// Target Parquet file size for compaction, e.g. `128MB`.
    pub target_file_size: String,
    /// Parquet row group size in rows.
    pub row_group_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretConfig {
    /// Secret name (optional).
    #[serde(default)]
    pub name: Option<String>,
    /// Remaining keys are passed verbatim: `type`, `provider`, `key_id`, `secret`,
    /// `region`, `endpoint`, `url_style`, `use_ssl`, `scope`, `connection_string`, ...
    #[serde(flatten)]
    pub options: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// Flush buffered rows to DuckLake at least this often, e.g. `5s`.
    pub flush_interval: String,
    /// Flush early when this many rows are buffered.
    pub flush_rows: u64,
    /// Reject new requests (back-pressure) when the buffer holds more than this, e.g. `512MB`.
    pub max_buffer_bytes: String,
    /// Max size of one OTLP request body (after decompression), e.g. `32MB`.
    pub max_request_bytes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    /// Keep spans this long (`0` = forever), e.g. `7d`.
    pub traces: String,
    pub logs: String,
    pub metrics: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaintenanceConfig {
    /// Run maintenance tasks in this process. Disable on extra replicas if you
    /// prefer a single maintainer (concurrent runs are safe but wasteful).
    pub enabled: bool,
    /// Flush inlined rows and merge small files this often.
    pub compact_interval: String,
    /// Files below this size are merged by the frequent compaction.
    pub small_file_size: String,
    /// Merge everything below `target_file_size` this often.
    pub full_compact_interval: String,
    /// Retention deletes, snapshot expiry and file cleanup this often.
    pub retention_interval: String,
    /// Keep old snapshots (time travel) this long.
    pub snapshot_retention: String,
    /// Delete files no longer referenced after this grace period.
    pub file_cleanup_delay: String,
    /// Run catalog-wide tasks (snapshot expiry, old file cleanup). These cannot
    /// be scoped to one schema; when several applications share a catalog, let
    /// exactly one of them run these.
    pub catalog_wide: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryConfig {
    /// Number of concurrent query connections.
    pub max_concurrent: usize,
    /// Abort queries running longer than this.
    pub timeout: String,
    /// Enable `POST /api/v1/sql` (read-only ad-hoc SQL). Requires `auth.api_tokens`.
    pub enable_sql: bool,
    /// Max time span a single query may cover, e.g. `31d`.
    pub max_window: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Bearer tokens accepted by the query API. Empty = no auth.
    pub api_tokens: Vec<String>,
    /// Bearer tokens accepted by OTLP receivers. Empty = no auth.
    pub ingest_tokens: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DuckDbConfig {
    /// DuckDB memory limit, e.g. `2GB`. Empty = DuckDB default (80% of RAM).
    pub memory_limit: String,
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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            otlp_grpc_addr: "0.0.0.0:4317".into(),
            otlp_http_addr: "0.0.0.0:4318".into(),
            api_addr: "0.0.0.0:8080".into(),
            log_format: "text".into(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            catalog: "data/catalog.ducklake".into(),
            data_path: "data/lake/".into(),
            schema: "otel".into(),
            secrets: vec![],
            extensions: vec![],
            data_inlining_row_limit: 1000,
            target_file_size: "128MB".into(),
            row_group_size: 122_880,
        }
    }
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            flush_interval: "5s".into(),
            flush_rows: 200_000,
            max_buffer_bytes: "512MB".into(),
            max_request_bytes: "32MB".into(),
        }
    }
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            traces: "7d".into(),
            logs: "7d".into(),
            metrics: "30d".into(),
        }
    }
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            compact_interval: "10m".into(),
            small_file_size: "16MB".into(),
            full_compact_interval: "6h".into(),
            retention_interval: "1h".into(),
            snapshot_retention: "1h".into(),
            file_cleanup_delay: "15m".into(),
            catalog_wide: true,
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            timeout: "30s".into(),
            enable_sql: false,
            max_window: "31d".into(),
        }
    }
}

impl Default for DuckDbConfig {
    fn default() -> Self {
        Self {
            memory_limit: String::new(),
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
    pub fn as_str(self) -> &'static str {
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

impl Config {
    /// Load config: defaults <- TOML file <- `ODL_*` environment variables.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        Self::load_with_prefix(path, "ODL_")
    }

    /// Like [`Config::load`] with a custom environment prefix, e.g. `OBS_OTEL_`.
    pub fn load_with_prefix(path: Option<&Path>, env_prefix: &str) -> Result<Self> {
        let mut root = toml::Value::try_from(Config::default())?;
        if let Some(path) = path {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading config file {}", path.display()))?;
            let file: toml::Value =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            merge(&mut root, file);
        }
        apply_env(&mut root, env_prefix, std::env::vars())?;
        interpolate(&mut root, &|name| std::env::var(name).ok())?;
        let cfg: Config = root.try_into().context("invalid configuration")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        for (name, v) in [
            ("ingest.flush_interval", &self.ingest.flush_interval),
            (
                "maintenance.compact_interval",
                &self.maintenance.compact_interval,
            ),
            (
                "maintenance.full_compact_interval",
                &self.maintenance.full_compact_interval,
            ),
            (
                "maintenance.retention_interval",
                &self.maintenance.retention_interval,
            ),
            (
                "maintenance.snapshot_retention",
                &self.maintenance.snapshot_retention,
            ),
            (
                "maintenance.file_cleanup_delay",
                &self.maintenance.file_cleanup_delay,
            ),
            ("query.timeout", &self.query.timeout),
            ("query.max_window", &self.query.max_window),
            ("retention.traces", &self.retention.traces),
            ("retention.logs", &self.retention.logs),
            ("retention.metrics", &self.retention.metrics),
        ] {
            parse_duration(v).with_context(|| format!("{name} = {v:?}"))?;
        }
        for (name, v) in [
            ("ingest.max_buffer_bytes", &self.ingest.max_buffer_bytes),
            ("ingest.max_request_bytes", &self.ingest.max_request_bytes),
            ("storage.target_file_size", &self.storage.target_file_size),
            (
                "maintenance.small_file_size",
                &self.maintenance.small_file_size,
            ),
        ] {
            parse_bytes(v).with_context(|| format!("{name} = {v:?}"))?;
        }
        // Empty strings are ignored as tokens, so they do not count.
        for (name, v) in [
            ("ingest.flush_interval", &self.ingest.flush_interval),
            (
                "maintenance.compact_interval",
                &self.maintenance.compact_interval,
            ),
            (
                "maintenance.full_compact_interval",
                &self.maintenance.full_compact_interval,
            ),
            (
                "maintenance.retention_interval",
                &self.maintenance.retention_interval,
            ),
            ("query.timeout", &self.query.timeout),
        ] {
            if parse_duration(v)?.is_zero() {
                bail!("{name} must be greater than 0");
            }
        }
        if self.ingest.flush_rows == 0 {
            bail!("ingest.flush_rows must be greater than 0");
        }
        // An empty token is almost always a missing secret; refuse it instead
        // of silently running without authentication.
        for (name, list) in [
            ("auth.api_tokens", &self.auth.api_tokens),
            ("auth.ingest_tokens", &self.auth.ingest_tokens),
        ] {
            if list.iter().any(|t| t.trim().is_empty()) {
                bail!(
                    "{name} contains an empty token (is the secret/env var set?); remove it or set a value"
                );
            }
        }
        if self.query.enable_sql && self.auth.api_tokens.iter().all(|t| t.trim().is_empty()) {
            bail!("query.enable_sql requires auth.api_tokens to be set");
        }
        if !is_identifier(&self.storage.schema) {
            bail!("storage.schema must be a plain identifier (letters, digits, _)");
        }
        if self.query.max_concurrent == 0 {
            bail!("query.max_concurrent must be at least 1");
        }
        if !self.duckdb.memory_limit.is_empty() {
            parse_bytes(&self.duckdb.memory_limit).context("duckdb.memory_limit")?;
        }
        Ok(())
    }

    pub fn flush_interval(&self) -> Duration {
        parse_duration(&self.ingest.flush_interval).expect("validated")
    }
    pub fn max_buffer_bytes(&self) -> u64 {
        parse_bytes(&self.ingest.max_buffer_bytes).expect("validated")
    }
    pub fn max_request_bytes(&self) -> usize {
        parse_bytes(&self.ingest.max_request_bytes).expect("validated") as usize
    }
    pub fn query_timeout(&self) -> Duration {
        parse_duration(&self.query.timeout).expect("validated")
    }
    pub fn max_window(&self) -> Duration {
        parse_duration(&self.query.max_window).expect("validated")
    }
    /// Example config with comments, printed by `otel-ducklake config`.
    pub fn example_toml() -> &'static str {
        include_str!("../../../config/otel-ducklake.example.toml")
    }
}

pub fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

/// Parse `5s`, `10m`, `7d`, `1h30m`. `0` means zero.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    Ok(humantime::parse_duration(s)?)
}

/// Parse `512MB`, `2GiB`, `1024` (bytes). Decimal units are powers of 1000,
/// binary units (`KiB`, `MiB`, `GiB`) powers of 1024.
pub fn parse_bytes(s: &str) -> Result<u64> {
    let s = s.trim();
    let idx = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(idx);
    let num: f64 = num.parse().with_context(|| format!("invalid size {s:?}"))?;
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024f64.powi(4),
        other => bail!("unknown size unit {other:?} in {s:?}"),
    };
    Ok((num * mult) as u64)
}

fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) if bv.is_table() && v.is_table() => merge(bv, v),
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Apply `ODL_SECTION__KEY=value` overrides. The value is parsed as TOML when
/// possible (numbers, booleans, arrays) and as a string otherwise.
fn apply_env(
    root: &mut toml::Value,
    prefix: &str,
    vars: impl Iterator<Item = (String, String)>,
) -> Result<()> {
    for (k, v) in vars {
        let Some(rest) = k.strip_prefix(prefix) else {
            continue;
        };
        let path: Vec<String> = rest.split("__").map(|p| p.to_ascii_lowercase()).collect();
        if path.len() < 2 {
            continue;
        }
        // Keep the raw text for string settings (e.g. retention "0"); parse
        // numbers, booleans and arrays as TOML otherwise.
        let existing_is_string = path
            .iter()
            .try_fold(&*root, |cur, seg| cur.get(seg.as_str()))
            .is_some_and(|v| v.is_str());
        let value = if existing_is_string {
            toml::Value::String(v.clone())
        } else {
            match toml::from_str::<toml::Table>(&format!("v = {v}")) {
                Ok(mut t) => t.remove("v").unwrap(),
                Err(_) => toml::Value::String(v.clone()),
            }
        };
        let mut cur = &mut *root;
        for (i, seg) in path.iter().enumerate() {
            let table = cur
                .as_table_mut()
                .with_context(|| format!("env override {k}: {seg} is not a section"))?;
            if i == path.len() - 1 {
                table.insert(seg.clone(), value.clone());
                break;
            }
            cur = table
                .entry(seg.clone())
                .or_insert_with(|| toml::Value::Table(Default::default()));
        }
    }
    Ok(())
}

/// Replace `${NAME}` in all string values.
fn interpolate(v: &mut toml::Value, lookup: &dyn Fn(&str) -> Option<String>) -> Result<()> {
    match v {
        toml::Value::String(s) => {
            let mut out = String::with_capacity(s.len());
            let mut rest = s.as_str();
            while let Some(start) = rest.find("${") {
                out.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                let end = after.find('}').context("unterminated ${ in config value")?;
                let name = &after[..end];
                let val = lookup(name)
                    .with_context(|| format!("config references ${{{name}}} but it is not set"))?;
                out.push_str(&val);
                rest = &after[end + 1..];
            }
            out.push_str(rest);
            *s = out;
        }
        toml::Value::Array(a) => a.iter_mut().try_for_each(|x| interpolate(x, lookup))?,
        toml::Value::Table(t) => t.iter_mut().try_for_each(|(_, x)| interpolate(x, lookup))?,
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations() {
        assert_eq!(parse_bytes("512MB").unwrap(), 512_000_000);
        assert_eq!(parse_bytes("1GiB").unwrap(), 1 << 30);
        assert_eq!(parse_bytes("100").unwrap(), 100);
        assert!(parse_bytes("5XB").is_err());
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(
            parse_duration("7d").unwrap(),
            Duration::from_secs(7 * 86400)
        );
    }

    #[test]
    fn env_overrides_and_interpolation() {
        let mut root = toml::Value::try_from(Config::default()).unwrap();
        apply_env(
            &mut root,
            "ODL_",
            vec![
                (
                    "ODL_STORAGE__DATA_PATH".to_string(),
                    "s3://b/p/".to_string(),
                ),
                ("ODL_QUERY__MAX_CONCURRENT".to_string(), "8".to_string()),
                (
                    "ODL_AUTH__API_TOKENS".to_string(),
                    "[\"${TOK}\"]".to_string(),
                ),
                ("ODL_RETENTION__TRACES".to_string(), "0".to_string()),
                ("UNRELATED".to_string(), "x".to_string()),
            ]
            .into_iter(),
        )
        .unwrap();
        interpolate(&mut root, &|n| (n == "TOK").then(|| "secret".to_string())).unwrap();
        let cfg: Config = root.try_into().unwrap();
        assert_eq!(cfg.storage.data_path, "s3://b/p/");
        assert_eq!(cfg.query.max_concurrent, 8);
        assert_eq!(cfg.auth.api_tokens, vec!["secret".to_string()]);
        assert_eq!(cfg.retention.traces, "0");
    }

    #[test]
    fn missing_env_is_an_error() {
        let mut v = toml::Value::String("${NOPE_NOT_SET}".into());
        assert!(interpolate(&mut v, &|_| None).is_err());
    }

    #[test]
    fn sql_requires_tokens() {
        let mut cfg = Config::default();
        cfg.query.enable_sql = true;
        assert!(cfg.validate().is_err());
        cfg.auth.api_tokens = vec!["".into()];
        assert!(cfg.validate().is_err(), "empty token must not enable SQL");
        cfg.auth.api_tokens = vec!["t".into()];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn zero_intervals_and_empty_tokens_are_rejected() {
        let mut cfg = Config::default();
        cfg.ingest.flush_interval = "0".into();
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.ingest.flush_rows = 0;
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.auth.ingest_tokens = vec!["ok".into(), " ".into()];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn catalog_kinds() {
        let mut s = StorageConfig::default();
        assert_eq!(s.catalog_kind(), CatalogKind::DuckDb);
        s.catalog = "postgres:host=x".into();
        assert_eq!(s.catalog_kind(), CatalogKind::Postgres);
        s.catalog = "sqlite:x.db".into();
        assert_eq!(s.catalog_kind(), CatalogKind::Sqlite);
    }

    #[test]
    fn example_config_parses() {
        let v: toml::Value = toml::from_str(Config::example_toml()).unwrap();
        let mut root = toml::Value::try_from(Config::default()).unwrap();
        merge(&mut root, v);
        let cfg: Config = root.try_into().unwrap();
        cfg.validate().unwrap();
    }
}
