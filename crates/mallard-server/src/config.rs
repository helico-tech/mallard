//! Platform configuration.
//!
//! One TOML file plus `MALLARD_<SECTION>__<KEY>` environment variables, e.g.
//! `MALLARD_STORAGE__DATA_PATH=s3://bucket/lake/`,
//! `MALLARD_OTEL__RETENTION__TRACES=0`, `MALLARD_SENTRY__RETENTION__EVENTS=30d`.
//! String settings keep the variable's text as is; `${NAME}` inside a
//! string value is replaced by environment variable NAME
//! (see [`mallard_common::config`]). Unknown keys are errors.
//!
//! `[server]`, `[storage]`, `[duckdb]` and `[bridge]` are mallard's.
//! `[otel]` and `[sentry]` hold only the settings each side owns.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use mallard_common::{LogFormat, duration};
use mallard_duck::{DuckDbConfig, StorageConfig};
use serde::{Deserialize, Serialize};

pub const SENTRY_PREFIX: &str = "/sentry";
pub const OTEL_PREFIX: &str = "/otel";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    /// DuckLake catalog, data path, secrets and the sourcemap location,
    /// shared by both sides.
    pub storage: StorageConfig,
    pub duckdb: DuckDbConfig,
    pub otel: OtelConfig,
    pub sentry: mallard_sentry_server::Config,
    pub bridge: BridgeConfig,
}

/// `[bridge]`: what crosses between the two sides (docs/adr/0009-bridge.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BridgeConfig {
    /// Store Sentry transactions and streamed spans (`span` items, ADR
    /// 0013) as OTel traces.
    pub transactions: bool,
    /// Turn exceptions recorded by OTel SDKs (span events, logs) into
    /// Sentry events and issues.
    pub exceptions: bool,
    /// Project slug for OTel exceptions whose `service.name` matches no
    /// project (created when missing; a project that is the same app under the slug rule is
    /// used instead, ADR 0031). Empty: one new project per service.
    pub default_project: String,
    /// How long a bridged item waits for its copy from the other side
    /// (dedupe, ADR 0009 and 0010): an OTel exception for a Sentry SDK
    /// event of the same trace + span, a Sentry transaction or span item for
    /// OTLP spans with its span ids. Copies that came are not stored twice.
    #[serde(with = "duration")]
    pub dedupe_window: Duration,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        BridgeConfig {
            transactions: true,
            exceptions: true,
            default_project: String::new(),
            dedupe_window: Duration::from_secs(10),
        }
    }
}

impl BridgeConfig {
    pub fn validate(&self) -> Result<()> {
        let p = &self.default_project;
        anyhow::ensure!(
            p.is_empty()
                || (p.len() <= 64
                    && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
                    && !p.bytes().all(|b| b.is_ascii_digit())),
            "bridge.default_project ({p:?}) must be a project slug: 1-64 chars of a-z, 0-9, - or _, not only digits"
        );
        anyhow::ensure!(
            self.dedupe_window <= Duration::from_secs(600),
            "bridge.dedupe_window must be at most 10m (exceptions and transactions wait in memory)"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Sentry ingest, both query APIs, OTLP/HTTP, health and metrics.
    pub http_addr: String,
    pub otlp_http_addr: String,
    pub otlp_grpc_addr: String,
    /// Serve `/metrics` on this address instead of `http_addr` (keeps it off
    /// a public ingress), e.g. `0.0.0.0:9090`. Empty = on `http_addr`.
    pub metrics_addr: String,
    /// How clients reach `http_addr`; used to build Sentry DSNs.
    pub public_url: String,
    /// Bearer tokens for the query/management APIs of both sides. Required.
    pub api_tokens: Vec<String>,
    /// Bearer tokens for OTLP ingest; empty = open. (Sentry ingest uses DSN keys.)
    pub ingest_tokens: Vec<String>,
    pub log_format: LogFormat,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            http_addr: "0.0.0.0:8080".into(),
            otlp_http_addr: "0.0.0.0:4318".into(),
            otlp_grpc_addr: "0.0.0.0:4317".into(),
            metrics_addr: String::new(),
            public_url: "http://localhost:8080".into(),
            api_tokens: vec![],
            ingest_tokens: vec![],
            log_format: LogFormat::Text,
        }
    }
}

/// `[otel]`: the settings the telemetry side owns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OtelConfig {
    pub ingest: mallard_otel_server::config::IngestConfig,
    pub retention: mallard_otel_server::config::RetentionConfig,
    pub maintenance: mallard_otel_server::config::MaintenanceConfig,
    pub query: mallard_otel_server::config::QueryConfig,
}

impl Config {
    /// Load `path` (optional) and apply `MALLARD_*` environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        Self::finish(mallard_common::config::load(path, "MALLARD_")?)
    }

    /// [`Config::load`] from text and explicit environment variables.
    pub fn load_from(text: Option<&str>, vars: impl IntoIterator<Item = (String, String)>) -> Result<Config> {
        Self::finish(mallard_common::config::load_from(text, "MALLARD_", vars)?)
    }

    fn finish(mut cfg: Config) -> Result<Config> {
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
        // One policy for both query APIs: a token is required.
        anyhow::ensure!(
            !self.server.api_tokens.is_empty(),
            "server.api_tokens must contain at least one token (e.g. MALLARD_SERVER__API_TOKENS=secret)"
        );
        self.otel_config().validate()?;
        self.sentry.validate()?;
        self.sentry_host().validate().context("server.public_url")?;
        self.bridge.validate()?;
        let artifacts = &self.storage.artifacts_path;
        anyhow::ensure!(
            artifacts.is_empty() || !is_inside(artifacts, &self.storage.data_path),
            "storage.artifacts_path ({artifacts}) must not be inside storage.data_path ({}): \
             lake maintenance deletes files there that the catalog does not know",
            self.storage.data_path
        );
        Ok(())
    }

    /// The telemetry config with the shared settings applied.
    pub fn otel_config(&self) -> mallard_otel_server::config::Config {
        let o = &self.otel;
        mallard_otel_server::config::Config {
            storage: StorageConfig { artifacts_path: String::new(), ..self.storage.clone() },
            ingest: o.ingest.clone(),
            retention: o.retention.clone(),
            maintenance: o.maintenance.clone(),
            query: o.query.clone(),
            auth: mallard_otel_server::config::AuthConfig {
                api_tokens: self.server.api_tokens.clone(),
                ingest_tokens: self.server.ingest_tokens.clone(),
            },
            duckdb: self.duckdb.clone(),
            commit_lock: Default::default(),
        }
    }

    /// What mallard decides for the Sentry side. Otel owns catalog-wide
    /// maintenance (ADR 0001); global DuckDB settings belong to `[duckdb]`.
    pub fn sentry_host(&self) -> mallard_sentry_server::Host {
        let artifacts = &self.storage.artifacts_path;
        mallard_sentry_server::Host {
            public_url: format!("{}{SENTRY_PREFIX}", self.server.public_url.trim_end_matches('/')),
            api_tokens: self.server.api_tokens.clone(),
            artifacts: (!artifacts.is_empty()).then(|| mallard_sentry_server::Artifacts {
                url: artifacts.clone(),
                options: mallard_duck::object_store_options(&self.storage, artifacts),
            }),
            catalog_maintenance: None,
            maintenance_threads: 0,
            commit_lock: Default::default(),
        }
    }
}

/// True when location `inner` is `outer` or below it (paths or URLs).
fn is_inside(inner: &str, outer: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().trim_start_matches("file://").trim_start_matches("./");
        format!("{}/", s.trim_end_matches('/'))
    };
    norm(inner).starts_with(&norm(outer))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn load(text: &str, vars: &[(&str, &str)]) -> Result<Config> {
        Config::load_from(Some(text), vars.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    const TOKEN: &[(&str, &str)] = &[("MALLARD_SERVER__API_TOKENS", "t")];

    #[test]
    fn example_config_loads() {
        let text = include_str!("../../../config/mallard.example.toml");
        let cfg = load(text, &[("MALLARD_API_TOKEN", "t")]).unwrap();
        assert_eq!(cfg.server.api_tokens, ["t"]);
    }

    #[test]
    fn an_api_token_is_required() {
        // Without one, the OTel query API would be open while Sentry's is closed.
        let err = load("", &[]).unwrap_err();
        assert!(err.to_string().contains("api_tokens"), "{err}");
        // A ${VAR} token that is empty is dropped, so this is still an error.
        assert!(load("server.api_tokens = [\"${T}\"]", &[("T", "")]).is_err());
    }

    #[test]
    fn env_overrides_keep_strings() {
        let cfg = load(
            "",
            &[
                ("MALLARD_SERVER__API_TOKENS", "123"),
                ("MALLARD_SERVER__PUBLIC_URL", "https://x.example"),
                ("MALLARD_OTEL__RETENTION__TRACES", "0"),
                ("MALLARD_SENTRY__RETENTION__EVENTS", "0"),
                ("MALLARD_DUCKDB__MEMORY_LIMIT", "2000000"),
                ("MALLARD_CONFIG", "/etc/x.toml"),
                ("MALLARD_API_TOKEN", "not an override"),
            ],
        )
        .unwrap();
        assert_eq!(cfg.server.api_tokens, ["123"], "a single numeric token");
        assert_eq!(cfg.server.public_url, "https://x.example");
        assert!(cfg.otel.retention.traces.is_zero());
        assert!(cfg.sentry.retention.events.is_zero());
        assert_eq!(cfg.duckdb.memory_limit, Some(mallard_common::ByteSize::mb(2)));
    }

    /// Every setting of the Sentry side can be set from the mallard config.
    #[test]
    fn sentry_settings_come_from_the_config() {
        let cfg = load(
            "",
            &[
                ("MALLARD_SERVER__API_TOKENS", "t"),
                ("MALLARD_SENTRY__RETENTION__EVENTS", "30d"),
                ("MALLARD_SENTRY__INGEST__MAX_EVENT_BYTES", "2MiB"),
                ("MALLARD_SENTRY__INGEST__MAX_BODY_BYTES", "30MiB"),
                ("MALLARD_SENTRY__INGEST__CONCURRENCY", "7"),
                ("MALLARD_SENTRY__INGEST__QUEUE_CAPACITY", "11"),
                ("MALLARD_SENTRY__INGEST__BATCH_MAX", "13"),
                ("MALLARD_SENTRY__INGEST__BATCH_WINDOW", "20ms"),
                ("MALLARD_SENTRY__INGEST__TRUST_FORWARDED_FOR", "true"),
                ("MALLARD_SENTRY__MAINTENANCE__FLUSH_INTERVAL", "1m"),
                ("MALLARD_SENTRY__MAINTENANCE__FLUSH_ROWS", "17"),
                ("MALLARD_SENTRY__MAINTENANCE__COMPACT_INTERVAL", "2h"),
                ("MALLARD_SENTRY__QUERY__READERS", "3"),
                ("MALLARD_SENTRY__QUERY__ENABLE_SQL", "true"),
                ("MALLARD_SENTRY__SOURCEMAPS__CACHE_SIZE", "64MiB"),
            ],
        )
        .unwrap();
        let s = &cfg.sentry;
        assert_eq!(s.retention.events, Duration::from_secs(30 * 86_400));
        assert_eq!(s.ingest.max_event_bytes, mallard_common::ByteSize::mib(2));
        assert_eq!(s.ingest.max_body_bytes, mallard_common::ByteSize::mib(30));
        assert_eq!((s.ingest.concurrency, s.ingest.queue_capacity, s.ingest.batch_max), (7, 11, 13));
        assert_eq!(s.ingest.batch_window, Duration::from_millis(20));
        assert!(s.ingest.trust_forwarded_for);
        assert_eq!(s.maintenance.flush_interval, Duration::from_secs(60));
        assert_eq!(s.maintenance.flush_rows, 17);
        assert_eq!(s.maintenance.compact_interval, Duration::from_secs(7200));
        assert_eq!(s.query.readers, 3);
        assert!(s.query.enable_sql);
        assert_eq!(s.sourcemaps.cache_size, mallard_common::ByteSize::mib(64));
    }

    /// Unset settings keep the library's defaults, and in mallard otel owns
    /// catalog-wide maintenance (ADR 0001).
    #[test]
    fn an_empty_config_keeps_the_sentry_defaults() {
        let cfg = load("", TOKEN).unwrap();
        assert_eq!(format!("{:?}", cfg.sentry), format!("{:?}", mallard_sentry_server::Config::default()));
        let host = cfg.sentry_host();
        assert_eq!((host.catalog_maintenance, host.maintenance_threads), (None, 0));
    }

    #[test]
    fn interpolated_values_may_contain_quotes() {
        let pw = r#"p"w\d'x"#;
        let cfg = load(
            "[storage]\ncatalog = \"postgres:password=${PW}\" # ${NOT_SET_IN_A_COMMENT}\n\
             [server]\napi_tokens = [\"${MALLARD_API_TOKEN}\"]",
            &[("PW", pw), ("MALLARD_API_TOKEN", "a,b")],
        )
        .unwrap();
        assert_eq!(cfg.storage.catalog, format!("postgres:password={pw}"));
        assert_eq!(cfg.server.api_tokens, ["a", "b"]);
    }

    #[test]
    fn log_format_is_mallards() {
        assert_eq!(load("", TOKEN).unwrap().server.log_format, LogFormat::Text);
        assert_eq!(load("[server]\nlog_format = \"json\"", TOKEN).unwrap().server.log_format, LogFormat::Json);
    }

    #[test]
    fn unknown_and_overridden_keys_are_rejected() {
        for bad in [
            "[server]\ntypo = 1",
            "[sentry]\nretention_days = 30",
            "[sentry]\nstorage_url = \"x\"",
            "[sentry]\npublic_url = \"http://x\"",
            "[otel.server]\napi_addr = \"x\"",
            "[otel.storage]\ncatalog = \"x\"",
            "[otel.auth]\napi_tokens = []",
            "[otel.maintenance]\ncatalog_wide = false",
            "[storage]\nschema = \"x\"",
            "[server]\nlog_format = \"xml\"",
        ] {
            assert!(load(bad, TOKEN).is_err(), "{bad}");
        }
        assert!(load("", &[("MALLARD_SENTRY__TYPO", "1"), TOKEN[0]]).is_err());
    }

    #[test]
    fn shared_settings_reach_both_sides() {
        let cfg = load(
            r#"
            [server]
            public_url = "https://mallard.example.com/"
            api_tokens = ["a", "b"]
            [storage]
            catalog = "postgres:host=pg"
            data_path = "s3://bucket/lake/"
            artifacts_path = "s3://bucket/artifacts"
            [[storage.secrets]]
            type = "s3"
            key_id = "k"
            secret = "s"
            endpoint = "minio:9000"
            use_ssl = false
            url_style = "path"
            [otel.retention]
            traces = "3d"
            [sentry.retention]
            events = "30d"
            "#,
            &[],
        )
        .unwrap();
        let o = cfg.otel_config();
        assert_eq!(o.storage.data_path, "s3://bucket/lake/");
        assert_eq!(o.retention.traces, Duration::from_secs(3 * 86_400));
        assert_eq!(o.auth.api_tokens, ["a", "b"]);
        assert_eq!(cfg.sentry.retention.events, Duration::from_secs(30 * 86_400));
        let h = cfg.sentry_host();
        assert_eq!(h.public_url, "https://mallard.example.com/sentry");
        assert_eq!(h.api_tokens, ["a", "b"]);
        assert_eq!(h.catalog_maintenance, None);
        assert_eq!(h.maintenance_threads, 0);
        let a = h.artifacts.unwrap();
        assert_eq!(a.url, "s3://bucket/artifacts");
        assert_eq!(a.options.endpoint.as_deref(), Some("http://minio:9000"));
        assert!(a.options.path_style);
        assert_eq!(a.options.key_id.as_deref(), Some("k"));
    }

    #[test]
    fn artifacts_must_not_live_inside_the_lake() {
        let with = |a: &str| {
            load(&format!("[storage]\ndata_path = \"s3://bucket/lake/\"\nartifacts_path = \"{a}\""), TOKEN).map(|_| ())
        };
        assert!(with("s3://bucket/lake/artifacts").is_err());
        assert!(with("s3://bucket/lake").is_err());
        with("s3://bucket/lake-artifacts").unwrap();
        with("s3://bucket/artifacts").unwrap();
    }

    #[test]
    fn bridge_is_on_by_default_and_checked() {
        let cfg = load("", TOKEN).unwrap();
        assert!(cfg.bridge.transactions && cfg.bridge.exceptions);
        assert_eq!(cfg.bridge.dedupe_window, Duration::from_secs(10));
        let cfg = load(
            "[bridge]\ntransactions = false\nexceptions = false\ndefault_project = \"otel\"\ndedupe_window = \"30s\"",
            TOKEN,
        )
        .unwrap();
        assert!(!cfg.bridge.transactions && !cfg.bridge.exceptions);
        assert_eq!(cfg.bridge.default_project, "otel");
        assert_eq!(cfg.bridge.dedupe_window, Duration::from_secs(30));
        let cfg = load("", &[TOKEN[0], ("MALLARD_BRIDGE__EXCEPTIONS", "false")]).unwrap();
        assert!(!cfg.bridge.exceptions, "env override");
        for bad in [
            "[bridge]\ntypo = 1",
            "[bridge]\ndefault_project = \"My Project\"",
            "[bridge]\ndefault_project = \"123\"",
            "[bridge]\ndedupe_window = \"1h\"",
        ] {
            assert!(load(bad, TOKEN).is_err(), "{bad}");
        }
    }

    /// Messages name the key as it is written in the mallard config.
    #[test]
    fn validation_messages_name_mallard_keys() {
        for (bad, key) in [
            ("[otel.query]\ntimeout = \"0\"", "otel.query.timeout"),
            ("[otel.ingest]\nflush_rows = 0", "otel.ingest.flush_rows"),
            ("[otel.query]\nmax_concurrent = 0", "otel.query.max_concurrent"),
            ("[sentry.query]\nmax_window = \"0\"", "sentry.query.max_window"),
            ("[sentry.ingest]\nbatch_max = 0", "sentry.ingest.batch_max"),
            ("[sentry.query]\nreaders = 0", "sentry.query.readers"),
        ] {
            let err = format!("{:#}", load(bad, TOKEN).unwrap_err());
            assert!(err.starts_with(key), "{bad}: {err}");
        }
    }

    #[test]
    fn tokens_split_on_commas() {
        let cfg = load("[server]\napi_tokens = [\"a, b\", \"\"]\ningest_tokens = [\"\"]", &[]).unwrap();
        assert_eq!(cfg.server.api_tokens, ["a", "b"]);
        assert!(cfg.server.ingest_tokens.is_empty());
    }
}
