---
status: todo
type: bug
created: 2026-11-02
---
# `mallard trace` may fail where the old `telemetry trace` fell back to 24h

## Problem
`mallard trace <id>` asks for 7 days by default. On a server with `otel.query.max_window` below that, the removed
`telemetry trace` said so and searched the last 24 hours. `mallard trace` has no such fallback, so the default command
probably fails with a window error on that server. The TUI handles this case on its own (`short_window`).

From reading the code; not run.

## How to reproduce
Run `mallard serve` with `MALLARD_OTEL__QUERY__MAX_WINDOW=1d`, then `mallard trace <id>` without `--since`.

## Notes
Found while merging the removal of the hidden commands (issue 0183). The default of 7 days is `WINDOW` in
`crates/mallard/src/trace.rs`.
