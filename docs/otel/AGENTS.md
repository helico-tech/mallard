# AGENTS.md

> **Retired binary.** Written for the old standalone repo; its
> binaries are gone (story 0002/06). The repo map is [AGENTS.md](../../AGENTS.md).

The telemetry side of mallard: an OpenTelemetry (OTLP) collector in Rust that stores traces,
logs and metrics in DuckLake (DuckDB 2.0 / DuckLake 1.1) and serves them
through an OpenAPI query API, a CLI and a TUI.

## Map

- `crates/mallard-otel-api`: API types (source of the OpenAPI schema)
- `crates/mallard-otel-server`: receivers, storage, maintenance, query API
- `crates/mallard-otel-client`: typed HTTP client (+ `LogTail`)
- `crates/mallard-otel-cli`: CLI commands and TUI
- `crates/mallard-otel-loadgen`: load generator
- `config/`: example config; `deploy/`: compose files; `charts/`: Helm chart
- `bench/`: reproducible experiments and capacity runs
- `docs/`: [architecture](docs/architecture.md), [data model](docs/data-model.md),
  [configuration](docs/configuration.md), [API](docs/api.md),
  [deployment](docs/deployment.md), [operations](docs/operations.md),
  [capacity](docs/capacity.md), [ADRs](docs/adr/)
- `work/`: [queue](work/queue.md), epics, issues

## Build and test

```sh
cargo build --release                 # first build compiles DuckDB (~2 min)
cargo test --workspace                # unit + e2e (real DuckLake in temp dirs)
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all
UPDATE_OPENAPI=1 cargo test -p mallard-otel-server --test openapi   # after API changes (a test checks it)
```

## Rules

- DuckDB is pinned (`duckdb = "=1.20000.1"`). `libduckdb-sys` must build
  without DEBUG (see `Cargo.toml` profile): debug builds abort on prebuilt
  extensions.
- DuckDB 2.0 lambdas use `lambda x: expr` (the `->` form is removed).
- Table columns are append-only; column order in `store/schema.rs` must match
  `otlp/convert.rs`.
- User input reaches SQL only as bound parameters.
- Public library surface (`Server`, `mallard_otel_client`, `mallard_otel_cli::{Command, Ctx,
  run, Embedded}`) is used by mallard. Don't break it silently. DuckDB set-up
  (`open_connection`, extensions, secrets) lives in `crates/mallard-duck`.
- Work tracking: `work/queue.md` → stories/issues; decisions → `docs/adr/`.
