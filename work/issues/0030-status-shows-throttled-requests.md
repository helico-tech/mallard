---
status: done
type: idea
created: 2026-11-01
---
# CLI/TUI status: show throttled requests

## Problem
`GET /api/v1/stats` has `throttled_requests` per signal (issue 0013), but
the `mallard status` table and the TUI status view show only `requests`,
`accepted`, `throttled` and `invalid`. Requests refused by the in-flight
limit are visible in JSON output and Prometheus only.

## Notes
Add a column to `odl-cli/src/fmt.rs` and `odl-cli/src/tui/views/status.rs`.

Done 2026-11-01. Text table: column `throttled requests`. TUI: column
`thr reqs`. The TUI puts Ingest and Storage side by side from 120 columns
(was 100): below that, the six ingest columns do not fit in 45% of the width.
`mallard status` itself stays one line per side, with no ingest table.
