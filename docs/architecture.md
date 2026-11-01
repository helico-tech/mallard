# Architecture

## Parts

```
                    ┌──────────────────────── mallard (one process) ─────────────────────────────┐
 Sentry SDKs     ──▶│ :8080 HTTP  /sentry/api/{project}/envelope → mallard-sentry-server router  │
 OTel SDKs       ──▶│ :4318 HTTP  /v1/traces|logs|metrics        → mallard-otel-server OTLP/HTTP │
                 ──▶│ :4317 gRPC  OTLP                           → mallard-otel-server gRPC      │
 mallard CLI/TUI ──▶│ :8080 HTTP  /sentry/v1/... sentry API, /api/0 (+ /sentry/api/0) sentry-cli │
                    │             /otel/...    otel query API (nested)                           │
                    │             /api/v1/apps, /api/v1/find   what spans both sides (ADR 0027)  │
                    │             /v1/traces|logs|metrics   OTLP/HTTP (also on :4318)            │
                    │             /healthz /readyz /metrics (or server.metrics_addr)             │
                    └───────────────┬────────────────────────────────────────────────────────────┘
                                    ▼
                one DuckLake catalog (DuckDB file | SQLite | Postgres)
                  schema `sentry`  ← mallard-sentry-server
                  schema `otel`    ← mallard-otel-server
                Parquet data files on local disk or S3/GCS/Azure
```

## Crates

| crate | role |
|---|---|
| `mallard` | The one product binary (ADR 0023): one flat command tree (ADR 0008) mounting `mallard-sentry-cli` and `mallard-otel-cli` commands, plus `status`, `setup`, `apps`, `find`, `trace`, `series`, `help agents`, `tui` (the host holds the app picker and the one time window of both tabs); `--app` is worked out once in `scope.rs`; `errors …`/`telemetry …` are hidden aliases. These speak HTTP only. Feature `server` (on by default) adds `serve`, `check`, `maintenance`, `install-extensions` from `mallard-server`; with `--no-default-features` the same CLI and TUI build with no DuckDB (CI checks `cargo tree`). |
| `mallard-web` | The Web UI behind `mallard web` (ADR 0016): the React app built in, plus a same-origin proxy that holds the token or a signed-in user's session and passes only what the TUI uses. HTTP only, like the TUI; no DuckDB, no server code. |
| `mallard-server` | A library, run by `mallard serve`. Loads config, opens the shared catalog (`mallard_duck::open_connection`), builds both sides on it, mounts their routers, `/api/v1/apps`, `/api/v1/find` (`src/api.rs`), `/healthz`, `/readyz` (both sides) and `/metrics`, runs their background jobs and the shutdown order. The bridge (`src/bridge/`, ADR 0009, 0013, 0021) turns Sentry transactions and streamed spans into OTel spans and OTel exceptions into Sentry events through the libraries' hooks. `commands` is what the server commands run, and the only place that installs logging and the Prometheus recorder (`mallard-common`). |
| `mallard-api` | The types of `/api/v1/...`, shared by `mallard-server` and the CLI, so the slim build needs no server crate. |
| `mallard-common` | Shared by both sides (ADR 0005): duration/size parsing, query windows, the `ApiError` body; with feature `server` the config loader, bearer tokens, HTTP helpers (`ApiErr`, `Q`, `J`, `require_bearer`, CORS, docs page, readiness), logging, signals, Prometheus. |
| `mallard-duck` | The only crate that depends on `duckdb`: `[storage]`/`[duckdb]` types, `open_connection`, extensions, secrets, SQL quoting, identifier check, redaction, object-store options for sourcemaps, and the read-only SQL guard (`sql`) both sides' SQL endpoints use (ADR 0011). |
| `mallard-term` | Terminal output shared by both CLIs, both TUIs and the host: cleaning untrusted text, tables (plain when piped), formats, colour, error kinds and exit codes, the output contract (`--format`, `--fields`, list envelope, `--quiet` notes), window flags and `--wait` polling (ADR 0006, 0008), TUI theme, keys, `Outcome`, `Scope` (the chosen app), the window presets and `tui::suggest` (the filter bar's list, no I/O). Depends on neither client. |
| `mallard-client` | Wraps `mallard-sentry-client` (`<url>/sentry`) and `mallard-otel-client` (`<url>/otel`) behind one base URL + token; trace correlation (spans + errors + logs). |
| `mallard-sentry-*` | Sentry side. See [sentry/architecture.md](sentry/architecture.md). |
| `mallard-otel-*` | OpenTelemetry side. See [otel/architecture.md](otel/architecture.md). |

## Key rules

- Each side owns its domain (ingest, storage schema, maintenance).
  `mallard-*` never writes into their schemas directly.
- The bridge (ADR 0009, 0010, 0013) crosses sides in process through small library
  hooks: `mallard_sentry_server::Server::{set_transaction_sink, set_span_sink,
  set_event_observer, ingest_event}` and `mallard_otel_server::Server::{set_ingest_hook,
  ingest_traces}`. The libraries do not know each other; a library run
  alone leaves the hooks unset.

```
 Sentry SDK ─ transaction, span item ─▶ sentry ingest ─ TransactionSink, SpanSink ─▶ bridge queue
     ─ (dedupe_window, minus spans seen over OTLP) ─ ingest_traces ─▶ otel buffer
 OTel SDK ─ spans ─▶ otel ingest ─ IngestHook ─▶ bridge "OTLP spans"
 OTel SDK ─ span event / log "exception" ─▶ otel ingest ─ IngestHook ─▶ bridge queue
     ─ (dedupe_window, no SDK event for trace+span) ─ ingest_event ─▶ sentry writer
 Sentry SDK ─ error event ─▶ sentry ingest ─ EventObserver ─▶ bridge "seen spans"
```
  Shutdown: receivers stop, the bridge stores what it holds (transactions and span items
  before otel's final flush, exceptions before Sentry's), then both flush.
- One `duckdb` crate version for the workspace (it is bundled and linked
  once): only `mallard-duck` depends on it; everything else uses
  `mallard_duck::duckdb`.
- How to run it: [deployment.md](deployment.md).
- Binaries: only `mallard` (plus the dev load generators `mallard-sentry-loadgen`,
  `mallard-otel-loadgen`); `mallard-server` and the `mallard-sentry-*`/`mallard-otel-*` crates are libraries
  (the old standalone binaries are gone; ADR 0024 lists their names).
- Library crates have no global side effects. Logging and metrics are set up by
  `mallard_server::commands`, which only the server commands of the binary call,
  so CLI commands never get them. Logs go to stderr; stdout is data only (ADR 0008).
- One config file, TOML, sections `[server]`, `[storage]`, `[duckdb]`,
  `[bridge]` (mallard's), `[sentry]`, `[otel]` (only what each side owns). One loader
  (`mallard_common::config`): env overrides `MALLARD_<SECTION>__<KEY>`
  (text settings stay text), `${NAME}` inside parsed strings, unknown keys
  are errors.
- Both query APIs require a bearer token (`server.api_tokens`) and answer
  errors as `{"error": code, "message": text}`.
- Schemas are fixed: `sentry`, `otel`; otel owns catalog-wide maintenance.
- Correlation of one trace lives in `mallard-client`, built on the two library APIs
  (`/sentry/v1/events?trace_id=`, `/otel/api/v1/traces/{id}`,
  `/otel/api/v1/logs?trace_id=`).
- The server's own API is what every client would otherwise have to work out
  itself: which project and services are one app (ADR 0027, `service_slug` is the one
  slug rule, also the bridge's, and project slugs go through it too, so `my_app` joins the
  service `my_app`; with several such projects `app_projects` picks one), and what an id points at. Document:
  `docs/openapi.json`. It calls the libraries' `Server` methods (`service_names`,
  `projects`, `find_event`, …), which run on each side's read path with its limits,
  so it never depends on their table layout. A side that fails adds to `warnings`;
  `find` answers with its error when nothing else was found, and a 404 only when
  every source was read. This goes further than ADR 0008's "no new `mallard-server`
  API", as ADR 0027 decided.
- Trace lists and waterfalls mark Sentry errors without a call per trace:
  `mallard-otel-cli` has an optional `ErrorLookup` hook (it never depends on
  `mallard-sentry-*`); the `mallard` host answers it with one
  `/sentry/v1/events/by-trace?trace_id=a,b,…` call per list.
- Errors and traces correlate on `trace_id` (Sentry `contexts.trace.trace_id`
  = OTel span `trace_id`).
- Ad-hoc SQL is per side (ADR 0011): `/otel/api/v1/sql` over the telemetry
  tables, `/sentry/v1/sql` over `events`, `issues`, `issue_activity`. Both
  are off by default and run through one guard, `mallard_duck::sql` (one
  SELECT, only that side's tables, no files or settings, a timeout, at most
  `limit` rows and 32 MiB per answer; ADR 0015). `mallard sql` picks the side
  with `--errors`; `mallard schema` lists the tables of both.
