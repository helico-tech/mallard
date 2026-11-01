---
status: done
type: debt
created: 2026-11-01
---
# Metric queries assume one temporality per metric

## Problem
`detect()` takes any_value(temporality) per metric. A metric exported with both delta and cumulative series would run delta series through cumulative reset logic.

## Notes
Found during the v0.1 build and review (epic 0001).

Done (2026-11-01): `detect()` no longer reads temporality. The query path
reads it per point: counters compute cumulative differences partitioned by
`(series_id, cumulative?)` and sum delta points; `sum` picks average or total
per series; histograms run one pass over cumulative points (last per bucket,
reset-aware) and one over delta points, merged per bucket. Regression test
`mixed_temporality_in_one_metric` (e2e): a delta and a cumulative series
(with a reset) in one sum and one histogram. Small change: delta sums and
histograms now count the whole first bucket (as cumulative ones always did),
not only the part after `from`, so both kinds cover the same interval. The catalog (`GET /metrics`) still shows one temporality per metric:
issue 0031.
