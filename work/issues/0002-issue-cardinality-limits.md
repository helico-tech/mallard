---
status: todo
type: debt
created: 2026-11-01
---
# 0002. No limit on the number of issues per project

## Problem
`issues` rows and the in-memory issue-status cache never shrink. A noisy
`captureMessage("order 8f3a... failed")` whose variable part is not masked
by grouping, or a custom fingerprint with ids in it, can create one issue per
event: up to ~1M issues a day, a multi-GB cache and a slow startup.

## Notes
- Fix sketch: cap new issues per project per hour (overflow goes into one
  "too many issues" group, counted in a metric), and expire issues whose last
  event is older than the retention period (drop from cache; append-only
  `issues` rows can be rewritten during compaction or filtered by last seen).
- Found by the data-model review (2026-11-01).
