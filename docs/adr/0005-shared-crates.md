# 0005 — Shared crates: mallard-common and mallard-duck

## Status
accepted

## Context
The repo joined two codebases (ADR 0004). Both servers still carried their
own copy of the same infrastructure, and mallard added a third:

- three config loaders and env-var schemes; mallard's (figment plus raw-text
  `${VAR}`) broke on `MALLARD_OTEL__RETENTION__TRACES=0`, single tokens and
  values with quotes;
- two DuckDB/DuckLake set-ups (extensions, secrets, attach, redaction), with
  the shared one living inside the OTel crate;
- two bearer-token checks with opposite defaults, two JSON error shapes,
  two `Window` types, two OpenAPI set-ups, three logging/signal/metrics
  set-ups;
- `[otel]` and `[sentry]` accepted settings mallard overrides or ignores.

Notes on older ADRs: 0001 and 0002 say `obs-server` / `obs sentry …`; these
are now `mallard-server` and `mallard errors …`. The rest still holds.

## Decision
Two small shared crates, split by dependencies (clients must not pull in
DuckDB or axum):

- `mallard-common`: duration and byte-size parsing with serde, query time
  windows, the `ApiError { error, message }` body (re-exported by `sdl-types`
  and `odl-api`). Feature `server`: the one config loader
  (`config::load::<T>(path, prefix)`), `auth::Tokens` with an explicit
  `AuthPolicy`, HTTP helpers (`ApiErr`, `Q`, `J`, `require_bearer`,
  `ingest_cors`, `BearerAuth`, `docs_routes`, `readiness`), `LogFormat`,
  `init_tracing`, `shutdown_signal`, `install_prometheus`.
- `mallard-duck`: the only crate that depends on `duckdb` (re-exported as
  `mallard_duck::duckdb`). `StorageConfig`, `DuckDbConfig`, `SecretConfig`,
  `CatalogKind`, `CATALOG`, `open_connection`, `required_extensions`,
  `ALL_EXTENSIONS`, `install_extensions`, `sql_str`, `is_identifier`,
  `redact`, `object_store_options` (sourcemap storage from the lake's
  secrets).

Each side keeps what is domain specific (ADR 0001 and the review's 2.S):
writer threads and buffers, per-schema maintenance, schema migrations, SQL
helpers.

Config: libraries expose plain serde structs and never load config.
`sdl_server::Config` holds only the library's settings (mallard's
`[sentry]`), `sdl_server::Host` what the application decides, and
`sdl_server::Standalone` the `sentry-ducklake` flags. mallard's `[otel]` is
`ingest`, `retention`, `maintenance`, `query`. All sections use
`deny_unknown_fields`. Schemas are fixed (`sentry`, `otel`); otel always runs
catalog-wide maintenance, sentry never does inside mallard.

Auth: both query APIs require a token in mallard (`AuthPolicy::Required`);
OTLP ingest is open when no ingest token is set. The standalone
`otel-ducklake` keeps its documented open-when-empty API.

## Consequences
- One place to fix parsing, auth, CORS, DuckDB set-up; one duckdb pin.
- Config is stricter: typos and settings mallard owns are errors. Old keys
  (`[sentry] retention_days`, `storage_url`, `s3_*`, `[storage] schema`,
  `maintenance.catalog_wide`, Helm `retention.errorsDays`) must be moved;
  the error message names the key.
- The Sentry API's error body gained `message` and an `error` code;
  `sdl-client` reads both the old and the new shape.
- Both sides export Prometheus metrics (`sdl_*`, `odl_*`) through one
  recorder with histogram buckets.
- A new shared need goes into one of these crates only when both sides use
  it; terminal output goes to `mallard-term`, not here.
