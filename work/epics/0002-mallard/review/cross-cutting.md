# mallard: cross-cutting review

Repo: `/Volumes/Samsung SSD 4TB/Projects/mallard`. Paths below are relative to it.
Read-only review; no files in the repo were changed.

How things were checked:
- Read the code in full for `crates/mallard*`, both servers' `lib.rs`/`main.rs`/`config.rs`,
  auth, API error types, DuckDB setup, both clients, CLI entry points, themes and key handlers,
  both loadgens and `deploy/`.
- Ran `target/debug/mallard` (already built) against a closed port to confirm the CLI findings.
- Ran a tiny scratch crate on `figment 0.10.19` to confirm the env-override bug (B2).
- Ran `cargo tree -i` / read `Cargo.lock` for duplicate versions.
- Not run: `cargo test`, clippy, Helm template rendering, Docker build.

Severity: **bug** = wrong behaviour now; **high** = security, data loss or a big structural
problem; **medium** = real inconsistency or friction; **low** = cleanup.

Totals: bug 4, high 7, medium 17, low 15 (job 1: bug 4, high 2, medium 4, low 12; job 2: high 5, medium 13, low 3).

---

## 1. Review of mallard-*, loadgens, workspace and deploy

### B1 (bug): if a listener fails, the server keeps running without it
`crates/mallard-server/src/lib.rs:80`, `:87-94`
- The gRPC address is parsed but not bound up front: `serve_with_shutdown(addr)` binds inside the task.
- None of the three tasks cancels `shutdown` when it ends.
- So if port 4317 is busy, or any server returns an error, the other two keep serving. The error
  only shows up on SIGTERM. `odl-server` already gets this right (`crates/odl-server/src/lib.rs:162`, `:171-204`).

**Fix:** bind the gRPC `TcpListener` together with the other two and serve it through
`TcpIncoming::from(listener)`. In each task, call `sd.cancel()` after `serve` returns, as odl does.

### B2 (bug): `MALLARD_*` env overrides fail for string values that look like numbers, and for lists
`crates/mallard-server/src/config.rs:156-158`
- figment's `Env` provider guesses each value's type. Verified with figment 0.10.19:
  - `MALLARD_OTEL__RETENTION__TRACES=0` gives "found unsigned int `0`, expected a string".
    "0" is the documented way to keep data forever.
  - `MALLARD_SERVER__API_TOKENS=tok` gives "expected a sequence". It only works as `["tok"]`.
  - `MALLARD_SERVER__PUBLIC_URL=123` and numeric-only tokens fail the same way.
- odl's own loader handled this on purpose (`crates/odl-server/src/config.rs:443-454`: it keeps the
  raw text when the existing value is a string).

**Fix:** replace figment with odl's loader (`merge` + `apply_env` + `interpolate`), made generic and
moved to the shared crate (see 2.A). Add tests for `=0` and for a single token.

### B3 (bug): `${VAR}` is expanded as raw text before the TOML is parsed
`crates/mallard-server/src/config.rs:162-182`
- A value that contains `"`, `\` or a newline breaks the file or changes its structure. Examples: a
  Postgres password with a quote in compose's `catalog = "${MALLARD_CATALOG}"`
  (`deploy/compose/mallard.toml:7`), or a token in Helm's `api_tokens = ["${MALLARD_API_TOKEN}"]`
  (`deploy/helm/mallard/templates/configmap.yaml:17`).
- Only full comment lines are skipped. A trailing `# ${X}` comment is still expanded, and the
  load fails if `X` is unset.

**Fix:** parse first, then replace `${…}` inside string values only. odl already does this
(`crates/odl-server/src/config.rs:469-491`).

### B4 (bug): Sentry events get no client IP in mallard
`crates/mallard-server/src/lib.rs:88-89`
- The server is started as `axum::serve(http_l, http)`, without `ConnectInfo`.
- `crates/sdl-server/src/ingest.rs:93` reads `ConnectInfo<SocketAddr>` and falls back to `None`.
  So `{{auto}}` user IPs and `client_ip` are empty unless `trust_forwarded_for` is set.
  The standalone binary sets it up (`crates/sdl-server/src/main.rs:105`).

**Fix:** `axum::serve(http_l, http.into_make_service_with_connect_info::<SocketAddr>())`. Do the
same on the OTLP listener. Add an e2e assertion on `user.ip_address`.

### 1.H1 (high): the OTel query API is open when no API token is set; the Sentry API is closed
`crates/mallard-server/src/config.rs:87-97`, `crates/odl-server/src/auth.rs:14-21`, `crates/sdl-server/src/lib.rs:91-93`
- odl treats an empty token list as "no auth". It also refused empty tokens on purpose ("almost
  always a missing secret", `crates/odl-server/src/config.rs:334-342`).
- mallard's `normalize()` drops empty entries before odl's check runs. So an unset
  `MALLARD_API_TOKEN` (or a config without `api_tokens`) serves `/otel/api/v1/*` to anyone:
  traces, log bodies and stats. `/sentry/v1/*` rejects everything in the same setup.
- Helm requires a token (`_helpers.tpl:75`). Plain `mallard serve` and the default config do not.

**Fix:** in `Config::validate`, require at least one `server.api_tokens` entry, unless an explicit
`server.allow_anonymous_api = true` is set. Use one token policy for both sides (2.C).

### 1.H2 (high): `[sentry]` and `[otel]` accept settings that mallard silently ignores or overrides
`crates/mallard-server/src/config.rs:30-37`, `:117-142`; `crates/sdl-server/src/config.rs:15-16`
- `[sentry]` deserializes the whole standalone `sdl_server::Config`:
  `listen`, `catalog`, `lake_alias`, `schema`, `api_token`, `public_url`, `duckdb_*`,
  `extension_dir`, `maintenance_threads`, `catalog_wide_maintenance`, `snapshot_retention`.
  mallard overrides or ignores all of them.
- That struct also has no `deny_unknown_fields`, so typos in `[sentry]` are silently dropped.
- `[otel]` includes `[otel.server]`, `[otel.storage]`, `[otel.auth]`, `[otel.duckdb]` and
  `maintenance.catalog_wide`, which are all overwritten without a warning.

**Fix:** give mallard section types that contain only the settings each library owns, with
`deny_unknown_fields`:
- otel: embed `odl_server::config::{IngestConfig, RetentionConfig, MaintenanceConfig, QueryConfig}`
  directly (drop `catalog_wide` from the user surface).
- sentry: split `sdl_server::Config` into a host part (listen, catalog, alias, schema, tokens,
  duckdb, public_url) and a library part (retention, batching, limits, sourcemaps). mallard embeds
  only the library part.

### 1.M1 (medium): `mallard trace` prints untrusted text without cleaning it
`crates/mallard/src/trace.rs:50-101` (also `status`, `crates/mallard/src/main.rs:131-153`)
- Span names, service names, error titles and log bodies come from SDKs. OTLP ingest is open by
  default. They are printed raw, so escape sequences can reach the terminal.
- sdl-cli strips these (`crates/sdl-cli/src/fmt.rs:173-213`, test `crates/sdl-cli/tests/cli.rs:80`).

**Fix:** pass every server string through one shared `clean()` (2.N).

### 1.M2 (medium): `/metrics` is public, and histograms differ from the standalone binary
`crates/mallard/src/server_cmd.rs:27`, `crates/mallard-server/src/lib.rs:68-70`
- `/metrics` is served only on the public `http_addr`. The Helm ingress routes `/`, so the metrics
  are reachable from the internet. sdl had `--metrics-listen` to avoid exactly this
  (`crates/sdl-server/src/main.rs:24-27`).
- mallard installs the recorder without the buckets sdl sets (`crates/sdl-server/src/main.rs:79-85`).
  Its histograms come out as summaries, so dashboards built on sdl do not work on mallard.

**Fix:** add an optional `server.metrics_addr`. Put the recorder setup, with the buckets, in one
shared function.

### 1.M3 (medium): the Helm docs say OTLP/HTTP works from browsers, but the server sends no CORS headers for it
`deploy/helm/mallard/values.yaml:176-177`, `crates/odl-server/src/otlp/http.rs:31-40`
- The values file says "Sentry browser SDKs and OTLP/HTTP both work through it. The server answers
  CORS itself."
- Only sdl ingest has a `CorsLayer` (`crates/sdl-server/src/ingest.rs:33`). Browser OTel exporters
  fail the preflight request.

**Fix:** add the same CORS layer to the OTLP router (shared helper), or correct the text.

### 1.M4 (medium): the three parts of a trace overview use different time windows, and one failure hides the rest
`crates/mallard-client/src/lib.rs:68-95`
- Default lookback: spans 7d (server default), errors 30d (`crates/sdl-server/src/api.rs:299`),
  logs a hard-coded 7d. Logs are silently cut at 1000.
- `try_join!` fails the whole overview if one side fails. For example, `--since 60d` exceeds otel's
  31d `max_window` and you see nothing, not even the errors.

**Fix:** use one default window for all three. Return partial results with an error per source, or
clamp to otel's maximum. Report when the log list was truncated.

### 1.L1 (low): `mallard status --json` output shape
`crates/mallard/src/main.rs:124-127`. Serializing `Result<Value, String>` produces
`{"sentry":{"Ok":{…}},"otel":{"Err":"…"}}`, which is an awkward external API. The two `.unwrap()`
calls can be avoided too.
**Fix:** emit `{"sentry":{"ok":true,"info":{…}}}` / `{"ok":false,"error":"…"}`.

### 1.L2 (low): trace ids are silently mangled
`crates/mallard-client/src/lib.rs:69`, `crates/odl-client/src/lib.rs:111`. Non-hex characters are
dropped (`abc-xyz` becomes `abc`) and the length is never checked. odl-client also does not lowercase.
**Fix:** one `TraceId::parse` (32 hex characters, or a UUID with dashes) that returns an error.

### 1.L3 (low): `/readyz` reaches into sdl's router and ignores otel
`crates/mallard-server/src/lib.rs:60-67`, `:117-124`. It builds a fake request against sdl's whole
router and does not check otel at all.
**Fix:** add `ready()` on both `Server` types and combine them (2.L).

### 1.L4 (low): logging setup
`crates/mallard/src/server_cmd.rs:21-26`.
- `log_format` is a free string: any value other than `"json"` silently means text.
- There is no ANSI check (odl turns colors off when stdout is not a terminal, `crates/odl-server/src/main.rs:52`).
- The default filter is plain `info`; odl quiets tower_http, h2 and tonic.

**Fix:** a `LogFormat` enum and one `init_tracing` (2.K).

### 1.L5 (low): a third copy of the extension install code
`crates/mallard/src/server_cmd.rs:10-11`, `:40-50`. The list differs from both libraries: it adds
`json` and `parquet`, which are compiled in, and `mysql`, which sdl skips. odl's list has `icu`. The
quoting is done by hand.
**Fix:** one constant and one `install_extensions` in `mallard-duck` (2.D).

### 1.L6 (low): `mallard check` validates less than `otel-ducklake check`, and the example config is untested
`crates/mallard/src/server_cmd.rs:13-17`. It only parses the config; odl's `check` also attaches the
storage (`crates/odl-server/src/main.rs:92-98`). No test parses `config/mallard.example.toml`; odl has
`example_config_parses` (`crates/odl-server/src/config.rs:571`).
**Fix:** add `check --connect` and the test.

### 1.L7 (low): the e2e test picks ports with a race
`crates/mallard-server/tests/e2e.rs:16-25`. `free_port()` binds, drops the socket, then reuses the
number. This can flake in parallel CI.
**Fix:** let `Platform` serve on listeners passed in (`serve_with_listeners`) and bind `:0` in tests.

### 1.L8 (low): public API polish (Rust API Guidelines)
- `mallard_client::Client` and `odl_client::Client` do not implement `Debug` (C-DEBUG).
- The `trace_overview` / `trace_overview_since` pair is better as one function taking a small options struct.
- `Platform::config()` is unused.
- `readyz` and `bind` could be methods.

### 1.L9 (low): `odl-loadgen` runs forever on a bad `--duration`, and can underflow
- `crates/odl-loadgen/src/main.rs:853`: `parse_duration(..).unwrap_or(ZERO)` turns a typo into "run until Ctrl-C".
- `crates/odl-loadgen/src/main.rs:827`: `end - span_ns` underflows (panics in debug builds) for a huge `--backfill`.

**Fix:** parse durations with clap `value_parser = humantime::parse_duration`, and use `checked_sub`.

### 1.L10 (low): `sdl-loadgen` writes its own duration parser and panics on `--reps 0`
`crates/sdl-loadgen/src/main.rs:73-83`. This hand-written parser has no `d`, and an empty unit means
seconds, although `humantime` is already a workspace dependency. `:349` calls `last.unwrap()`, which
panics when `--reps 0`.
Defaults (`:24-28`) point at standalone port 8000. Against mallard you need
`--url http://localhost:8080/sentry`, and that is not documented.

### 1.L11 (low): Helm `extraConfig` lands inside `[sentry]`
`deploy/helm/mallard/templates/configmap.yaml:70-73`. Extra keys without a table header are added to
`[sentry]`, and `[sentry]` ignores unknown keys (1.H2).
**Fix:** document that `extraConfig` must start with a table header, or render it after a comment
line and validate it.

### 1.L12 (low): workspace hygiene and stale docs
- Workspace `Cargo.toml`: no `[workspace.lints]`; crates do not inherit `rust-version` or
  `repository`; `mallard*` lack `publish = false`.
- ADR 0001 and 0002 still say `obs-server` and `obs sentry …`. Per the house rules, do not rewrite
  accepted ADRs; add a note in a new ADR or in 0004.
- `docs/architecture.md:30` says `mallard-server` owns logging and metrics, but they are set up in
  the binary (`crates/mallard/src/server_cmd.rs`).
- mallard drops the layers the standalone binaries add: `CatchPanicLayer`
  (`crates/sdl-server/src/main.rs:102`) and `CompressionLayer` (`crates/odl-server/src/lib.rs:168`).
  This is probably accidental.

---

## 2. Duplication and inconsistency between Sentry (`sdl-*`) and OTel (`odl-*`)

Ordered by impact. Each item says what each side does, whether unifying is worth it, and the plan.
The shared-crate plan is at the end.

### 2.A (high): three config loaders and three env-var schemes
- **sdl:** one flat struct that is both clap `Args` and serde, read from `SDL_*` env. Durations use
  `humantime_serde`. It has a dead `from_env_prefix` (envy, `OBS_SENTRY_` example).
  `crates/sdl-server/src/config.rs:15-205`
- **odl:** nested TOML defaults, then the file, then `ODL_A__B` env, then `${VAR}` interpolation of
  string values. `load_with_prefix` is dead too. `crates/odl-server/src/config.rs:274-491`
- **mallard:** figment plus raw-text `${}` expansion. `crates/mallard-server/src/config.rs:71-182`.
  This loader has bugs B2 and B3.

**Worth it:** yes. Bugs B2 and B3 exist because of the third copy.

**Plan:**
- Move odl's `merge` / `apply_env` / `interpolate` into `mallard-common::config::load::<T>(path, prefix)`,
  generic over `T: Serialize + DeserializeOwned + Default`.
- mallard uses it with `MALLARD_`. Libraries stop loading config at all: they expose plain serde
  structs.
- Remove `figment`, `envy`, `from_env_prefix` and `load_with_prefix`.
- The standalone binaries keep their clap flags only until story 0002/06 retires them.

### 2.B (medium): six ways to parse durations, plus two different units for byte sizes
**Durations:**
- odl config `parse_duration` accepts `"0"` (`crates/odl-server/src/config.rs:386`).
- sdl uses `humantime_serde` (`crates/sdl-server/src/config.rs:84`), which rejects `"0"`.
- sdl API windows use `humantime::parse_duration` (`crates/sdl-server/src/db/read.rs:31`).
- sdl-cli `fmt::parse_duration` handles s/m/h/d/w (`crates/sdl-cli/src/fmt.rs:127`).
- odl-cli `humantime_secs` is the same function again (`crates/odl-cli/src/tui/mod.rs:684`).
- sdl-loadgen has its own parser (`crates/sdl-loadgen/src/main.rs:73`).

**How config stores them:** odl keeps durations and sizes as `String`, checks them in `validate()`,
then calls `expect("validated")` (`crates/odl-server/src/config.rs:358-372`). sdl uses typed
`Duration`.

**Byte sizes:** odl `parse_bytes` reads `"512MB"` as decimal (`crates/odl-server/src/config.rs:396`).
sdl uses raw integers (`max_event_bytes`) and `sourcemap_cache_mb`, which is MiB
(`crates/sdl-server/src/config.rs:120-125`, `:157-159`).

**Worth it:** yes. It is small, pure code, and users see the differences ("0" works in `[otel]` but
not in `[sentry]").

**Plan:** add to `mallard-common`:
- `duration::{parse, serde}`, accepting `"0"` and `w`;
- a `ByteSize` newtype with serde that takes `"512MB"`, `"1GiB"` or a plain number;
- typed `Duration` and `ByteSize` fields in both configs, which removes every `expect("validated")`.

The CLIs and loadgens use the same parser.

### 2.C (high): two bearer-token checks with opposite defaults
- **sdl** `require_token` (`crates/sdl-server/src/api.rs:115-131`, `constant_time_eq` in
  `crates/sdl-server/src/ingest.rs:164`):
  - no tokens means every request is rejected;
  - only `Bearer ` or `bearer ` is accepted;
  - the 401 body is `{"error","detail"}`;
  - tokens are configured as `api_token: Option<String>`, comma separated (`crates/sdl-server/src/config.rs:64-65`).
- **odl** `auth::Tokens` (`crates/odl-server/src/auth.rs:7-42`):
  - no tokens means every request is allowed;
  - the scheme is matched case-insensitively;
  - the 401 body is `{"error":"unauthorized","message"}`;
  - tokens are configured as `Vec<String>`.
- mallard joins its list with `,` (`crates/mallard-server/src/config.rs:136`) and sdl splits it
  again (`crates/sdl-server/src/config.rs:207`).

**Worth it:** yes. This is security behaviour and it causes 1.H1.

**Plan:**
- `mallard-common::auth::Tokens` (odl's version), with an explicit `AuthPolicy { Required, Open }`.
- An axum `require_bearer` middleware and one 401 body, behind the `http` feature.
- sdl's config becomes `api_tokens: Vec<String>`, with `#[serde(alias = "api_token")]` and comma
  splitting for env vars.

### 2.D (high): DuckDB/DuckLake setup is written twice, and the shared copy lives inside the OTel crate
- **sdl** (`crates/sdl-server/src/db/mod.rs:93-195`, `crates/sdl-server/src/storage.rs:53`, `:109-140`):
  - `configure`; `catalog_extensions`, which misses the `postgresql:` prefix;
  - always runs `INSTALL x; LOAD x`;
  - builds secrets from `storage_url` and `AWS_*` env;
  - `redact` only masks `password=` after splitting on spaces (`crates/sdl-server/src/db/mod.rs:324`);
  - its own `sql_str`;
  - identifier check is lowercase only (`crates/sdl-server/src/config.rs:220-225`).
- **odl** (`crates/odl-server/src/store/mod.rs:29-345`):
  - `open_connection`, `configure_duckdb`, `required_extensions`;
  - tries `LOAD` first and installs only if allowed;
  - creates secrets from `[[storage.secrets]]` and attaches with DuckLake options;
  - its `redact` also handles quoted values, `secret=` and `key_id=`;
  - its own `sql_str`;
  - `is_identifier` allows uppercase (`crates/odl-server/src/config.rs:379`).
- **mallard-server** already depends on odl for platform-level types:
  `odl_server::config::{StorageConfig, DuckDbConfig}` and `odl_server::store::{open_connection, CATALOG}`.
- There are three extension lists and three `install-extensions` commands, each with different flags
  (`--extension-dir`, positional names, `--dir`).

**Worth it:** yes. This is the largest real duplication, and it fixes a layering smell: the platform's
shared storage layer is owned by one of the two sides.

**Plan:** new crate `mallard-duck`, which also holds the single `duckdb` version pin. It moves
`StorageConfig`, `DuckDbConfig`, `SecretConfig`, `CatalogKind`, `CATALOG`, `open_connection`,
`required_extensions`, `ALL_EXTENSIONS`, `install_extensions`, `sql_str`, `is_identifier` and `redact`
out of odl.
- odl re-exports them, so its public API stays stable.
- sdl's standalone `Db::open`: delete it when 0002/06 retires the standalone binaries. Until then,
  build a `StorageConfig` from `storage_url` and call `mallard_duck::open_connection`.
- Delete sdl's `configure`, `catalog_extensions`, `install_all_extensions`, `redact` and `sql_str`.
- Not worth sharing: `Ns` versus `format!("{CATALOG}.{schema}")`, and the writers and maintenance
  code (see 2.S).

### 2.E (medium): object-storage credentials are configured twice, in two formats
- The lake uses DuckDB secrets: `[[storage.secrets]]` with `endpoint = "host:9000"`, `use_ssl` and
  `url_style`.
- Sourcemap bundles use sdl's `object_store` with `[sentry] storage_url`, `s3_endpoint` (scheme
  included), `s3_region`, `s3_path_style`, plus `AWS_*` env (`crates/sdl-server/src/storage.rs:40-140`, `:226`).
- Both copies are visible in `deploy/compose/mallard.toml:10-24` and
  `deploy/helm/mallard/templates/configmap.yaml:26-41` versus `:60-69`.

**Worth it:** yes, for mallard users. It is the same bucket configured twice, with different
endpoint syntax.

**Plan:** add `[storage] artifacts_path` to mallard. Build the `object_store` settings from the
`[[storage.secrets]]` entry of type s3/gcs/azure whose scope covers that path, or from the
credential chain. Do this once in `mallard-duck` or `mallard-server`, and remove `[sentry] s3_*`
from the mallard config.

### 2.F (medium): the JSON error bodies use the same field name for different things
- **sdl:** `ApiError { error: <message> }` (`crates/sdl-types/src/lib.rs:229`).
  - The 401 body adds `detail` (`crates/sdl-server/src/api.rs:126`).
  - Rejections from axum's `Query`/`Json` extractors come back as plain text.
- **odl:** `ApiError { error: <code>, message }` (`crates/odl-api/src/lib.rs:28`).
  - The `Q`/`J` extractors map rejections to JSON (`crates/odl-server/src/api/mod.rs:79-143`).
- The clients parse them differently (`crates/sdl-client/src/lib.rs:83-88`,
  `crates/odl-client/src/lib.rs:73-84`).

**Worth it:** yes. Under one host, `/sentry` and `/otel` should answer errors in one shape.

**Plan:**
- One `ApiError { error: code, message }` type in `mallard-common`, re-exported by `sdl-types` and
  `odl-api`.
- `ApiErr`, `Q` and `J` move to `mallard-common` behind the `http` feature.
- Keep `detail` only on the sentry-cli `/api/0` compatibility routes.

### 2.G (high): only the Sentry side exports Prometheus metrics
- **sdl** emits `metrics` counters and histograms (`sdl_*`, e.g. `crates/sdl-server/src/db/writer.rs:366-377`).
- **odl** keeps process-local atomics that are only visible through `/api/v1/stats`
  (`crates/odl-server/src/stats.rs`).
- So mallard's `/metrics` and the Helm ServiceMonitor see no OTel ingest, throttling, flush errors or
  maintenance. That is a gap for an observability product.

**Worth it:** yes.

**Plan:** odl emits `metrics::counter!` and `histogram!` next to `Stats`, with the same naming
(`odl_ingest_items_total{signal}`, `odl_flush_seconds`, …). Keep the `sdl_`/`odl_` prefixes.
One `install_recorder()` with the buckets (1.M2) in `mallard-common`.

### 2.H (medium): two `Window` types with different query parameters
- **sdl** `Window::parse(since, default)`: only `since`, no upper limit, a different default per
  endpoint (14d, 30d), and an `interval` parameter for buckets
  (`crates/sdl-server/src/db/read.rs:23-37`, `crates/sdl-server/src/api.rs:341`).
- **odl** `Window::resolve`: `since`, `from`, `to`, a `max_window` limit, and a `step` parameter
  (`crates/odl-server/src/query/mod.rs:180-240`).

**Worth it:** yes. The CLI flags and the mallard client follow from this.

**Plan:**
- `mallard-common::window::{TimeWindow, Window::resolve, step}` (pure chrono).
- sdl's API accepts `from`, `to` and `step`, with `interval` kept as an alias, and gains a maximum
  window.

### 2.I (medium): the two client crates have different constructors, errors and timeouts
- **sdl-client** (`crates/sdl-client/src/lib.rs:17-63`):
  - `new(&str, &str)`; validates with `url::Url::parse`;
  - 30s timeout;
  - `Error::Url`, `Error::Api { status, message }`, `is_not_found()`;
  - always sends a bearer header, even an empty one;
  - percent-encodes path segments (`segment()`).
- **odl-client** (`crates/odl-client/src/lib.rs:18-58`):
  - `new(&str, Option<String>)`; only checks the `http(s)://` prefix;
  - 60s timeout;
  - `Error::BadUrl`, `Error::Api { status, error, message }`, `status()`;
  - no `Debug`;
  - builds trace paths without encoding.
- mallard passes `""` for a missing token to sdl (`crates/mallard-client/src/lib.rs:53`).

**Worth it:** align the APIs. A shared HTTP core crate is not worth it: each side has about 40 lines
of plumbing.

**Plan:**
- Both use `new(base_url: &str, token: Option<&str>)` and the same `Error` variants:
  `InvalidUrl`, `Http`, `Api { status, code, message }`, with both `status()` and `is_not_found()`.
- Both parse the shared `ApiError` (2.F) and use the same timeout.
- Move `segment()` to `mallard-common`.

### 2.J (medium): two ways to build OpenAPI docs
- **sdl:** a hand-maintained `#[openapi(paths(...))]` list (`crates/sdl-server/src/api.rs:51-63`).
  It uses the `utoipa-scalar` crate, which loads a CDN script with no SRI integrity hash. There is
  no snapshot test.
- **odl:** `utoipa-axum` with `routes!`, so docs and routes cannot drift
  (`crates/odl-server/src/api/mod.rs:150-174`). It has its own Scalar page with SRI (`:197-205`) and
  a snapshot test against `docs/otel/openapi.json`. But odl's `openapi()` repeats the route list
  (`:175-192`), so it can drift inside odl itself.
- The `BearerAuth` modifier is duplicated (`crates/sdl-server/src/api.rs:66-76`,
  `crates/odl-server/src/api/mod.rs:67-77`).

**Worth it:** medium. The duplicated pieces are small but the approaches differ.

**Plan:**
- sdl moves to `utoipa-axum`.
- `mallard-common[http]` gets `BearerAuth` and `docs_routes(spec)` (the SRI-pinned page).
- Drop the `utoipa-scalar` dependency and add an sdl OpenAPI snapshot test.
- Optional: mallard serves a merged spec at `/openapi.json`, with `/sentry` and `/otel` prefixes.

### 2.K (medium): logging, signals and the background-task API are written three times
- **Signal handling:** `crates/sdl-server/src/main.rs:49`, `crates/odl-server/src/main.rs:118`,
  `crates/mallard/src/server_cmd.rs:52` (this one uses `expect`).
- **Tracing setup:** `crates/sdl-server/src/main.rs:42` (`--log-json`/`SDL_LOG_JSON`),
  `crates/odl-server/src/main.rs:48` (`server.log_format`, ANSI check),
  `crates/mallard/src/server_cmd.rs:21`.
- **Background-task API:** sdl's `run_background(fut)` returns `()` and has a separate `shutdown()`
  (`crates/sdl-server/src/lib.rs:126-134`). odl's returns `Result` (`crates/odl-server/src/lib.rs:132`).

**Plan:**
- `mallard-common::{shutdown_signal, LogFormat, init_tracing}`.
- Both `run_background` functions return `anyhow::Result<()>`, so writer errors reach mallard.

### 2.L (medium): health and readiness are not aligned
- **sdl:** `/healthz` and `/readyz` as plain text, with a writer and database check
  (`crates/sdl-server/src/api.rs:106-145`).
- **odl:** `/api/v1/health` as JSON, with no readiness check (`crates/odl-server/src/api/mod.rs:206`).
- mallard therefore exposes five health endpoints, and its readiness only reflects Sentry.

**Plan:**
- Each `Server` gets `fn ready(&self) -> Result<(), String>`: sdl checks writer and database; odl
  checks the flusher, the last flush error and a reader `SELECT 1`.
- mallard owns `/healthz` and `/readyz` and combines both.
- The per-side routes stay for the standalone binaries only.

### 2.M (medium): `mallard errors` and `mallard telemetry` behave like two different CLIs
Checked with `target/debug/mallard`:
- **`-p` position:** `mallard errors issues list -p web` fails with "unexpected argument '-p'". Only
  `mallard errors -p web issues list` works. The arg is not `global`
  (`crates/mallard/src/main.rs:73-79`). In `sdl` it is global.
- **Wrong hints:** errors under mallard say "set SDL_URL", "SDL_TOKEN", "pick one with -p or
  SDL_PROJECT" and "sdl events show …" (`crates/sdl-cli/src/cli.rs:31-53`, `:93`, `:414`, `:710`;
  `crates/sdl-cli/src/tui/mod.rs:193`).
- **Error output:** a telemetry failure prints `Error: request failed: …` followed by a 4-level
  `Caused by:` chain. An errors failure prints a one-line friendly message. This comes from
  `main() -> Result` (`crates/mallard/src/main.rs:88`). The standalone tools print a colored
  `error:` instead.

**Plan:**
- Add `global = true` on `--project`.
- `sdl_cli::Ctx` (and odl's) carry a `Hints { bin: "mallard errors", url_env, token_env, project_env }`.
- mallard's `main` prints every error through one `describe()` from `mallard-term`, then exits with
  code 1.

### 2.N (high): the telemetry CLI prints untrusted OTLP text raw; the errors CLI cleans it
- `crates/odl-cli/src/fmt.rs:340-350` (log body, service name) and `trace_lines` (`:265`) print raw.
- `strip_ansi` (`:24`) only runs when stdout is not a terminal, and it only removes CSI sequences.
  OSC, C1 and bidi-override characters pass through.
- sdl cleans every server string and has a test for it (`crates/sdl-cli/src/fmt.rs:173-213`,
  `crates/sdl-cli/tests/cli.rs:80`).
- Because OTLP ingest is open by default, any sender can write escape sequences to an operator's terminal.

**Plan:** move sdl's `clean`, `clean_text` and `json_safe` to `mallard-term`. odl-cli, the odl TUI
(`widgets::trunc` already replaces control characters, but not bidi) and `mallard trace` (1.M1) all use them.

### 2.O (medium): the two TUI tabs have different themes and conflicting keys
- **Theme:** sdl uses the 256-color palette (`crates/sdl-cli/src/tui/theme.rs:6-22`); odl uses
  truecolor RGB "Tokyo Night" colors (`crates/odl-cli/src/tui/theme.rs:7-18`). F1 and F2 look like two
  apps, and truecolor degrades on terminals without it (open otel issue 0007). The mallard bar uses
  sdl's color 141 (`crates/mallard/src/tui.rs:101`).
- **Keys:**
  - `r` resolves an issue (a write) on F1 (`crates/sdl-cli/src/tui/app.rs:725`) but refreshes on F2
    (`crates/odl-cli/src/tui/mod.rs:442`).
  - Tab cycles the status filter on F1 (`crates/sdl-cli/src/tui/app.rs:694`) but switches views on F2 (`:426`).
  - Digits select statuses 1-4 on F1 but views 1-5 on F2.
  - "Jump to the other side" is `t` on F1 and `e` on F2, while `t` cycles the time window on F2.
- **Labels:** severity labels differ (`" WARN  "` vs `"WARN"`).
- **Duplicated types:** two `Outcome` enums (`crates/sdl-cli/src/tui/mod.rs:31`,
  `crates/odl-cli/src/tui/mod.rs:47`), plus a ~60-line adapter in `crates/mallard/src/tui.rs:19-87`.

**Plan:**
- `mallard-term::tui`:
  - one palette (256-color by default, truecolor as an option);
  - one `Outcome { Consumed, Ignored, Quit, Jump(JumpTarget, String) }`;
  - a `Pane` trait that both libraries implement, so the mallard adapters go away.
- One key map: `r` = refresh everywhere; resolve/ignore/unresolve move to `R`/`I`/`U` or an action
  menu; the same jump key on both tabs (for example `g`).
- Record the key map in an ADR, since ADR 0002 already reserves F1 and F2.

### 2.P (medium): different crates or versions for the same job
- **Tables and colors:** comfy-table **7** (`crates/sdl-cli/Cargo.toml`) and **8**
  (`crates/odl-cli/Cargo.toml`) are both compiled. Colors are hand-written ANSI in
  `crates/sdl-cli/src/paint.rs` and `owo-colors` in odl-cli.
- **rand:** workspace 0.9 (used by odl-loadgen) versus 0.10 inline (sdl-server, sdl-loadgen). The
  lockfile has rand 0.8, 0.9 and 0.10.
- **base64:** workspace 0.22 (odl-server) versus 0.23 (sdl-protocol and the rest of the ecosystem).
  Move the workspace to 0.23.
- **Config parsing:** `toml` 0.9 (odl-server) plus `toml` 0.8 through figment (mallard-server), plus
  `envy` and `humantime-serde` (sdl-server). After 2.A only `toml` remains.
- **sdl-server ignores the workspace versions:**
  - it re-declares `duckdb = "=1.20000.1"` inline, so the "one version" rule is enforced in two places;
  - it also declares `axum`, `tower`, `tower-http`, `humantime`, `metrics-exporter-prometheus`,
    `url` and `rand` inline;
  - odl-server declares `tokio-util = "0.7"` inline; odl-cli and odl-server's dev-dependencies use
    `path = "../odl-client"`.
- **ratatui 0.30 and crossterm 0.29:** declared separately in three crates. ADR 0002 says they must
  match, so put them in `[workspace.dependencies]`. Same for `tempfile`, `url`, `comfy-table`,
  `unicode-width`, and `futures` versus `futures-util`.
- **API types crates:** the names differ (`sdl-types` vs `odl-api`), and so does their use of
  `utoipa`. sdl-types makes it an optional `openapi` feature; odl-api always depends on it, so
  odl-client, odl-cli and the mallard CLI compile utoipa for nothing.
  **Plan:** add an `openapi` feature to odl-api and pick one naming scheme (e.g. `*-types`).
- **Percentiles in loadgens:** `hdrhistogram` in sdl-loadgen versus sort-then-index in odl-loadgen.
  Low impact; fine to leave.

### 2.Q (medium): the same kind of setting has different units and names
- **Retention:** `[sentry] retention_days = 90` (an integer, 0 = forever) versus
  `[otel.retention] traces = "7d"` (a string, "0" = forever). Helm has `retention.errorsDays` next
  to `retention.traces: 7d` (`deploy/helm/mallard/values.yaml:81-87`).
- **Maintenance:** flat `compact_interval` and `snapshot_retention` in `[sentry]` versus
  `[otel.maintenance]`.
- **Sizes:** `sourcemap_cache_mb` (MiB) versus `"512MB"` (decimal).

**Plan:** group sentry's library settings like otel's (`[sentry.retention] events = "90d"`,
`[sentry.ingest]`, `[sentry.maintenance]`), with serde aliases for the old keys. Use the shared
`ByteSize` and duration types (2.B). In Helm: `retention.errors: 90d`.

### 2.R (low): the two CLI libraries name the same things differently
- `sdl_cli::Command` versus `odl_cli::Cmd`.
- `sdl_cli::Embedded` (re-exported at the root) versus `odl_cli::tui::Embedded`.
- `Ctx { json: bool, project }` versus `Ctx { output: Output }`
  (adapter in `crates/mallard/src/main.rs:102-107`).
- Jump methods `show_trace_errors` versus `open_trace`, and the extra `set_errors_enabled` call
  (`crates/mallard/src/tui.rs:146-150`).

**Plan:** use the same names and the same `Ctx` shape (an `Output` enum plus `Hints`).

### 2.M2 (medium): flags and output style differ between the two CLIs
- **Flags:**
  - `--since`: `-s` with a default of `1h` on the telemetry side (`crates/odl-cli/src/lib.rs:33-43`);
    no short flag and a 14d server default on the errors side (`crates/sdl-cli/src/lib.rs:104-106`).
  - `--limit`: `-l` in odl `traces`, `-n` in odl `logs` (where `-l` means level), no short flag in sdl.
  - JSON output: `--json` in sdl versus `-o table|json` in odl.
  - Trace filter: `-t/--trace` in odl logs versus a long-only `--trace` in sdl events.
- **Tables:** bold headers (comfy-table 7, `crates/sdl-cli/src/cli.rs:159-165`) versus cyan headers
  (comfy-table 8, `crates/odl-cli/src/fmt.rs:62-68`).
- **Color rules:** sdl turns color off on `NO_COLOR` or a non-terminal (`crates/sdl-cli/src/paint.rs:9-12`);
  odl also treats an empty `NO_COLOR` as unset and checks `TERM=dumb` (`crates/odl-cli/src/fmt.rs:14-21`).
- **Closed pipes** (e.g. `| head`): sdl's `out!` returns the error and `run` swallows it
  (`crates/sdl-cli/src/cli.rs:16-21`, `:134`); odl's `outln!` calls `exit(0)` (`crates/odl-cli/src/fmt.rs:43-51`).
- **Numbers and sizes:** compact numbers are "1.2k/3.4M/1B" in sdl and "1.2k/1.0M/1.0G" in odl.
  Byte sizes are KiB in sdl (`crates/sdl-cli/src/fmt.rs:83`) and decimal KB in odl (`crates/odl-cli/src/fmt.rs:103`).
- **Time display:**
  - sdl CLI: UTC `%Y-%m-%d %H:%M:%S` (`crates/sdl-cli/src/fmt.rs:122`);
  - odl CLI: UTC `%m-%d %H:%M:%S%.3f`;
  - odl TUI: **local** time (`crates/odl-cli/src/tui/widgets.rs:46`);
  - sdl TUI: UTC;
  - `mallard trace`: `… UTC` (`crates/mallard/src/trace.rs:19`).
- **Text helpers:** sparklines use a zero baseline for u64 in sdl and min-max scaling for f64 in odl.
  Truncation counts characters in sdl (`crates/sdl-cli/src/fmt.rs:95`) but terminal width in odl
  (`crates/odl-cli/src/tui/widgets.rs:57`).

**Plan:** `mallard-term`:
- `out` (color detection, `outln!`, closed-pipe handling) and `table()` with one style;
- `fmt` (compact, bytes, ms, relative time, timestamps with one time-zone rule — UTC by default,
  `--local` as an option);
- width-aware `trunc`;
- shared clap `WindowArgs` (`-s/--since`, `--from`, `--to`) and one flag table:
  `-n/--limit`, `-q/--query`, `-S/--service`, `-t/--trace`, `--json`.

### 2.T (low): test and load-generation helpers are written several times
- **OTLP request builders:** `crates/odl-server/tests/e2e.rs:31-200`,
  `crates/odl-loadgen/src/main.rs:252-560`, and JSON versions in `crates/mallard-server/tests/e2e.rs:90-105`.
- **Sentry envelope builders:** sdl-loadgen, `crates/sdl-cli/src/send_test.rs` and
  `crates/mallard-server/tests/e2e.rs:67-88`.
- **Loadgen env vars:** `SDL_URL`/`SDL_TOKEN` versus `OTLP_ENDPOINT` (not the standard
  `OTEL_EXPORTER_OTLP_ENDPOINT`) and `ODL_INGEST_TOKEN`.
- **Correlated data:** neither loadgen can send errors and spans that share a trace id, which is
  mallard's main feature.

**Worth it:** not as a crate yet.
**Plan:** rename the env vars to `MALLARD_URL`/`MALLARD_TOKEN` and `OTEL_EXPORTER_OTLP_ENDPOINT`.
When correlated load is needed, merge both tools into one `mallard-loadgen` with `errors`, `otlp` and
`mixed` subcommands, and share the builders there.

### 2.U (low): three ways to install extensions
Already covered by 2.D and 1.L5: three `install-extensions` commands and lists
(`crates/sdl-server/src/db/mod.rs:127-137`, `crates/odl-server/src/main.rs:40-45`,
`crates/mallard/src/server_cmd.rs:10-11`). One constant, one function.

### 2.S (note, not counted): what should stay separate
- Write paths: sdl's writer thread on a crossbeam channel versus odl's `WriteBuffer`/`Flusher`.
- Per-schema maintenance: `crates/sdl-server/src/db/writer.rs` versus
  `crates/odl-server/src/store/maintenance.rs`.
- Schema migration style, `$S` placeholders versus `format!`, and small helpers like `ts_value` and `us_to_dt`.

These differ by domain or are tiny. Unifying them would add abstraction without removing real
duplication. Catalog-wide maintenance ownership is already settled by ADR 0001.

---

## Proposed shared crates (the minimum)

Three crates. The split follows the dependencies: clients and CLIs must not pull in DuckDB, and
servers must not pull in ratatui.

1. **`mallard-common`** (light: serde, chrono, humantime, thiserror; feature `http` adds axum,
   utoipa and utoipa-axum; feature `clap` is optional)
   - `duration` (parse plus serde, accepts "0"), `ByteSize`, `window::{TimeWindow, Window}`.
   - `config::load::<T>(path, env_prefix)`: odl's merge, env overrides and interpolation, used by mallard.
   - `auth::{Tokens, AuthPolicy}`; `ApiError { error, message }`.
     With `http`: `ApiErr`, `Q`, `J`, `require_bearer`, `BearerAuth`, `docs_routes`, CORS helper.
   - `shutdown_signal()`, `LogFormat`, `init_tracing()`, `install_prometheus()` (with buckets).
   - `segment()` for client URL paths.
2. **`mallard-duck`** (owns the `duckdb` pin)
   - `StorageConfig`, `DuckDbConfig`, `SecretConfig`, `CatalogKind`, `CATALOG`.
   - `open_connection`, `required_extensions`, `ALL_EXTENSIONS`, `install_extensions`.
   - `sql_str`, `is_identifier`, `redact`.
   - Building the object-store settings for sourcemaps from the storage secrets (2.E).
   - odl re-exports these; sdl's standalone DuckDB code is deleted.
3. **`mallard-term`** (ratatui, crossterm, comfy-table 8, unicode-width, chrono)
   - CLI output: color detection, `outln!` with closed-pipe handling, `table()`, number, size, time
     and duration formatting, `clean()`/`json_safe()`, `describe()` with `Hints`, shared clap
     `WindowArgs`.
   - TUI: one theme, `Outcome`, the `Pane` trait, widgets (`centered`, `trunc`, `hints`, `panel`,
     `sparkline`), and the key map.

Not proposed: a shared HTTP client core (align the two APIs instead, 2.I), a test-kit crate (2.T),
or shared writer/maintenance code (2.S).

**Suggested order:**
1. Fix B1-B4 and 1.H1 directly in mallard. They are small, and the B2/B3 fix is the start of
   `mallard-common::config`.
2. `mallard-common` together with 2.C, 2.F and 2.B.
3. `mallard-duck`, together with 2.D and 2.E. Story 0002/06 (retire the standalone binaries) makes
   this cheaper, because the sdl standalone storage code can then simply be deleted.
4. `mallard-term`, together with 2.N first (security), then 2.M, 2.M2 and 2.O.
5. 2.G (OTel metrics) can run in parallel at any time.
