//! Server configuration.
//!
//! Loaded from a TOML file (optional) and then overridden by environment
//! variables with the `ODL_` prefix, using `__` as section separator, e.g.
//! `ODL_STORAGE__DATA_PATH=s3://bucket/otel/`.
//! Values of the form `${NAME}` are replaced by the environment variable NAME,
//! so secrets can stay out of the file.

use std::{collections::BTreeMap, fmt, path::Path, str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::store::schema::Signal;

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
    pub log_format: LogFormat,
}

/// Log output format of the binary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// DuckLake catalog connection. Examples:
    /// - `data/catalog.ducklake` (DuckDB file, single process)
    /// - `sqlite:data/catalog.sqlite`
    /// - `postgres:host=pg dbname=ducklake user=odl` (password via `PGPASSWORD`)
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
    pub target_file_size: ByteSize,
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
    #[serde(with = "duration")]
    pub flush_interval: Duration,
    /// Flush early when this many rows are buffered.
    pub flush_rows: u64,
    /// Reject new requests (back-pressure) when the buffer holds more than this, e.g. `512MB`.
    pub max_buffer_bytes: ByteSize,
    /// Max size of one OTLP request body (after decompression), e.g. `32MB`.
    pub max_request_bytes: ByteSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    /// Keep spans this long (`0` = forever), e.g. `7d`.
    #[serde(with = "duration")]
    pub traces: Duration,
    #[serde(with = "duration")]
    pub logs: Duration,
    #[serde(with = "duration")]
    pub metrics: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaintenanceConfig {
    /// Run maintenance tasks in this process. Disable on extra replicas if you
    /// prefer a single maintainer (concurrent runs are safe but wasteful).
    pub enabled: bool,
    /// Flush inlined rows and merge small files this often.
    #[serde(with = "duration")]
    pub compact_interval: Duration,
    /// Files below this size are merged by the frequent compaction.
    pub small_file_size: ByteSize,
    /// Merge everything below `target_file_size` this often.
    #[serde(with = "duration")]
    pub full_compact_interval: Duration,
    /// Retention deletes, snapshot expiry and file cleanup this often.
    #[serde(with = "duration")]
    pub retention_interval: Duration,
    /// Keep old snapshots (time travel) this long.
    #[serde(with = "duration")]
    pub snapshot_retention: Duration,
    /// Delete files no longer referenced after this grace period.
    #[serde(with = "duration")]
    pub file_cleanup_delay: Duration,
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
    #[serde(with = "duration")]
    pub timeout: Duration,
    /// Enable `POST /api/v1/sql` (read-only ad-hoc SQL). Requires `auth.api_tokens`.
    pub enable_sql: bool,
    /// Max time span a single query may cover, e.g. `31d`.
    #[serde(with = "duration")]
    pub max_window: Duration,
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
    /// DuckDB memory limit, e.g. `2GB`. Empty or unset = DuckDB default (80% of RAM).
    #[serde(with = "optional_bytes", skip_serializing_if = "Option::is_none")]
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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            otlp_grpc_addr: "0.0.0.0:4317".into(),
            otlp_http_addr: "0.0.0.0:4318".into(),
            api_addr: "0.0.0.0:8080".into(),
            log_format: LogFormat::Text,
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
            target_file_size: ByteSize::mb(128),
            row_group_size: 122_880,
        }
    }
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(5),
            flush_rows: 200_000,
            max_buffer_bytes: ByteSize::mb(512),
            max_request_bytes: ByteSize::mb(32),
        }
    }
}

const fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

impl RetentionConfig {
    /// Retention of one signal; zero keeps data forever.
    pub(crate) fn of(&self, s: Signal) -> Duration {
        match s {
            Signal::Traces => self.traces,
            Signal::Logs => self.logs,
            Signal::Metrics => self.metrics,
        }
    }
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self { traces: days(7), logs: days(7), metrics: days(30) }
    }
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            compact_interval: Duration::from_secs(10 * 60),
            small_file_size: ByteSize::mb(16),
            full_compact_interval: Duration::from_secs(6 * 3600),
            retention_interval: Duration::from_secs(3600),
            snapshot_retention: Duration::from_secs(3600),
            file_cleanup_delay: Duration::from_secs(15 * 60),
            catalog_wide: true,
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self { max_concurrent: 4, timeout: Duration::from_secs(30), enable_sql: false, max_window: days(31) }
    }
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
            let text =
                std::fs::read_to_string(path).with_context(|| format!("reading config file {}", path.display()))?;
            let file: toml::Value = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            merge(&mut root, file);
        }
        apply_env(&mut root, env_prefix, std::env::vars())?;
        interpolate(&mut root, &|name| std::env::var(name).ok())?;
        let cfg: Config = root.try_into().context("invalid configuration")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Check ranges and combinations. Units are checked while parsing.
    pub fn validate(&self) -> Result<()> {
        let (i, m, q, r) = (&self.ingest, &self.maintenance, &self.query, &self.retention);
        for (name, d) in [
            ("ingest.flush_interval", i.flush_interval),
            ("maintenance.compact_interval", m.compact_interval),
            ("maintenance.full_compact_interval", m.full_compact_interval),
            ("maintenance.retention_interval", m.retention_interval),
            ("maintenance.snapshot_retention", m.snapshot_retention),
            ("maintenance.file_cleanup_delay", m.file_cleanup_delay),
            ("query.timeout", q.timeout),
            ("query.max_window", q.max_window),
            ("retention.traces", r.traces),
            ("retention.logs", r.logs),
            ("retention.metrics", r.metrics),
        ] {
            if d > MAX_DURATION {
                bail!("{name} is longer than 100 years");
            }
        }
        for (name, d) in [
            ("ingest.flush_interval", i.flush_interval),
            ("maintenance.compact_interval", m.compact_interval),
            ("maintenance.full_compact_interval", m.full_compact_interval),
            ("maintenance.retention_interval", m.retention_interval),
            ("query.timeout", q.timeout),
            ("query.max_window", q.max_window),
        ] {
            if d.is_zero() {
                bail!("{name} must be greater than 0");
            }
        }
        if i.flush_rows == 0 {
            bail!("ingest.flush_rows must be greater than 0");
        }
        // An empty token is almost always a missing secret; refuse it instead
        // of silently running without authentication.
        for (name, list) in
            [("auth.api_tokens", &self.auth.api_tokens), ("auth.ingest_tokens", &self.auth.ingest_tokens)]
        {
            if list.iter().any(|t| t.trim().is_empty()) {
                bail!("{name} contains an empty token (is the secret/env var set?); remove it or set a value");
            }
        }
        if q.enable_sql && self.auth.api_tokens.iter().all(|t| t.trim().is_empty()) {
            bail!("query.enable_sql requires auth.api_tokens to be set");
        }
        if !is_identifier(&self.storage.schema) {
            bail!("storage.schema must be a plain identifier (letters, digits, _)");
        }
        if q.max_concurrent == 0 {
            bail!("query.max_concurrent must be at least 1");
        }
        Ok(())
    }

    /// Example config with comments, printed by `otel-ducklake config`.
    pub fn example_toml() -> &'static str {
        include_str!("../../../config/otel-ducklake.example.toml")
    }
}

/// Upper bound for every duration setting; keeps date arithmetic in range.
const MAX_DURATION: Duration = Duration::from_secs(100 * 366 * 86_400);

pub fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

/// Parse `5s`, `10m`, `7d`, `1h30m`. `0` means zero.
pub(crate) fn parse_duration(s: &str) -> Result<Duration, humantime::DurationError> {
    let s = s.trim();
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    humantime::parse_duration(s)
}

/// A size in bytes. Parsed from `512MB`, `2GiB` or `1024` (bytes): decimal
/// units are powers of 1000, binary units (`KiB`, `MiB`, `GiB`) powers of 1024.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl ByteSize {
    pub const fn mb(n: u64) -> Self {
        Self(n * 1_000_000)
    }
}

/// Error parsing a [`ByteSize`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid size {0:?}: expected a number with an optional unit, e.g. 512MB or 2GiB")]
pub struct ByteSizeError(String);

impl FromStr for ByteSize {
    type Err = ByteSizeError;

    fn from_str(s: &str) -> Result<Self, ByteSizeError> {
        let s = s.trim();
        let err = || ByteSizeError(s.to_string());
        let idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
        let (num, unit) = s.split_at(idx);
        let num: f64 = num.parse().map_err(|_| err())?;
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
            _ => return Err(err()),
        };
        Ok(Self((num * mult) as u64))
    }
}

impl fmt::Display for ByteSize {
    /// The largest decimal unit that divides the value exactly.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        for (unit, m) in [("TB", 1_000_000_000_000), ("GB", 1_000_000_000), ("MB", 1_000_000), ("KB", 1_000)] {
            if n != 0 && n.is_multiple_of(m) {
                return write!(f, "{}{unit}", n / m);
            }
        }
        write!(f, "{n}")
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    /// A string with a unit, or a plain number of bytes.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = ByteSize;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a size such as \"512MB\" or a number of bytes")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<ByteSize, E> {
                v.parse().map_err(E::custom)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<ByteSize, E> {
                Ok(ByteSize(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<ByteSize, E> {
                u64::try_from(v).map(ByteSize).map_err(|_| E::custom("a size cannot be negative"))
            }
        }
        d.deserialize_any(V)
    }
}

/// `Option<ByteSize>` where an empty string means `None`.
mod optional_bytes {
    use super::*;

    pub fn serialize<S: Serializer>(v: &Option<ByteSize>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => b.serialize(s),
            None => s.serialize_str(""),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ByteSize>, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Option<ByteSize>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a size such as \"2GB\", a number of bytes, or \"\"")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                if v.trim().is_empty() { Ok(None) } else { v.parse().map(Some).map_err(E::custom) }
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Some(ByteSize(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                u64::try_from(v).map(|v| Some(ByteSize(v))).map_err(|_| E::custom("a size cannot be negative"))
            }
        }
        d.deserialize_any(V)
    }
}

/// A [`Duration`] written as `5s`, `10m`, `7d`, `1h30m`; `"0"` (or `0`) means zero.
mod duration {
    use super::*;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        if d.is_zero() { s.serialize_str("0") } else { s.collect_str(&humantime::format_duration(*d)) }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Duration;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a duration such as \"30s\", \"10m\" or \"7d\"")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Duration, E> {
                parse_duration(v).map_err(|e| E::custom(format!("invalid duration {v:?}: {e}")))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Duration, E> {
                if v == 0 { Ok(Duration::ZERO) } else { Err(E::custom("a duration needs a unit, e.g. \"30s\"")) }
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Duration, E> {
                self.visit_u64(u64::try_from(v).map_err(|_| E::custom("a duration cannot be negative"))?)
            }
        }
        d.deserialize_any(V)
    }
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
fn apply_env(root: &mut toml::Value, prefix: &str, vars: impl Iterator<Item = (String, String)>) -> Result<()> {
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
        let existing_is_string =
            path.iter().try_fold(&*root, |cur, seg| cur.get(seg.as_str())).is_some_and(|v| v.is_str());
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
            let table = cur.as_table_mut().with_context(|| format!("env override {k}: {seg} is not a section"))?;
            if i == path.len() - 1 {
                table.insert(seg.clone(), value.clone());
                break;
            }
            cur = table.entry(seg.clone()).or_insert_with(|| toml::Value::Table(Default::default()));
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
                let val = lookup(name).with_context(|| format!("config references ${{{name}}} but it is not set"))?;
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
        assert_eq!("512MB".parse(), Ok(ByteSize(512_000_000)));
        assert_eq!("1GiB".parse(), Ok(ByteSize(1 << 30)));
        assert_eq!("100".parse(), Ok(ByteSize(100)));
        assert!("5XB".parse::<ByteSize>().is_err());
        assert_eq!(ByteSize::mb(128).to_string(), "128MB");
        assert_eq!(ByteSize(1 << 30).to_string(), (1u64 << 30).to_string());
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("7d").unwrap(), days(7));
    }

    fn from_toml(text: &str) -> Result<Config, toml::de::Error> {
        let mut root = toml::Value::try_from(Config::default()).unwrap();
        merge(&mut root, toml::from_str(text).unwrap());
        root.try_into()
    }

    #[test]
    fn typed_values_parse_at_load_time() {
        let cfg = from_toml(
            r#"
            server.log_format = "json"
            ingest = { flush_interval = "250ms", max_buffer_bytes = "1GiB", max_request_bytes = 1024 }
            retention = { traces = "0", logs = "3d" }
            duckdb.memory_limit = "2GB"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.log_format, LogFormat::Json);
        assert_eq!(cfg.ingest.flush_interval, Duration::from_millis(250));
        assert_eq!(cfg.ingest.max_buffer_bytes, ByteSize(1 << 30));
        assert_eq!(cfg.ingest.max_request_bytes, ByteSize(1024));
        assert_eq!(cfg.retention.traces, Duration::ZERO);
        assert_eq!(cfg.retention.logs, days(3));
        assert_eq!(cfg.duckdb.memory_limit, Some(ByteSize(2_000_000_000)));
        assert_eq!(from_toml("duckdb.memory_limit = \"\"").unwrap().duckdb.memory_limit, None);
        // Defaults survive the round trip through TOML.
        assert_eq!(from_toml("").unwrap().retention.metrics, days(30));
        for bad in [
            "server.log_format = \"xml\"",
            "ingest.flush_interval = \"5 parsecs\"",
            "ingest.flush_interval = 5",
            "ingest.max_buffer_bytes = \"5XB\"",
        ] {
            assert!(from_toml(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn env_overrides_and_interpolation() {
        let mut root = toml::Value::try_from(Config::default()).unwrap();
        apply_env(
            &mut root,
            "ODL_",
            vec![
                ("ODL_STORAGE__DATA_PATH".to_string(), "s3://b/p/".to_string()),
                ("ODL_QUERY__MAX_CONCURRENT".to_string(), "8".to_string()),
                ("ODL_AUTH__API_TOKENS".to_string(), "[\"${TOK}\"]".to_string()),
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
        assert_eq!(cfg.retention.traces, Duration::ZERO);
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
        cfg.ingest.flush_interval = Duration::ZERO;
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.retention.traces = humantime::parse_duration("300000y").unwrap();
        assert!(cfg.validate().is_err(), "durations are bounded");
        let mut cfg = Config::default();
        cfg.query.max_window = Duration::ZERO;
        assert!(cfg.validate().is_err(), "a zero max_window makes every query fail");
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
