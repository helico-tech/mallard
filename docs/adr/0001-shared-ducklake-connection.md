# 0001 — Embed both libraries on one shared DuckLake connection

## Status
accepted

## Context
sentry-ducklake and otel-ducklake each can open their own DuckDB/DuckLake.
The platform's main value is joining errors and traces by trace id. Two
separate catalogs would need cross-catalog queries and twice the config.

## Decision
`obs-server` opens one DuckDB connection, sets up secrets and ATTACHes the
DuckLake catalog `AS lake`. It passes clones of that connection to
`odl_server::Server::with_connection` (schema `otel`) and
`sdl_server::Server::with_connection` (schema `sentry`). Exactly one side
runs catalog-wide maintenance (snapshot expiry, file cleanup): otel, with
sentry's catalog-wide flag off. Both crates pin `duckdb = "=1.20000.1"`.

Join keys: `trace_id` UUID, `span_id` UBIGINT in both schemas.

## Consequences
- Correlation is a plain SQL join in one catalog.
- Storage config lives once, in `[storage]` of the obs config.
- A duckdb upgrade must happen in all three repos at once.
- Both libraries must support an injected connection; that is a contract
  with the sibling repos (story 0001/01).
