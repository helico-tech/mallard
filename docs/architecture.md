# Architecture

## Parts

```
                ┌──────────────────────── mallard (one process) ───────────────────────┐
 Sentry SDKs ──▶│ :8080 HTTP  /sentry/api/{project}/envelope  → sdl-server router            │
 OTel SDKs   ──▶│ :4318 HTTP  /v1/traces|logs|metrics         → odl-server OTLP/HTTP router   │
             ──▶│ :4317 gRPC  OTLP                            → odl-server gRPC services      │
 mallard CLI/TUI ──▶│ :8080 HTTP  /sentry/v1/... sentry API, /sentry/api/0 sentry-cli            │
                │             /otel/...    otel query API (nested)                           │
                │             /v1/traces|logs|metrics   OTLP/HTTP (also on :4318)             │
                │             /healthz /readyz /metrics (or server.metrics_addr)      │
                └───────────────┬────────────────────────────────────────────────────────────┘
                                ▼
                one DuckLake catalog (DuckDB file | SQLite | Postgres)
                  schema `sentry`  ← sdl-server
                  schema `otel`    ← odl-server
                Parquet data files on local disk or S3/GCS/Azure
```

## Crates

| crate | role |
|---|---|
| `mallard` | The CLI/TUI binary (ADR 0007): one flat command tree (ADR 0008) mounting `sdl-cli` and `odl-cli` commands, plus `status`, `setup`, `find`, `trace`, `help agents`, `tui`; `errors …`/`telemetry …` are hidden aliases. HTTP only; no DuckDB, no server code. |
| `mallard-server` | Loads config, opens the shared catalog (`mallard_duck::open_connection`), builds both sides on it, mounts their routers, `/healthz`, `/readyz` (both sides) and `/metrics`, runs their background jobs and the shutdown order. Its `mallard-server` binary installs logging and the Prometheus recorder (`mallard-common`). |
| `mallard-common` | Shared by both sides (ADR 0005): duration/size parsing, query windows, the `ApiError` body; with feature `server` the config loader, bearer tokens, HTTP helpers (`ApiErr`, `Q`, `J`, `require_bearer`, CORS, docs page, readiness), logging, signals, Prometheus. |
| `mallard-duck` | The only crate that depends on `duckdb`: `[storage]`/`[duckdb]` types, `open_connection`, extensions, secrets, SQL quoting, identifier check, redaction, object-store options for sourcemaps. |
| `mallard-term` | Terminal output shared by both CLIs, both TUIs and the host: cleaning untrusted text, tables (plain when piped), formats, colour, error kinds and exit codes, the output contract (`--format`, `--fields`, list envelope) and window flags (ADR 0006, 0008), TUI theme, keys and `Outcome`. Depends on neither client. |
| `mallard-client` | Wraps `sdl-client` (`<url>/sentry`) and `odl-client` (`<url>/otel`) behind one base URL + token; trace correlation (spans + errors + logs). |
| `sdl-*` | Sentry side. See [sentry/architecture.md](sentry/architecture.md). |
| `odl-*` | OpenTelemetry side. See [otel/architecture.md](otel/architecture.md). |

## Key rules

- Each side owns its domain (ingest, storage schema, maintenance).
  `mallard-*` never writes into their schemas directly.
- One `duckdb` crate version for the workspace (it is bundled and linked
  once): only `mallard-duck` depends on it; everything else uses
  `mallard_duck::duckdb`.
- Library crates have no global side effects; the binaries set up logging,
  metrics and the runtime (with `mallard-common`).
- One config file, TOML, sections `[server]`, `[storage]`, `[duckdb]`
  (mallard's), `[sentry]`, `[otel]` (only what each side owns). One loader
  (`mallard_common::config`): env overrides `MALLARD_<SECTION>__<KEY>`
  (text settings stay text), `${NAME}` inside parsed strings, unknown keys
  are errors.
- Both query APIs require a bearer token (`server.api_tokens`) and answer
  errors as `{"error": code, "message": text}`.
- Schemas are fixed: `sentry`, `otel`; otel owns catalog-wide maintenance.
- Correlation lives in `mallard-client`, built on the two library APIs
  (`/sentry/v1/events?trace_id=`, `/otel/api/v1/traces/{id}`,
  `/otel/api/v1/logs?trace_id=`). The server adds no API of its own, so it
  never depends on the libraries' table layout.
- Trace lists and waterfalls mark Sentry errors without a call per trace:
  `odl-cli` has an optional `ErrorLookup` hook (it never depends on
  `sdl-*`); the `mallard` host answers it with one
  `/sentry/v1/events/by-trace?trace_id=a,b,…` call per list.
- Errors and traces correlate on `trace_id` (Sentry `contexts.trace.trace_id`
  = OTel span `trace_id`).
