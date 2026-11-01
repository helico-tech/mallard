---
status: done
created: 2026-11-01
---
# 11 — Crate cleanup: one naming scheme, no standalone leftovers

## Goal
The crates still carry the names and set-up of the two old projects. A new
reader should see one product.

## What is wrong (checked 2026-11-01)
- `sdl-*` and `odl-*` mean nothing to a new reader, next to `mallard-*`.
- `sdl-types` and `odl-api` do the same job under different names.
- `sdl-server/src/standalone.rs` (294 lines of clap flags and `SDL_*` env
  vars) survives only for tests; its binary is gone.
- OpenAPI titles still say `sentry-ducklake API` and `otel-ducklake query
  API`; about 25 source files still use the old names or env vars.

## Acceptance criteria
- Crates renamed: `sdl-*` to `mallard-sentry-*`, `odl-*` to `mallard-otel-*`,
  `sdl-types` to `mallard-sentry-api`.
- `standalone.rs` replaced by a small test helper; `SDL_*` and `ODL_*` env
  vars are gone from code and docs, unless a test needs its own.
- Old product names are gone outside `work/` and history notes. Metric names
  (`sdl_*`, `odl_*`) are renamed too, since nothing outside this repo uses
  them yet.
- Tests, e2e, Docker, Helm and OpenAPI snapshots pass.

## Plan
Run after story 10, as one mechanical change per step, and before the docs
rewrite in issue 0016.

1. Rename packages, directories and Rust paths with `git mv` and a scripted
   replace; no behaviour change (one commit, `cargo fmt` included).
2. Replace `standalone.rs`, `Server::new` and `storage::lake` with a small helper
   in `tests/integration.rs`; drop `clap` from the server crate.
3. `SDL_*`/`ODL_*` out of the code: default CLI hints, `Config::load` of the otel
   crate (no callers), test switches become `MALLARD_TEST_POSTGRES`.
4. Metric names: `sdl_*` to `mallard_sentry_*`, `odl_*` to `mallard_otel_*`.
5. Product names in code, then regenerate both OpenAPI snapshots (same paths).
6. Mechanical docs pass: crate, env var, metric and binary names; no rewrite.
7. ADR 0024 with the full mapping. Verify: workspace, slim build, e2e, Helm,
   Docker, and an upgrade run from a build made before the rename.

Must not change: the `meta` lease, schemas, table names, anything on disk, and
wire names that SDKs or sentry-cli rely on.

## Progress log
- 2026-11-01: baseline binary and test count taken from `d9218d6` before any edit: 499 passed, 4 ignored.
- 2026-11-01: steps 1 to 6 done, one commit each. No `SDL_*` variable was read in production: only `Standalone` read them, and mallard never built it. All settings that matter have a `[sentry.*]`, `[server]`, `[storage]` or `[duckdb]` key (mapping in ADR 0024). `SDL_CATALOG_WIDE_MAINTENANCE` and `SDL_MAINTENANCE_THREADS` have no equal, and never had one in mallard.
- 2026-11-01: checked. Workspace 499 passed and 4 ignored, as before (two `Standalone` tests went, two config tests came). Slim build 41 passed, no DuckDB in its tree. e2e 28 of 28. Helm lint and template on all CI values. Docker image serves `/readyz`. `kind` is not installed, so `deploy/helm/kind/test.sh` did not run.
- 2026-11-01: upgrade check: lakes made by the build from `d9218d6` (inlined and Parquet, with a lease in `otel.meta` and a sourcemap bundle) read fine with the new build; same file layout, no errors in the log. Both `MALLARD_TEST_POSTGRES` tests pass, each on its own empty database.

## Follow-ups
- [0120](../../issues/0120-sentry-compaction-threads-in-mallard.md): Sentry compaction uses all DuckDB threads inside mallard.
- [0121](../../issues/0121-standalone-paths-left-in-side-crates.md): standalone paths left in the side crates.
- [0016](../../issues/0016-per-side-docs-for-mallard.md) stays open: the per-side docs now have the right names, but some of their content (Helm values, ports) still describes the old binaries.
