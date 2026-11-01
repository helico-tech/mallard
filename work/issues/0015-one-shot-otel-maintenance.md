---
status: todo
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
