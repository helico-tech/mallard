---
status: todo
type: debt
created: 2026-11-01
---
# Stats: split "rejected" into throttled vs invalid

## Problem
`SignalIngestStats.rejected` counts both back-pressure refusals (retried by clients) and invalid items. Operators need them apart.

## Notes
Found during the v0.1 build and review (epic 0001).
