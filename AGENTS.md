# AGENTS.md

otel-ducklake: an OpenTelemetry (OTLP) collector in Rust that stores traces,
logs and metrics in DuckLake (DuckDB 2.0 / DuckLake 1.1) and serves them
through an OpenAPI query API, a CLI and a TUI.

## Map

- `crates/odl-api`: API types (source of the OpenAPI schema)
- `crates/odl-server`: receivers, storage, maintenance, query API; bin `otel-ducklake`
- `crates/odl-client`: typed HTTP client (+ `LogTail`)
- `crates/odl-cli`: bin `odl` (CLI + `odl tui`)
- `crates/odl-loadgen`: load generator
- `config/`: example config; `deploy/`: compose files; `charts/`: Helm chart
- `bench/`: reproducible experiments and capacity runs
- `docs/`: [architecture](docs/architecture.md), [data model](docs/data-model.md),
  [configuration](docs/configuration.md), [API](docs/api.md), [ADRs](docs/adr/)
- `work/`: [queue](work/queue.md), epics, issues

## Build and test

```sh
cargo build --release                 # first build compiles DuckDB (~2 min)
cargo test --workspace                # unit + e2e (real DuckLake in temp dirs)
cargo clippy --workspace -- -D warnings
cargo fmt --all
cargo run -p odl-server -- openapi > docs/openapi.json   # after API changes (a test checks it)
```

## Rules

- DuckDB is pinned (`duckdb = "=1.20000.1"`). `libduckdb-sys` must build
  without DEBUG (see `Cargo.toml` profile): debug builds abort on prebuilt
  extensions.
- DuckDB 2.0 lambdas use `lambda x: expr` (the `->` form is removed).
- Table columns are append-only; column order in `store/schema.rs` must match
  `otlp/convert.rs`.
- User input reaches SQL only as bound parameters.
- Public library surface (`Server`, `store::open_connection`, `odl_client`,
  `odl_cli::{Cmd, Ctx, run}`, `tui::Embedded`) is used by obs-ducklake. Don't
  break it silently.
- Work tracking: `work/queue.md` → stories/issues; decisions → `docs/adr/`.
