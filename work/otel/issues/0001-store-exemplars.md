---
status: done
type: idea
created: 2026-11-01
---
# Exemplars are not stored

## Problem
Metric exemplars (links from metric points to traces) are dropped at ingest. Storing them as a nested list per point would allow "jump from a latency spike to a trace".

## Notes
Found during the v0.1 build and review (epic 0001).

Done (ADR 0012):
- `metric_points`, `metric_histograms` and `metric_exp_histograms` have a
  last column `exemplars STRUCT(time, value, trace_id UUID, span_id
  UBIGINT, attributes MAP)[]` (summaries have no exemplars in OTLP).
  Existing catalogs get it at start-up (`ALTER TABLE ADD COLUMN`); tested
  with a table created by the old DDL (`e2e::upgrade_adds_exemplars_to_an_old_catalog`).
- `metrics/query?exemplars=true` returns per series and bucket the exemplar
  with the highest value that has a trace id (max 1000). Code in
  `query/exemplars.rs`, so the metric math in `query/metrics.rs` is
  untouched apart from making `labels_expr` visible.
- CLI: `odl metric … --exemplars` / `mallard metrics show … --exemplars`.
- The TUI chart does not show exemplars yet; not needed for the API use.
