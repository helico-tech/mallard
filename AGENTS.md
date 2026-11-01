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
MALLARD_API_TOKEN=dev cargo run -p mallard-server -- -c config/mallard.example.toml
cargo run -p mallard -- --help        # status, trace, errors, telemetry, tui
docker compose -f deploy/compose/docker-compose.yml up -d --build
```

JS end-to-end tests with real Sentry SDKs: `e2e/sentry/README.md`.
After API changes: `cargo run -p odl-server -- openapi > docs/otel/openapi.json`
and `cargo run -p sdl-server -- openapi > docs/sentry/openapi.json` (tests check them).
Helm: `helm lint --strict deploy/helm/mallard -f deploy/helm/mallard/ci/<file>.yaml`;
the rendered `mallard.toml` must pass `mallard-server check -c <file>`.

## Map

| path | what |
|---|---|
| `crates/mallard` | the `mallard` binary: `serve`, `status`, `trace`, `errors …`, `telemetry …`, `tui` |
| `crates/mallard-server` | embeds both sides on one DuckLake connection; config, routes, readiness, shutdown |
| `crates/mallard-common` | shared by both sides: durations, sizes, windows, `ApiError`; feature `server`: config loader, tokens, HTTP helpers, logging, signals, Prometheus (ADR 0005) |
| `crates/mallard-duck` | the only `duckdb` dependency: storage/DuckDB config, `open_connection`, extensions, secrets, quoting, redaction |
| `crates/mallard-client` | one client for both APIs + trace correlation |
| `crates/mallard-term` | terminal output both sides share: `clean()`, tables, formats (UTC), colour, error lines, TUI theme/keys/`Outcome` (ADR 0006) |
| `crates/sdl-*` | Sentry side: `sdl-protocol` (pure protocol), `sdl-server` (ingest, writer, `/v1` API), `sdl-types`, `sdl-client`, `sdl-cli` (commands + TUI component), `sdl-loadgen` |
| `crates/odl-*` | OpenTelemetry side: `odl-server` (OTLP receivers, storage, maintenance, query API), `odl-api`, `odl-client`, `odl-cli` (commands + TUI component), `odl-loadgen` |
| `config/` | example configs |
| `deploy/` | Dockerfile, compose (RustFS + Postgres), Helm chart |
| `e2e/sentry/` | JS e2e with real Sentry SDKs, browser and sourcemaps |
| `bench/otel/` | reproducible experiments and capacity runs |
| `docs/` | [architecture](docs/architecture.md), [ADRs](docs/adr/), [CLI and TUI keys](docs/cli.md); per side: [docs/sentry/](docs/sentry/), [docs/otel/](docs/otel/) |
| `work/` | [queue](work/queue.md) → epics and issues (also `work/sentry/`, `work/otel/`) |

`sdl-server` and `odl-server` still build their standalone binaries
(`sentry-ducklake`, `otel-ducklake`); the e2e suite and load tests use them.
The product is `mallard`.

## Rules

- `duckdb = "=1.20000.1"` (DuckDB 2.0.0), one version for the whole
  workspace. Only `mallard-duck` depends on it; use `mallard_duck::duckdb`.
  `libduckdb-sys` must build without DEBUG (see the profile in
  `Cargo.toml`): debug builds abort on the prebuilt extensions.
- Infrastructure both sides need lives once, in `mallard-common` or
  `mallard-duck` (ADR 0005): config loading, durations/sizes, auth, error
  body, CORS, docs page, logging, signals, metrics, DuckDB set-up. Domain
  code (writers, maintenance, schemas) stays per side. Terminal output
  belongs in `mallard-term`.
- Config: libraries expose serde structs with `deny_unknown_fields` and
  never read files or env; binaries load them with
  `mallard_common::config::load`. mallard's `[otel]`/`[sentry]` hold only
  what each side owns.
- API errors are `{"error": code, "message": text}` (`mallard_common::http::ApiErr`);
  use `Q`/`J` extractors so bad input gets that body too.
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
- CLIs and TUIs print through `mallard-term`: server text goes through
  `text::clean`, shared TUI keys come from `tui::keys` (ADR 0006).

## Work

`work/queue.md` is the single queue (top = next). Epics in `work/epics/`,
issues in `work/issues/`. `work/sentry/` and `work/otel/` hold the history
of the two sides from before the merge; new items go in `work/`.
Decisions: `docs/adr/` (the side-specific ADRs stay in `docs/*/adr/`).
