# obs-ducklake

Errors and telemetry in one place, on one DuckLake (DuckDB catalog + Parquet
files on disk or object storage):

- **Errors**: send from any Sentry SDK (built on [sentry-ducklake](../sentry-ducklake)).
- **Traces, logs, metrics**: send over OTLP from any OpenTelemetry SDK or
  Collector (built on [otel-ducklake](../otel-ducklake)).
- **Together**: errors and spans share the trace id, so `obs trace <id>`
  shows the span tree with each error and log under the span it happened in.

One server binary (`obs-ducklake`), one CLI and TUI (`obs`).

## Quick start (Docker)

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
export OBS_URL=http://localhost:8080 OBS_TOKEN=dev-token

# Errors: create a project and use the DSN it returns in your Sentry SDK.
curl -s -XPOST -H "Authorization: Bearer $OBS_TOKEN" -H 'content-type: application/json' \
     $OBS_URL/sentry/v1/projects -d '{"name":"web"}'

# Traces/logs/metrics: point OpenTelemetry at the server.
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
```

The stack runs obs-ducklake with a Postgres catalog and an S3 bucket
(RustFS). Build the image from this repo's root with the two library repos
next to it (`../sentry-ducklake`, `../otel-ducklake`).

## Quick start (local)

```sh
cargo run -p obs-server --release -- -c config/obs-ducklake.example.toml   # data in ./data
cargo run -p obs-cli --release -- status
```

The Sentry API needs a token: set `server.api_tokens` in the config or
`OBS_SERVER__API_TOKENS='["secret"]'`.

## Kubernetes

```sh
helm install obs deploy/helm/obs-ducklake \
  --set publicUrl=https://obs.example.com --set apiToken=secret \
  --set storage.dataPath=s3://my-bucket/obs/lake/
```

See `deploy/helm/obs-ducklake/values.yaml` and the examples in its `ci/` folder
(DuckDB file on a PVC, MinIO + Postgres, AWS IRSA + Postgres).

## Ports and paths

| where | what |
|---|---|
| `:8080 /sentry/...` | Sentry ingest (DSN `http://KEY@host:8080/sentry/1`), Sentry API `/sentry/v1`, docs `/sentry/docs`, sentry-cli (`--url http://host:8080/sentry`) |
| `:8080 /otel/...` | otel query API `/otel/api/v1`, docs `/otel/docs` |
| `:8080 /v1/traces\|logs\|metrics`, `:4318` | OTLP/HTTP |
| `:4317` | OTLP/gRPC |
| `:8080 /healthz /readyz /metrics` | health, Prometheus metrics |

## CLI

```sh
obs status                     # both sides
obs trace <trace-id>           # spans + Sentry errors + logs
obs otel traces|logs|services|metric ...   # every otel-ducklake command
obs sentry issues|events|projects ...      # every sentry-ducklake command
obs tui                        # terminal UI: F1 errors, F2 telemetry
```

## Configuration

One TOML file, see `config/obs-ducklake.example.toml`. `[server]`,
`[storage]` and `[duckdb]` are shared; `[otel]` and `[sentry]` hold each
library's own settings. Any key can be overridden with
`OBS_<SECTION>__<KEY>`, e.g. `OBS_OTEL__RETENTION__TRACES=3d`.

More: `docs/architecture.md`, `docs/adr/`.
