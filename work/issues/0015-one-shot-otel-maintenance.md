---
status: done
type: debt
created: 2026-11-01
---
# No one-shot maintenance command since `otel-ducklake` is gone

## Problem
`otel-ducklake maintenance [--full]` ran the OTel side's maintenance once
and exited (flush inlined rows, merge small files or with `--full` all
files below the target size, then retention). The capacity runs in
`docs/otel/capacity.md` used it to compact a backfilled lake before timing
queries. The binary was retired (story 0002/06); `mallard-server` has no
such command, and the OTel query API has no maintenance endpoint (the
Sentry side has `POST /sentry/v1/maintenance`).

## Notes
The library function is still there: `odl_server::run_maintenance(&cfg,
full)`. A `mallard-server maintenance [--full]` subcommand could call it
with `Config::otel_config()` (server stopped, DuckDB-file or SQLite
catalog), plus the Sentry side's flush. Decide whether the Sentry side
needs the same, and whether catalog-wide steps belong there too.

Done 2026-11-01: `mallard-server maintenance [--full]`
(`mallard_server::run_maintenance`). Sentry first (new public
`sdl_server::Server::run_maintenance`, the same as `POST /v1/maintenance`), then
`odl_server::run_maintenance`, because otel owns the catalog-wide snapshot expiry
and file cleanup. `--full` only changes the telemetry tables: Sentry's compaction
has no such mode. The catalog-wide steps stay on the otel side. With a running
server on a DuckDB-file catalog the command fails (tested). `docs/otel/capacity.md`
updated.
