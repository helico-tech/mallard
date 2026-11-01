---
status: todo
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
