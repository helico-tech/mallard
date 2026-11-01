# mallard: the Sentry side

Sentry SDKs send errors to mallard when you change only the DSN. mallard groups
them into issues, with stack traces, sourcemaps, tags and trends, and stores them
in the shared DuckLake catalog (schema `sentry`). You read them with the `mallard`
CLI, `mallard tui` or the HTTP API under `/sentry`.

```js
Sentry.init({ dsn: "https://<public_key>@mallard.example.com/sentry/1" });
```

mallard is one binary and one server; this folder covers the Sentry half. Start
with the [repo README](../../README.md) and [architecture](../architecture.md).

- **SDKs:** envelope and store endpoints, all auth styles, gzip/deflate/br/zstd,
  CORS. Tested with `@sentry/node`, `@sentry/browser` (in Chromium), the Rust SDK,
  `sentry-cli` 2.x and 3.x, and `@sentry/vite-plugin`.
- **Sourcemaps:** upload with `sentry-cli` or the bundler plugins. Frames are
  symbolicated at ingest, by debug id or by release.
- **Storage:** Parquet with daily partitions, a few hundred bytes per event
  ([capacity.md](capacity.md)). Retention drops whole days.
- **Query:** `/sentry/v1` API with OpenAPI, the CLI and TUI, and plain SQL on the
  lake (errors join traces on `trace_id`).

## Quick start

```sh
cargo build -p mallard                  # first build compiles DuckDB (~2 min)
alias mallard=$PWD/target/debug/mallard

# 1. Run it. Data goes to ./data/; use s3://... in production.
MALLARD_SERVER__API_TOKENS=secret MALLARD_STORAGE__ARTIFACTS_PATH=data/artifacts mallard serve &

# 2. Create a project. The answer holds its DSN.
export MALLARD_URL=http://localhost:8080 MALLARD_TOKEN=secret
mallard projects create web --platform javascript

# 3. Put the DSN in Sentry.init, or send a test error.
mallard send-test --dsn "http://<key>@localhost:8080/sentry/1"

# 4. Look at it.
mallard issues list -p web
mallard tui
open http://localhost:8080/sentry/docs  # API reference
```

A full local stack with S3 (RustFS) and a PostgreSQL catalog:
`docker compose -f deploy/compose/docker-compose.yml up -d --build`.

## How it works

```
SDKs ──envelope──> ingest ──> normalize ─> symbolicate ─> group ─> queue
                                                                     │
                       one writer thread: group commit into DuckLake <┘
                       (events partitioned by day, sorted by issue)
   /sentry/v1 API, CLI, TUI <── reader pool <── DuckLake (catalog + Parquet)
```

## Documentation

| Topic | |
|---|---|
| Inside the Sentry side | [architecture.md](architecture.md) |
| Tables and why | [data-model.md](data-model.md) |
| Docker, compose, storage, catalog, replicas, backups | [../deployment.md](../deployment.md) |
| What of the Sentry protocol is supported | [compatibility.md](compatibility.md) |
| Sourcemaps with `sentry-cli` or the Vite plugin | [sourcemaps.md](sourcemaps.md) |
| Load test results | [capacity.md](capacity.md) |
| CLI and TUI | [docs/cli.md](../cli.md), [mallard-sentry-cli](../../crates/mallard-sentry-cli/README.md) |
| API reference | `GET /sentry/docs` or `GET /sentry/openapi.json`, also [openapi.json](openapi.json) |
| Decisions | [adr/](adr/) (from before the merge), [docs/adr/](../adr/) |
| Protocol notes | [references/sentry-sourcemap-protocol.md](references/sentry-sourcemap-protocol.md) |
| Tests with real SDKs | [e2e/sentry/README.md](../../e2e/sentry/README.md) |

## Status and limits

- Stored: errors and messages. Sentry transactions and span items become OTel
  spans through the bridge (ADR 0009, 0013, 0022). Sessions, replays, profiles and
  attachments are accepted and dropped ([compatibility.md](compatibility.md)).
- Several processes can share a PostgreSQL catalog
  ([deployment.md](../deployment.md#running-more-than-one-process)).
- Not there: alerts, user accounts (only API tokens), PII scrubbing.

The commands for tests, lint and the load generator are in the repo
[AGENTS.md](../../AGENTS.md).
