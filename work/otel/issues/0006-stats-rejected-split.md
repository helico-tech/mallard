---
status: done
type: debt
created: 2026-11-01
---
# Stats: split "rejected" into throttled vs invalid

## Problem
`SignalIngestStats.rejected` counts both back-pressure refusals (retried by clients) and invalid items. Operators need them apart.

## Notes
Found during the v0.1 build and review (epic 0001).

Fixed (2026-11-01). `SignalIngestStats.rejected` is replaced by
`throttled` (buffer full: HTTP 429 / gRPC UNAVAILABLE, exporters retry)
and `invalid` (refused for good: invalid ids reported as partial success,
or a request too large for the buffer); both `#[serde(default)]`, so a new
client reads an old server. Prometheus: `odl_ingest_rejected_items_total`
keeps its name and gets a `reason` label (`throttled` | `invalid`), so
`sum by (signal)` still gives the old total. `odl status` and the TUI
status view show both columns (throttled in yellow, invalid in red).
docs/otel/operations.md and openapi.json updated. Test:
`otlp::tests::throttled_and_invalid_items_are_counted_apart`. Follow-up:
requests refused by the in-flight limit are not counted at all
(work/issues/0013).
