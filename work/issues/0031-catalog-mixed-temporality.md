---
status: done
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

Done 2026-11-01. The catalog now says `mixed` for a metric with both delta and
cumulative points: one new value in `Temporality`, no new field. A list of
temporalities would be a bigger API change and nothing needs more than this.
Empty temporalities (a metric that is a gauge in one place and a sum in
another) do not count as a kind, so such a metric shows the sum's temporality.
