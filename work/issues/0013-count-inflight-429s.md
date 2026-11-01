---
status: done
type: idea
created: 2026-11-01
---
# Stats: count requests refused by the in-flight limit

## Problem
OTLP requests refused before decoding because too many bytes are in flight
(`Ingest::admit`: HTTP 429 / gRPC `UNAVAILABLE`) are not counted anywhere:
not in `requests`, not in `throttled` (their items are unknown), not in
Prometheus. An operator who sees 429s at exporters but `throttled = 0` has
no server-side number for them.

## Notes
Found while splitting `rejected` into `throttled`/`invalid` (otel issue
0006). A per-signal `throttled_requests` counter (stats + Prometheus) would
cover both kinds of back-pressure.

Done (2026-11-01): `SignalIngestStats.throttled_requests` and Prometheus
`odl_ingest_throttled_requests_total{signal}` count every request refused
with back-pressure: by the in-flight limit (`Ingest::admit` now takes the
signal; also counted in `requests`, like other early refusals) and by the
full buffer. Items stay in `throttled` only when known. Documented in
`docs/otel/operations.md`. Tests: `inflight_budget_bounds_concurrent_requests`,
`throttled_and_invalid_items_are_counted_apart`, the Prometheus test in
`stats.rs`. Not yet in the CLI/TUI status table: issue 0030.
