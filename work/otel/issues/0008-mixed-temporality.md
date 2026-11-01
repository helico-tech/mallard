---
status: todo
type: debt
created: 2026-11-01
---
# Metric queries assume one temporality per metric

## Problem
`detect()` takes any_value(temporality) per metric. A metric exported with both delta and cumulative series would run delta series through cumulative reset logic.

## Notes
Found during the v0.1 build and review (epic 0001).
