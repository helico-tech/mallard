# mallard

Errors, traces, logs and metrics in one place, stored in a
[DuckLake](https://ducklake.select) (DuckDB catalog + Parquet files on disk
or object storage). One binary: `mallard serve` runs the server; the `mallard`
CLI and terminal UI talk to it over HTTP. Need only the client? A slim build
has the same CLI and TUI with no server and no DuckDB.

- **Errors**: point any Sentry SDK at mallard by changing only the DSN.
  Issues, grouping, releases, sourcemaps (`sentry-cli` uploads work).
- **Traces, logs, metrics**: send OTLP (HTTP or gRPC) from any
  OpenTelemetry SDK or Collector.
- **Together**: errors and spans share the trace id. `mallard trace <id>`
  shows the span tree with each error and log under the span it happened
  in; in the TUI, `x` jumps from an error to its trace and back.
- **Either SDK is enough**: Sentry transactions and streamed spans are stored as traces, and
  exceptions recorded by OpenTelemetry SDKs become Sentry issues (with
  parsed stack traces for Python, Java, JS, Go, .NET and Rust). An error
  sent through both SDKs is stored once. Switches: `[bridge]` in the
  config ([ADR 0009](docs/adr/0009-bridge.md)).
- **Cheap to run**: one process, Parquet on S3 (or local disk), catalog in
  a DuckDB file or Postgres. No Kafka, ClickHouse or Elasticsearch.

## The terminal UI

`mallard tui`: errors on F1, telemetry on F2, and `x` jumps between them by
trace id.

**An issue, with the trace of the request that failed** — open an issue,
press `x`, and the telemetry tab shows that request's spans with the failing
one in red:

<p>
  <img src="docs/screenshots/errors-issue.png" alt="Issue detail: PaymentDeclinedError with tags, releases and the trace id" width="49%">
  <img src="docs/screenshots/telemetry-trace.png" alt="Waterfall of the trace: web, checkout-api, inventory, postgres, payments; the payments span failed" width="49%">
</p>

**Errors and telemetry at a glance**:

<p>
  <img src="docs/screenshots/errors-issues.png" alt="Errors tab: unresolved issues with event and user counts" width="49%">
  <img src="docs/screenshots/telemetry-overview.png" alt="Telemetry tab: request rate, error rate, p95 latency and per-service RED metrics" width="49%">
</p>

**Logs of the same request** (`l` on the trace):

<img src="docs/screenshots/telemetry-logs.png" alt="Logs filtered to the trace: placing order, card declined" width="49%">

Screenshots come from the real TUI and demo data (`scripts/screenshots.sh`).

## Quick start (Docker)

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
export MALLARD_URL=http://localhost:8080 MALLARD_TOKEN=dev-token

# Create a project; prints SENTRY_DSN and the OTEL_EXPORTER_OTLP_* env vars
# for your app (Sentry SDK + OpenTelemetry SDK). Safe to run again.
mallard setup web >> .env

mallard status
mallard tui
```

The compose stack runs mallard with a Postgres catalog and an S3 bucket
(RustFS). Its credentials are for local use only. The Web UI is on
http://localhost:8088: sign in with the API token (`dev-token`).

## Quick start (local)

```sh
cargo install --path crates/mallard
MALLARD_API_TOKEN=dev-token mallard serve -c config/mallard.example.toml
```

Data goes to `./data` by default. The first build compiles DuckDB and takes
a few minutes. For only the CLI and TUI, on a machine that talks to a server
elsewhere: `cargo install --path crates/mallard --no-default-features` (quick
to build, much smaller, no `serve`; [ADR 0023](docs/adr/0023-one-binary.md)).

## Production

mallard ships as a Docker image. There is no Helm chart for now
([ADR 0026](docs/adr/0026-no-helm-chart-for-now.md)), and nothing publishes the image yet,
so build it yourself:

```sh
docker build -f deploy/docker/Dockerfile -t mallard .
docker run -d --name mallard -p 8080:8080 -p 4318:4318 -p 4317:4317 -v mallard-data:/data \
  -e MALLARD_SERVER__API_TOKENS=change-me \
  -e MALLARD_SERVER__PUBLIC_URL=https://mallard.example.com \
  mallard
```

[`docs/deployment.md`](docs/deployment.md) covers object storage, a Postgres catalog,
health endpoints and running behind a proxy.

## Ports and paths

| where | what |
|---|---|
| `:8080 /sentry/...` | Sentry ingest (DSN `http://KEY@host:8080/sentry/1`), Sentry API `/sentry/v1`, docs `/sentry/docs`, bundler plugins (`url: http://host:8080/sentry`) |
| `:8080 /api/0/...` | sentry-cli's API (`--url http://host:8080`: the Rust `sentry-cli` only accepts a URL root; also under `/sentry/api/0`) |
| `:8080 /otel/...` | telemetry query API `/otel/api/v1`, docs `/otel/docs` |
| `:8080 /v1/traces\|logs\|metrics`, `:4318` | OTLP/HTTP |
| `:4317` | OTLP/gRPC |
| `:8080 /healthz /readyz /metrics` | health, Prometheus metrics |

## CLI

```sh
mallard serve -c mallard.toml             # run the server (or MALLARD_CONFIG)
mallard status                            # health of both sides (exit 6 when one is down)
mallard setup web                         # project + DSN + OTLP env vars
mallard issues list --since 1h            # errors: issues, events, projects
mallard find <id>                         # trace, event or issue id: what is it?
mallard trace <trace-id>                  # spans + errors + logs of one trace
mallard traces list --errors              # telemetry: traces, logs, services, ops, metrics, sql
mallard logs --around <time> -S checkout  # logs around an error
mallard tui                               # F1 errors, F2 telemetry
mallard issues list -n 20 --json --fields id,title,count
```

Client commands read `MALLARD_URL` and `MALLARD_TOKEN`. One flat command
tree with shared flags (`--format text|json|ndjson`, `--fields`, `--raw`,
`-s/--since`, `--from/--to`, `-n/--limit`, `-q`, `-t/--trace`, `--color`),
typed exit codes, JSON errors on stderr, plain text and compact JSON when
piped, times in UTC, and one set of TUI keys (`r` refresh, `t` time window,
`x` same trace on the other tab, `?` help). See [docs/cli.md](docs/cli.md).
**AI agents**: [docs/agents.md](docs/agents.md) (also `mallard help agents`).

## Configuration

One TOML file, see `config/mallard.example.toml` (every key with its
default). `[server]`, `[storage]` and `[duckdb]` are shared; `[otel]` and
`[sentry]` hold only each side's own settings. Unknown keys are errors. Any
key can be overridden with `MALLARD_<SECTION>__<KEY>`, e.g.
`MALLARD_OTEL__RETENTION__TRACES=0` or `MALLARD_SERVER__API_TOKENS=a,b`
(text settings keep the value as is; lists may be comma separated).
`${NAME}` inside a text value is replaced by environment variable `NAME`.
`mallard check -c mallard.toml --connect` validates a file and opens the
catalog.

## More

- [`docs/deployment.md`](docs/deployment.md) — how to deploy: Docker, compose, storage,
  catalog, backups.
- `docs/architecture.md` and `docs/adr/` — how it fits together.
- `docs/sentry/`, `docs/otel/` — each side in depth (data model, capacity,
  compatibility, API).
- `AGENTS.md` — map for contributors and coding agents.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this
project by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
