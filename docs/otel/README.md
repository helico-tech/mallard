# mallard: the telemetry side

mallard receives OTLP (gRPC and HTTP) and stores traces, logs and metrics in the
shared DuckLake catalog (schema `otel`): compact, sorted Parquet files plus a SQL
catalog, on local disk or object storage. You read them with the `mallard` CLI,
`mallard tui` or the HTTP API under `/otel`.

mallard is one binary and one server; this folder covers the telemetry half. Start
with the [repo README](../../README.md) and [architecture](../architecture.md).

```
$ mallard services -s 15m
╭─────────────────┬───────┬─────────┬────────┬─────────┬─────────┬─────────╮
│ service         ┆ spans ┆ rate    ┆ errors ┆ p50     ┆ p95     ┆ p99     │
╞═════════════════╪═══════╪═════════╪════════╪═════════╪═════════╪═════════╡
│ gateway         ┆  1619 ┆  1.80/s ┆  1.85% ┆  17.5ms ┆ 170.3ms ┆ 282.6ms │
│ frontend        ┆  1210 ┆  1.34/s ┆  1.98% ┆  32.6ms ┆ 208.5ms ┆ 315.9ms │
│ checkout        ┆   113 ┆ 0.126/s ┆  4.42% ┆ 191.7ms ┆ 339.4ms ┆   1.38s │
╰─────────────────┴───────┴─────────┴────────┴─────────┴─────────┴─────────╯
```

## What it does

- OTLP/gRPC (`:4317`) and OTLP/HTTP (`:4318`, and `/v1/*` on `:8080`; protobuf and
  JSON, gzip/deflate/zstd) for traces, logs and metrics (gauge, sum, histogram,
  exponential histogram, summary).
- Query API under `/otel` with OpenAPI 3.1 ([api.md](api.md), `/otel/docs`):
  - RED stats per service and operation
  - trace search and trace waterfalls
  - log search and live tail
  - metric catalog and time series: counter rates with reset detection, histogram
    percentiles, group-by
  - optional guarded read-only SQL
- Back-pressure (HTTP 429 / gRPC `UNAVAILABLE`) instead of dropping data or running
  out of memory. Automatic compaction, retention and snapshot expiry.
- A bearer token for the query API (required) and optionally for ingest.
- Open data: Parquet plus a DuckLake catalog. Any DuckDB of the same version can open
  it ([data-model.md](data-model.md#opening-the-lake-without-the-server)).
- The bridge (ADR 0009) links the sides: exceptions recorded by OTel SDKs become
  Sentry issues, in a project named after the service, and Sentry transactions become
  spans. Set in `[bridge]`.

Measured numbers are in [capacity.md](capacity.md): about 80,000 spans/s on 2 CPUs,
about 30 bytes per span on disk, last-hour queries in 5-50 ms on 68 M spans.

## Quick start

With Docker (local use only: the compose defaults are development credentials):

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
docker compose -f deploy/compose/docker-compose.yml exec mallard mallard --token dev-token status
```

From source (Rust 1.95+; the first build compiles DuckDB and takes ~2 min):

```sh
cargo build --release
MALLARD_SERVER__API_TOKENS=dev ./target/release/mallard serve &    # data in ./data/
./target/release/mallard-otel-loadgen --rate 20 --duration 0 &     # demo traffic
MALLARD_TOKEN=dev ./target/release/mallard tui
```

Send your own telemetry:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
export OTEL_SERVICE_NAME=my-service
```

Then explore (with `MALLARD_TOKEN` set):

```sh
mallard services                       # who is slow, who fails
mallard traces list -S checkout -e     # failing checkout traces
mallard trace <trace-id>               # spans, Sentry errors and logs of one trace
mallard logs -l error -f               # follow errors
mallard metrics show http.server.request.duration -g p99 -b http.route
curl localhost:8080/otel/openapi.json  # the API
```

## Documentation

| | |
|---|---|
| [Architecture](architecture.md) | components, write path, failure behaviour |
| [Data model](data-model.md) | tables, types, partitioning, sort order, and why (with measurements) |
| [Configuration](configuration.md) | every `[otel.*]` key, the shared sections |
| [Storage backends](storage-backends.md) | DuckDB/SQLite/PostgreSQL catalogs; local, S3, GCS, Azure, R2 |
| [Deployment](deployment.md) | pointing SDKs at it, Collector in front, TLS |
| [Capacity](capacity.md) | measured ingest, storage and query numbers; sizing guide |
| [Operations](operations.md) | monitoring, retention, backups, upgrades, troubleshooting |
| [Query API](api.md) | endpoints and semantics ([openapi.json](openapi.json)) |
| [CLI and TUI](cli.md) | what [docs/cli.md](../cli.md) leaves out for this side |
| [Library use](embedding.md) | how `mallard-server` and `mallard` use these crates |
| [Decisions](adr/) | from before the merge: DuckLake, data model, durability, compaction, SQL endpoint; newer ones in [docs/adr/](../adr/) |

## Status

Known limits: attribute values are stored as strings; in-memory buffering may lose
up to `otel.ingest.flush_interval` (5 s) of data on a hard crash. Put an OpenTelemetry
Collector with a persistent queue in front if you need zero loss
([recipe](deployment.md#with-an-opentelemetry-collector-in-front-recommended-for-zero-loss),
[ADR 0003](adr/0003-ingest-durability.md)).
