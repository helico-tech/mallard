# sentry-ducklake

Sentry-compatible error tracking server. Sentry SDKs send errors to it by
changing only the DSN. Data lives in DuckLake (DuckDB 2.0 + Parquet on S3 /
GCS / Azure / local disk). Rust workspace; HTTP API with OpenAPI; CLI + TUI.

## Build and test
- `cargo build --workspace` / `cargo test --workspace` (DuckDB compiles from
  source the first time: ~2 min).
- JS end-to-end tests (real Sentry SDKs, browser, sourcemaps): see `e2e/README.md`.
- Run locally: `SDL_API_TOKEN=dev SDL_STORAGE_URL=./.data/storage cargo run -p sdl-server`.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass.

## Map
- `crates/sdl-protocol` - pure Sentry protocol: envelopes, auth, normalization,
  grouping, sourcemap symbolication. No I/O.
- `crates/sdl-server` - library + `sentry-ducklake` binary: ingest, writer,
  DuckLake schema, `/v1` API, sentry-cli sourcemap upload, storage.
- `crates/sdl-types` - `/v1` API types (shared by server, client, CLI).
- `crates/sdl-client` - typed async client.
- `crates/sdl-cli` - `sdl` CLI and `sdl tui`.
- `crates/sdl-loadgen` - load generator for capacity tests.
- `e2e/` - JS end-to-end tests with real SDKs.
- `deploy/` - Docker, docker compose, Helm chart.

## Docs
- [docs/architecture.md](docs/architecture.md) - parts and how they connect.
- [docs/data-model.md](docs/data-model.md) - DuckLake tables and why.
- [docs/adr/](docs/adr/) - decisions.
- [docs/deployment.md](docs/deployment.md), [docs/compatibility.md](docs/compatibility.md),
  [docs/sourcemaps.md](docs/sourcemaps.md), [docs/capacity.md](docs/capacity.md)
- [docs/references/sentry-sourcemap-protocol.md](docs/references/sentry-sourcemap-protocol.md)

## Work
- [work/queue.md](work/queue.md) - what to pick up next.
- Epics in `work/epics/`, issues in `work/issues/` (see global conventions).

## Rules
- duckdb crate is pinned `=1.20000.1` (DuckDB 2.0.0); a sibling project
  (obs-ducklake) embeds this crate and must use the identical version.
- All timestamps are UTC `TIMESTAMP` (bundled DuckDB has no ICU).
- Never write to DuckLake outside the writer thread (`db/writer.rs`).
- Tables are append-only; mutable state is an append-only log (latest wins).
