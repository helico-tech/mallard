# Architecture

## Parts

```
                ┌──────────────────────── obs-ducklake (one process) ───────────────────────┐
 Sentry SDKs ──▶│ :8080 HTTP  /api/{project}/envelope|store   → sdl-server router            │
 OTel SDKs   ──▶│ :4318 HTTP  /v1/traces|logs|metrics         → odl-server OTLP/HTTP router   │
             ──▶│ :4317 gRPC  OTLP                            → odl-server gRPC services      │
 obs CLI/TUI ──▶│ :8080 HTTP  /sentry/...  sentry management API (nested)                    │
                │             /otel/...    otel query API (nested)                           │
                │             /api/v1/correlate/...  obs-only: joins across both             │
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
| `obs-server` | bin `obs-ducklake` | Loads config, builds both library servers, mounts their routers on shared listeners, runs their background jobs, owns tracing, metrics and shutdown. Adds the correlation API. |
| `obs-client` | lib | Typed async client: wraps `sdl-client`, `odl-client` and the correlation API behind one base URL + token. |
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
- Errors and traces correlate on `trace_id` (Sentry `contexts.trace.trace_id`
  = OTel span `trace_id`).
