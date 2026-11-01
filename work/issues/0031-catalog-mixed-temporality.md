---
status: todo
type: debt
created: 2026-11-01
---
# Metric catalog shows one temporality for mixed metrics

## Problem
`GET /api/v1/metrics` (`query::metrics::list`) takes `any_value(temporality)`
per metric. Queries handle mixed delta/cumulative series correctly since
otel issue 0008, but the catalog shows an arbitrary one of the two.

## Notes
Options: a list of temporalities (API change), or a fixed rule. Decide
when someone needs it.
