# Architecture

## Parts

```
                ┌──────────────────────── obs-ducklake (one process) ───────────────────────┐
 Sentry SDKs ──▶│ :8080 HTTP  /sentry/api/{project}/envelope  → sdl-server router            │
 OTel SDKs   ──▶│ :4318 HTTP  /v1/traces|logs|metrics         → odl-server OTLP/HTTP router   │
             ──▶│ :4317 gRPC  OTLP                            → odl-server gRPC services      │
 obs CLI/TUI ──▶│ :8080 HTTP  /sentry/v1/... sentry API, /sentry/api/0 sentry-cli            │
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

| crate | kind | role |
|---|---|---|
| `obs-server` | lib + bin `obs-ducklake` | Loads config, opens the shared catalog (`odl_server::store::open_connection`), builds both library servers, mounts their routers, runs their background jobs, owns logging, metrics and shutdown. |
| `obs-client` | lib | Typed async client: wraps `sdl-client` (`<url>/sentry`) and `odl-client` (`<url>/otel`) behind one base URL + token, and does correlation (trace → errors + spans + logs) by calling both. |
| `obs-cli` | bin `obs` | CLI for errors, traces, logs, metrics, correlation. `obs tui` starts the TUI (ratatui). |

## Key rules

- The libraries own their domain (ingest, storage schema, maintenance).
  This repo never writes into their schemas directly; it only reads across
  them for correlation.
- Both libraries must use the same `duckdb` crate version (it is bundled
  and linked once).
- Library code has no global side effects; `obs-server` sets up logging,
  metrics and the runtime.
- One config file, TOML, sections `[server]`, `[storage]` (shared),
  `[sentry]`, `[otel]`. Env overrides: `OBS_<SECTION>__<KEY>`.
- Correlation lives in `obs-client`, built on the two library APIs
  (`/sentry/v1/events?trace_id=`, `/otel/api/v1/traces/{id}`,
  `/otel/api/v1/logs?trace_id=`). The server adds no API of its own, so it
  never depends on the libraries' table layout.
- Errors and traces correlate on `trace_id` (Sentry `contexts.trace.trace_id`
  = OTel span `trace_id`).
