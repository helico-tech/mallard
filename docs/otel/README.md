# otel-ducklake

> **Retired binary.** This page describes the standalone `otel-ducklake`
> server and `odl` CLI, which are gone (story 0002/06). The code lives on as
> the `odl-*` libraries inside mallard: run `mallard serve` (query API under
> `/otel`, OTLP on :4318/:4317) and the `mallard` CLI; see the
> [repo README](../../README.md). Rewrite: [issue 0016](../../work/issues/0016-per-side-docs-for-mallard.md).

**An OpenTelemetry collector that stores traces, logs and metrics in
[DuckLake](https://ducklake.select), with a query API, a CLI and a terminal UI.**

One Rust binary receives OTLP (gRPC and HTTP). It writes compact, sorted
Parquet files plus a SQL catalog, on local disk or object storage. It answers
"what is slow, what is failing, and why" in milliseconds. It is built for
small and mid-sized companies: one 2-vCPU instance ingests ~80,000 spans per
second, far more than thousands to tens of thousands of visitors a day
produce.

```
$ odl services -s 15m
╭─────────────────┬───────┬─────────┬────────┬─────────┬─────────┬─────────╮
│ service         ┆ spans ┆ rate    ┆ errors ┆ p50     ┆ p95     ┆ p99     │
╞═════════════════╪═══════╪═════════╪════════╪═════════╪═════════╪═════════╡
│ gateway         ┆  1619 ┆  1.80/s ┆  1.85% ┆  17.5ms ┆ 170.3ms ┆ 282.6ms │
│ frontend        ┆  1210 ┆  1.34/s ┆  1.98% ┆  32.6ms ┆ 208.5ms ┆ 315.9ms │
│ checkout        ┆   113 ┆ 0.126/s ┆  4.42% ┆ 191.7ms ┆ 339.4ms ┆   1.38s │
╰─────────────────┴───────┴─────────┴────────┴─────────┴─────────┴─────────╯
```

## Why

- **Open data, no lock-in.** Everything is Parquet plus a DuckLake catalog.
  Point any DuckDB at it and write SQL.
- **Storage anywhere DuckLake goes.** The catalog can be a DuckDB file,
  SQLite or PostgreSQL. Data can live on local disk, S3, S3-compatible
  stores (MinIO, SeaweedFS, R2, ...), Google Cloud Storage or Azure.
- **Cheap.** About 30 bytes per span on disk; retention drops whole days
  without rewriting data. 1-2 vCPU and 2 GB RAM are enough.
- **Fast where it matters.** Last-hour queries take 5-50 ms on a 68 M-span
  lake with 2 threads ([capacity](docs/capacity.md)).
- **Simple to run.** A single binary or container, extensions baked in, with
  a Helm chart and compose files. No queue or cluster required.

## Features

- OTLP/gRPC (`:4317`) and OTLP/HTTP (`:4318`, protobuf and JSON, gzip/zstd)
  for traces, logs and metrics (gauge, sum, histogram, exponential histogram,
  summary).
- Query API (`:8080`) with OpenAPI 3.1 ([docs/api.md](docs/api.md),
  `/docs`):
  - RED stats per service and operation
  - trace search and trace waterfalls
  - log search and live tail
  - metric catalog and time series: counter rates with reset detection,
    histogram percentiles, group-by
  - optional guarded read-only SQL
- `odl` CLI: tables or JSON, `logs -f`, trace waterfalls in the terminal.
- `odl tui`: overview dashboard, trace search plus waterfall with span
  details and logs, live logs, metric charts, status.
- Back-pressure (HTTP 429 / gRPC `UNAVAILABLE`) instead of dropping data or
  running out of memory. Automatic compaction, retention and snapshot expiry.
- Bearer-token auth for ingest and query.
- Usable as Rust libraries: embed the server, client, CLI or TUI
  ([docs/embedding.md](docs/embedding.md)).

## Quick start

With Docker:

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
docker compose -f deploy/compose/docker-compose.yml --profile loadgen up -d   # demo traffic
docker compose -f deploy/compose/docker-compose.yml exec otel-ducklake odl services
```

From source (Rust 1.90+; the first build compiles DuckDB and takes ~2 min):

```sh
cargo build --release
./target/release/otel-ducklake serve &                 # data in ./data/
./target/release/odl-loadgen --rate 20 --duration 0 &  # demo traffic
./target/release/odl tui
```

Send your own telemetry:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
export OTEL_SERVICE_NAME=my-service
```

Then explore:

```sh
odl services                       # who is slow, who fails
odl traces -S checkout -e          # failing checkout traces
odl trace <trace-id> -A            # waterfall with attributes
odl logs -l error -f               # follow errors
odl metric http.server.request.duration -g p99 -b http.route
curl localhost:8080/openapi.json   # the API
```

## Documentation

| | |
|---|---|
| [Architecture](docs/architecture.md) | components, write path, failure behaviour |
| [Data model](docs/data-model.md) | tables, types, partitioning, sort order — and why (with measurements) |
| [Configuration](docs/configuration.md) | every setting, env overrides, secrets |
| [Storage backends](docs/storage-backends.md) | DuckDB/SQLite/PostgreSQL catalogs; local, S3, GCS, Azure, R2 |
| [Deployment](docs/deployment.md) | container image, compose, Helm, Collector in front, TLS |
| [Capacity](docs/capacity.md) | measured ingest, storage and query numbers; sizing guide |
| [Operations](docs/operations.md) | monitoring, retention, backups, upgrades, troubleshooting |
| [Query API](docs/api.md) | endpoints and semantics ([openapi.json](docs/openapi.json)) |
| [CLI and TUI](docs/cli.md) | commands and keys |
| [Embedding](docs/embedding.md) | using the crates as libraries |
| [Decisions](docs/adr/) | ADRs: DuckLake, data model, durability, compaction, SQL endpoint |

## Project layout

```
crates/odl-api       API types (OpenAPI schema source)
crates/odl-server    collector + query API (bin: otel-ducklake)
crates/odl-client    typed HTTP client
crates/odl-cli       CLI and TUI (bin: odl)
crates/odl-loadgen   load generator (bin: odl-loadgen)
deploy/ charts/      compose files and Helm chart
bench/               reproducible experiments
docs/ work/          documentation and work tracking
```

## Status

Version 0.1: feature-complete for its scope, tested end to end (unit,
integration with a real DuckLake, compose and Kubernetes runs), and reviewed.
Known limitations: attribute values are stored as strings; in-memory buffering
may lose up to `flush_interval` (5 s) of data on a hard crash. Put an
OpenTelemetry Collector with a persistent queue in front if you need zero loss
([recipe](deployment.md#with-an-opentelemetry-collector-in-front-recommended-for-zero-loss),
[ADR 0003](adr/0003-ingest-durability.md)).

## License

MIT OR Apache-2.0 (see the repository root).
