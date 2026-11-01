---
status: todo
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
