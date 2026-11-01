# mallard

One observability app: Sentry-compatible error tracking plus an
OpenTelemetry collector (traces, logs, metrics), stored together in one
DuckLake catalog (DuckDB 2.0 + Parquet on disk or object storage). One
binary, `mallard`: server, CLI and TUI.

## Build and test

```sh
cargo build --workspace               # first build compiles DuckDB (~2 min)
cargo test --workspace                # unit + integration (real DuckLake in temp dirs)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p mallard -- serve -c config/mallard.example.toml
cargo run -p mallard -- --help        # status, trace, errors, telemetry, tui
docker compose -f deploy/compose/docker-compose.yml up -d --build
```

JS end-to-end tests with real Sentry SDKs: `e2e/sentry/README.md`.
After otel API changes: `cargo run -p odl-server -- openapi > docs/otel/openapi.json`
(a test checks it).

## Map

| path | what |
|---|---|
| `crates/mallard` | the `mallard` binary: `serve`, `status`, `trace`, `errors …`, `telemetry …`, `tui` |
| `crates/mallard-server` | embeds both sides on one DuckLake connection; config, routes, shutdown |
| `crates/mallard-client` | one client for both APIs + trace correlation |
| `crates/sdl-*` | Sentry side: `sdl-protocol` (pure protocol), `sdl-server` (ingest, writer, `/v1` API), `sdl-types`, `sdl-client`, `sdl-cli` (commands + TUI component), `sdl-loadgen` |
| `crates/odl-*` | OpenTelemetry side: `odl-server` (OTLP receivers, storage, maintenance, query API), `odl-api`, `odl-client`, `odl-cli` (commands + TUI component), `odl-loadgen` |
| `config/` | example configs |
| `deploy/` | Dockerfile, compose (RustFS + Postgres), Helm chart |
| `e2e/sentry/` | JS e2e with real Sentry SDKs, browser and sourcemaps |
| `bench/otel/` | reproducible experiments and capacity runs |
| `docs/` | [architecture](docs/architecture.md), [ADRs](docs/adr/); per side: [docs/sentry/](docs/sentry/), [docs/otel/](docs/otel/) |
| `work/` | [queue](work/queue.md) → epics and issues (also `work/sentry/`, `work/otel/`) |

`sdl-server` and `odl-server` still build their standalone binaries
(`sentry-ducklake`, `otel-ducklake`); the e2e suite and load tests use them.
The product is `mallard`.

## Rules

- `duckdb = "=1.20000.1"` (DuckDB 2.0.0), one version for the whole
  workspace. `libduckdb-sys` must build without DEBUG (see the profile in
  `Cargo.toml`): debug builds abort on the prebuilt extensions.
- One DuckDB connection is shared: catalog `lake`, schemas `sentry` and
  `otel` (ADR 0001). Libraries never change global DuckDB settings on a
  shared connection; `mallard-server` owns `[duckdb]`.
- All timestamps are UTC `TIMESTAMP` (bundled DuckDB has no ICU).
- Tables are append-only. Sentry: write only on the writer thread
  (`sdl-server/src/db/writer.rs`); mutable state is an append-only log.
  Otel: column order in `store/schema.rs` must match `otlp/convert.rs`.
- DuckDB 2.0 lambdas use `lambda x: expr` (the `->` form is removed).
- User input reaches SQL only as bound parameters.
- Errors and traces join on `trace_id` (UUID) and `span_id` (UBIGINT).

## Work

`work/queue.md` is the single queue (top = next). Epics in `work/epics/`,
issues in `work/issues/`. `work/sentry/` and `work/otel/` hold the history
of the two sides from before the merge; new items go in `work/`.
Decisions: `docs/adr/` (the side-specific ADRs stay in `docs/*/adr/`).
