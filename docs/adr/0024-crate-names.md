# 0024 — One naming scheme for crates, metrics and test switches

## Status
accepted

## Context
The repo joined two codebases (ADR 0004). Their crates kept the prefixes of the
old projects, `sdl-*` and `odl-*`, next to `mallard-*`, and `sdl-types` and
`odl-api` did the same job under different names. The metrics, the OpenAPI titles
and many messages still used the old names. `sdl-server` kept 294 lines of clap
flags and `SDL_*` variables (`Standalone`) that only its tests used, because the
binary had gone (ADR 0023). A new reader should see one product.

Older ADRs, `docs/*/adr/` and `work/` are history and keep the old names. This ADR
is the key to follow them.

## Decision

Crates (package, directory, Rust path follow, e.g. `sdl_server` is now
`mallard_sentry_server`):

| old | new |
|---|---|
| `sdl-protocol` | `mallard-sentry-protocol` |
| `sdl-server` | `mallard-sentry-server` |
| `sdl-types` | `mallard-sentry-api` |
| `sdl-client` | `mallard-sentry-client` |
| `sdl-cli` | `mallard-sentry-cli` |
| `sdl-loadgen` | `mallard-sentry-loadgen` (also the binary) |
| `odl-api` | `mallard-otel-api` |
| `odl-server` | `mallard-otel-server` |
| `odl-client` | `mallard-otel-client` |
| `odl-cli` | `mallard-otel-cli` |
| `odl-loadgen` | `mallard-otel-loadgen` (also the binary) |

Other names:

| old | new |
|---|---|
| metrics `sdl_*` | `mallard_sentry_*` |
| metrics `odl_*` | `mallard_otel_*` (`mallard_bridge_*` stays) |
| `sentry-ducklake`, `sentry-ducklake API` | `mallard`, `mallard errors API` |
| `otel-ducklake`, `otel-ducklake query API` | `mallard`, `mallard telemetry API` |
| commands `sdl …`, `odl …` | `mallard …` (`mallard errors …` and `mallard telemetry …` where there is no flat command) |
| `User-Agent` `sdl-client/x`, `odl-client/x` | `mallard-sentry-client/x`, `mallard-otel-client/x` |
| `SDL_TEST_POSTGRES`, `ODL_TEST_POSTGRES` | `MALLARD_TEST_POSTGRES` |
| `ODL_TEST_URL` | `MALLARD_TEST_URL` |
| `ODL_INGEST_TOKEN` (load generator) | `MALLARD_INGEST_TOKEN` |
| `SDL_URL`, `SDL_TOKEN`, `SDL_PROJECT`, `ODL_URL`, `ODL_TOKEN` | `MALLARD_URL`, `MALLARD_TOKEN`, `MALLARD_PROJECT` |

The `SDL_*` server settings were read only by `Standalone`. mallard never used
it, so no running server loses a setting. They map to the mallard config:

| old | now |
|---|---|
| `SDL_LISTEN`, `SDL_METRICS_LISTEN`, `SDL_PUBLIC_URL`, `SDL_API_TOKEN` | `server.http_addr`, `.metrics_addr`, `.public_url`, `.api_tokens` |
| `SDL_LOG_JSON` | `server.log_format = "json"` |
| `SDL_CATALOG` | `storage.catalog` |
| `SDL_STORAGE_URL` | `storage.data_path` and `storage.artifacts_path` |
| `SDL_S3_ENDPOINT`, `SDL_S3_REGION`, `SDL_S3_PATH_STYLE` | `[[storage.secrets]]`: `endpoint`, `region`, `url_style` |
| `SDL_DUCKDB_THREADS`, `SDL_DUCKDB_MEMORY_LIMIT`, `SDL_DUCKDB_TEMP_DIR`, `SDL_EXTENSION_DIR` | `duckdb.threads`, `.memory_limit`, `.temp_directory`, `.extension_directory` |
| `SDL_RETENTION_DAYS` | `sentry.retention.events` |
| `SDL_BATCH_MAX`, `SDL_BATCH_WINDOW`, `SDL_QUEUE_CAPACITY`, `SDL_INGEST_CONCURRENCY`, `SDL_MAX_EVENT_BYTES`, `SDL_MAX_BODY_BYTES`, `SDL_TRUST_FORWARDED_FOR` | `sentry.ingest.batch_max`, `.batch_window`, `.queue_capacity`, `.concurrency`, `.max_event_bytes`, `.max_body_bytes`, `.trust_forwarded_for` |
| `SDL_FLUSH_INTERVAL`, `SDL_FLUSH_ROWS`, `SDL_COMPACT_INTERVAL` | `sentry.maintenance.flush_interval`, `.flush_rows`, `.compact_interval` |
| `SDL_READERS`, `SDL_ENABLE_SQL` | `sentry.query.readers`, `.enable_sql` |
| `SDL_SOURCEMAP_CACHE_MB` | `sentry.sourcemaps.cache_size` |
| `SDL_SNAPSHOT_RETENTION` | `otel.maintenance.snapshot_retention` (otel owns catalog-wide maintenance, ADR 0001) |
| `SDL_CATALOG_WIDE_MAINTENANCE` | none: mallard always leaves it to otel |
| `SDL_MAINTENANCE_THREADS` | none: must stay 0 on a shared connection (`Host::maintenance_threads`, issue 0120) |

`ODL_<SECTION>__<KEY>` became `MALLARD_OTEL__<SECTION>__<KEY>`, or
`MALLARD_<SECTION>__<KEY>` for what mallard owns (`storage`, `duckdb`, `server`).

Unchanged on purpose:

- On disk: catalog schemas `sentry` and `otel`, table names, the `meta` lease
  (`otel.meta`), Parquet paths. Existing lakes keep working without a migration.
- Stored values are not rewritten. Old `send-test` events keep their `sdl.*` names;
  new ones use `mallard.*`, so the first after an upgrade opens a new issue.
- Sentry wire names (`X-Sentry-Auth`, `sentry_key`, `/api/0`, envelope items),
  OTel attribute names, and the paths `docs/sentry/openapi.json` and
  `docs/otel/openapi.json`.

`Standalone` is replaced by a small helper in the integration tests, which build
`Config` and `Host` directly. Sentry-side metrics, env vars and names are renamed
without aliases: nothing outside this repo uses them yet.

## Consequences
- One prefix for everything in `crates/`; `mallard-sentry-*` and `mallard-otel-*`
  say which side a crate belongs to.
- Dashboards or alerts on `sdl_*`/`odl_*`, had any existed, would have to change.
  Nothing in the repo (Helm, bench) names them.
- Long-lived branches need a rebase across the rename; `git log --follow` still
  finds the history.
- Older ADRs and `work/` still say `sdl-*`/`odl-*`. Use this table to read them.
