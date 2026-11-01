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
                │             /healthz /readyz /metrics                                       │
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
| `mallard` | The one binary. `serve` runs the server; `status`, `trace`, `errors …` (= `sdl-cli` commands), `telemetry …` (= `odl-cli` commands) and `tui` talk to a server over HTTP. |
| `mallard-server` | Loads config, opens the shared catalog (`odl_server::store::open_connection`), builds both sides on it, mounts their routers, runs their background jobs, owns logging, metrics and shutdown. |
| `mallard-client` | Wraps `sdl-client` (`<url>/sentry`) and `odl-client` (`<url>/otel`) behind one base URL + token; trace correlation (spans + errors + logs). |
| `sdl-*` | Sentry side. See [sentry/architecture.md](sentry/architecture.md). |
| `odl-*` | OpenTelemetry side. See [otel/architecture.md](otel/architecture.md). |

## Key rules

- Each side owns its domain (ingest, storage schema, maintenance).
  `mallard-*` never writes into their schemas directly.
- One `duckdb` crate version for the workspace (it is bundled and linked
  once).
- Library crates have no global side effects; the `mallard` binary sets up
  logging, metrics and the runtime.
- One config file, TOML, sections `[server]`, `[storage]` (shared),
  `[sentry]`, `[otel]`. Env overrides: `MALLARD_<SECTION>__<KEY>`.
- Correlation lives in `mallard-client`, built on the two library APIs
  (`/sentry/v1/events?trace_id=`, `/otel/api/v1/traces/{id}`,
  `/otel/api/v1/logs?trace_id=`). The server adds no API of its own, so it
  never depends on the libraries' table layout.
- Errors and traces correlate on `trace_id` (Sentry `contexts.trace.trace_id`
  = OTel span `trace_id`).
