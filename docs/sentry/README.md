# sentry-ducklake

> **Retired binary.** This page describes the standalone `sentry-ducklake`
> server and `sdl` CLI, which are gone (story 0002/06). The code lives on
> as the `sdl-*` libraries inside mallard: run `mallard-server` (Sentry under
> `/sentry`, DSNs `<public_url>/sentry/<id>`) and the `mallard` CLI; see the
> [repo README](../../README.md). Rewrite: [issue 0013](../../work/issues/0013-per-side-docs-for-mallard.md).

Self-hosted error tracking that speaks the Sentry protocol and stores
everything in [DuckLake](https://ducklake.select): Parquet files on S3 (or
GCS, Azure, local disk) plus a small SQL catalog. Keep your Sentry SDKs, swap
the DSN, and get grouped issues, stack traces with sourcemaps, tags and
trends, through an HTTP API, a CLI and a terminal UI.

```js
Sentry.init({ dsn: "https://3f1c...@errors.example.com/1" });   // that's the only change
```

- **Drop-in for Sentry SDKs**: envelope and store endpoints, all auth styles,
  gzip/br/zstd, CORS. Verified with `@sentry/node`, `@sentry/browser` (in
  Chromium), the Rust SDK, `sentry-cli` 2.x/3.x and `@sentry/vite-plugin`.
- **Sourcemaps**: upload with `sentry-cli` or the bundler plugins as usual;
  frames are symbolicated at ingest (debug ids or releases).
- **Cheap, boring storage**: one bucket, compressed Parquet (~0.5 KB per
  event), daily partitions, retention by dropping days. Catalog in a DuckDB
  file or PostgreSQL.
- **One small binary**: thousands of events per second on one core-ish
  process ([capacity.md](docs/capacity.md)); Docker image and Helm chart.
- **Queryable**: OpenAPI-documented `/v1` API, `sdl` CLI, `sdl tui`, and plain
  SQL on the lake (join errors with traces by `trace_id`).

## Quick start

```sh
# 1. Run it (local disk storage; use s3://bucket/prefix in production)
docker run -p 8000:8000 -e SDL_API_TOKEN=secret -e SDL_STORAGE_URL=/data/storage \
  -v sdl-data:/data sentry-ducklake:latest
# or: SDL_API_TOKEN=secret SDL_STORAGE_URL=./.data/storage cargo run --release -p sdl-server

# 2. Create a project and get its DSN
export SDL_URL=http://localhost:8000 SDL_TOKEN=secret
cargo run -p sdl-cli -- projects create web --platform javascript

# 3. Put the DSN in Sentry.init, or try it right away:
cargo run -p sdl-cli -- send-test --dsn "http://<key>@localhost:8000/1"

# 4. Look at it
cargo run -p sdl-cli -- issues list -p web
cargo run -p sdl-cli -- tui
open http://localhost:8000/docs        # API reference (Scalar)
```

A full local stack with S3 (RustFS) and a PostgreSQL catalog:
`docker compose -f deploy/compose/docker-compose.yml up`.

## How it works

```
SDKs ──envelope──> ingest ──> normalize ─> symbolicate ─> group ─> queue
                                                                     │
                       one writer thread: group commit into DuckLake <┘
                       (events partitioned by day, sorted by issue)
   /v1 API, CLI, TUI <── reader pool <── DuckLake (catalog + Parquet on S3)
```

Details: [architecture](docs/architecture.md) · [data model](docs/data-model.md)
· [decisions](docs/adr/).

## Documentation

| Topic | |
|---|---|
| Deploy (Docker, compose, Helm, storage backends, catalog, sizing) | [docs/deployment.md](docs/deployment.md) |
| What of the Sentry protocol is supported | [docs/compatibility.md](docs/compatibility.md) |
| Sourcemaps with sentry-cli / Vite plugin | [docs/sourcemaps.md](docs/sourcemaps.md) |
| Load test results and capacity | [docs/capacity.md](docs/capacity.md) |
| CLI and TUI | [crates/sdl-cli/README.md](crates/sdl-cli/README.md) |
| API reference | `GET /docs` (Scalar) or `GET /openapi.json` on a running server |
| Embedding the server as a library | [docs/architecture.md#embedding](docs/architecture.md#embedding) |
| Tests with real SDKs | [e2e/README.md](e2e/README.md) |

## Development

```sh
cargo test --workspace                       # unit + integration (Rust SDK)
cargo clippy --workspace --all-targets -- -D warnings
cd e2e && npm ci && npx playwright install chromium && npm test   # real JS SDKs
cargo run --release -p sdl-loadgen -- --token secret ingest --rate 500 --duration 60s
```

The first build compiles DuckDB from source (a few minutes).

## Status and limits

Built for small and mid-size teams: one server process per catalog (scale up,
not out; see [work/issues/0001](work/issues/0001-multi-replica-caches.md)).
Errors only: transactions, sessions, replays and attachments are accepted
and dropped. No alerting, no user accounts (one API token), no PII scrubbing.

License: MIT OR Apache-2.0 (see the repository root).
