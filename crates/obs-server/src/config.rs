//! Platform configuration.
//!
//! One TOML file plus `OBS_*` environment variables (`__` separates
//! sections, e.g. `OBS_STORAGE__DATA_PATH=s3://bucket/lake/`,
//! `OBS_OTEL__RETENTION__TRACES=3d`, `OBS_SENTRY__RETENTION_DAYS=30`).
//! `${NAME}` inside the file is replaced by environment variable NAME.
//!
//! Shared settings (`[server]`, `[storage]`, `[duckdb]`) are copied into
//! both library configs; `[otel]` and `[sentry]` hold the rest of each
//! library's own config.

use std::path::Path;

use anyhow::{Context, Result};
use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;

pub const SENTRY_PREFIX: &str = "/sentry";
pub const OTEL_PREFIX: &str = "/otel";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    /// DuckLake catalog + data path + secrets, shared by both libraries.
    /// `schema` is ignored here: otel uses `otel`, sentry uses `sentry`.
    pub storage: odl_server::config::StorageConfig,
    pub duckdb: odl_server::config::DuckDbConfig,
    /// otel-ducklake settings: `[otel.ingest]`, `[otel.retention]`,
    /// `[otel.maintenance]`, `[otel.query]`.
    pub otel: odl_server::config::Config,
    /// sentry-ducklake settings, e.g. `retention_days`, `storage_url`
    /// (sourcemap bundles).
    pub sentry: sdl_server::Config,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Sentry ingest, both query APIs, health and metrics.
    pub http_addr: String,
    pub otlp_http_addr: String,
    pub otlp_grpc_addr: String,
    /// How clients reach `http_addr`; used to build Sentry DSNs.
    pub public_url: String,
    /// Bearer tokens for the query/management APIs of both libraries.
    pub api_tokens: Vec<String>,
    /// Bearer tokens for OTLP ingest; empty = open. (Sentry ingest uses DSN keys.)
    pub ingest_tokens: Vec<String>,
    /// `text` or `json`.
    pub log_format: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            http_addr: "0.0.0.0:8080".into(),
            otlp_http_addr: "0.0.0.0:4318".into(),
            otlp_grpc_addr: "0.0.0.0:4317".into(),
            public_url: "http://localhost:8080".into(),
            api_tokens: vec![],
            ingest_tokens: vec![],
            log_format: "text".into(),
        }
    }
}

impl Config {
    /// Load `path` (optional) and apply `OBS_*` environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let mut fig = Figment::new();
        if let Some(p) = path {
            let text =
                std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            fig = fig.merge(Toml::string(&expand_env(&text)?));
        }
        let mut cfg: Config = fig
            .merge(env_overrides())
            .extract()
            .context("invalid configuration")?;
        cfg.normalize();
        cfg.validate()?;
        Ok(cfg)
    }

    /// Tokens may be given comma separated (handy with `${ENV}` values);
    /// empty entries are dropped.
    pub fn normalize(&mut self) {
        for list in [&mut self.server.api_tokens, &mut self.server.ingest_tokens] {
            *list = list
                .iter()
                .flat_map(|t| t.split(','))
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect();
        }
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.server.public_url.starts_with("http://")
                || self.server.public_url.starts_with("https://"),
            "server.public_url must start with http:// or https://"
        );
        self.otel_config().validate().context("otel")?;
        self.sentry_config().validate().context("sentry")?;
        Ok(())
    }

    /// The otel-ducklake config with the shared settings applied.
    pub fn otel_config(&self) -> odl_server::config::Config {
        let mut c = self.otel.clone();
        c.storage = self.storage.clone();
        c.storage.schema = "otel".into();
        c.duckdb = self.duckdb.clone();
        c.auth.api_tokens = self.server.api_tokens.clone();
        c.auth.ingest_tokens = self.server.ingest_tokens.clone();
        // otel owns catalog-wide maintenance on the shared catalog (ADR 0001).
        c.maintenance.catalog_wide = true;
        c
    }

    /// The sentry-ducklake config with the shared settings applied.
    pub fn sentry_config(&self) -> sdl_server::Config {
        let mut c = self.sentry.clone();
        c.lake_alias = odl_server::store::CATALOG.into();
        c.schema = "sentry".into();
        c.public_url = format!(
            "{}{SENTRY_PREFIX}",
            self.server.public_url.trim_end_matches('/')
        );
        c.api_token =
            (!self.server.api_tokens.is_empty()).then(|| self.server.api_tokens.join(","));
        c.catalog_wide_maintenance = false;
        // Global DuckDB settings belong to the platform ([duckdb]); a per-
        // library `SET threads` would also throttle otel on the shared DB.
        c.maintenance_threads = 0;
        c
    }
}

/// `OBS_<SECTION>__<KEY>` variables. Names without `__` (e.g. `OBS_CONFIG`,
/// or `OBS_API_TOKEN` used through `${...}` in the file) are not overrides.
fn env_overrides() -> Env {
    Env::prefixed("OBS_")
        .filter(|k| k.as_str().contains("__"))
        .split("__")
}

/// Replace `${NAME}` with the value of environment variable NAME.
/// Comment lines are left alone.
fn expand_env(text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('#') {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        while let Some(i) = rest.find("${") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let end = after.find('}').context("unterminated ${ in config")?;
            let name = &after[..end];
            let val = std::env::var(name)
                .with_context(|| format!("environment variable {name} is not set"))?;
            out.push_str(&val);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn shared_settings_reach_both_libraries() {
        let cfg: Config = Figment::new()
            .merge(Toml::string(
                r#"
                [server]
                public_url = "https://obs.example.com/"
                api_tokens = ["a", "b"]
                [storage]
                catalog = "postgres:host=pg"
                data_path = "s3://bucket/lake/"
                [otel.retention]
                traces = "3d"
                [sentry]
                retention_days = 30
                "#,
            ))
            .extract()
            .unwrap();
        let o = cfg.otel_config();
        assert_eq!(o.storage.data_path, "s3://bucket/lake/");
        assert_eq!(o.storage.schema, "otel");
        assert_eq!(o.retention.traces, "3d");
        assert_eq!(o.auth.api_tokens, vec!["a", "b"]);
        let s = cfg.sentry_config();
        assert_eq!(s.schema, "sentry");
        assert_eq!(s.lake_alias, "lake");
        assert_eq!(s.public_url, "https://obs.example.com/sentry");
        assert_eq!(s.api_tokens(), vec!["a", "b"]);
        assert_eq!(s.retention_days, 30);
        assert!(!s.catalog_wide_maintenance);
        assert_eq!(s.maintenance_threads, 0);
    }

    #[test]
    #[allow(clippy::result_large_err)] // figment's Jail API returns figment::Error
    fn only_section_vars_override() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("OBS_CONFIG", "/etc/x.toml");
            jail.set_env("OBS_API_TOKEN", "t");
            jail.set_env("OBS_SERVER__PUBLIC_URL", "https://x.example");
            jail.set_env("OBS_OTEL__RETENTION__TRACES", "3d");
            let cfg: Config = Figment::new().merge(env_overrides()).extract()?;
            assert_eq!(cfg.server.public_url, "https://x.example");
            assert_eq!(cfg.otel.retention.traces, "3d");
            Ok(())
        });
    }

    #[test]
    fn tokens_split_on_commas() {
        let mut cfg = Config::default();
        cfg.server.api_tokens = vec!["a, b".into(), "".into()];
        cfg.server.ingest_tokens = vec!["".into()];
        cfg.normalize();
        assert_eq!(cfg.server.api_tokens, vec!["a", "b"]);
        assert!(cfg.server.ingest_tokens.is_empty());
    }

    #[test]
    fn env_expansion() {
        // SAFETY: test-only, single-threaded use of a unique variable name.
        unsafe { std::env::set_var("OBS_TEST_SECRET_X", "s3cr3t") };
        assert_eq!(
            expand_env("k = \"${OBS_TEST_SECRET_X}\"").unwrap(),
            "k = \"s3cr3t\""
        );
        assert!(expand_env("${OBS_TEST_MISSING_Y}").is_err());
        assert_eq!(
            expand_env("  # ${OBS_TEST_MISSING_Y}\n").unwrap(),
            "  # ${OBS_TEST_MISSING_Y}\n"
        );
    }
}
