# 0012 — Metric exemplars are stored nested per data point

## Status
accepted (amends otel ADR 0002: exemplars are now stored)

## Context
OTLP metric points (gauges, sums, histograms, exponential histograms) can
carry exemplars: a few sample measurements, each with a time, a value, the
trace and span that were active, and the attributes the aggregation dropped
("filtered attributes"). They are how a dashboard jumps from a latency
spike to a trace. Until now they were dropped at ingest (otel issue 0001).

Options:
- A nested list column on each metric table: one row per point, as span
  events and links are stored. Parquet keeps nested columns apart, so
  queries that don't use exemplars don't read them. No join.
- A separate `metric_exemplars` table keyed by `series_id` and time: a
  sixth write path and a join to find the point an exemplar belongs to.
- Not storing them: no way to get from a metric to a trace.

## Decision
- `metric_points`, `metric_histograms` and `metric_exp_histograms` get a
  last column `exemplars STRUCT(time TIMESTAMP, value DOUBLE, trace_id UUID,
  span_id UBIGINT, attributes MAP(VARCHAR, VARCHAR))[]`. Same conventions as
  span links: `trace_id UUID`, `span_id UBIGINT`, invalid ids `NULL`; a
  zero exemplar time falls back to the point's time. Integer values become
  `DOUBLE`. `metric_summaries` has none (OTLP summaries carry no exemplars).
- Existing catalogs get the column at start-up through the usual
  `ALTER TABLE ... ADD COLUMN` (data-model.md section 12); old rows read
  `NULL`, which the query treats as no exemplars.
- The query API returns exemplars only on request (`metrics/query` with
  `exemplars=true`): per series (the `group_by` labels) and time bucket, the
  exemplar with the highest value that has a trace id. At most 1000 per
  response. The highest value is the one worth opening for latency spikes;
  one per bucket keeps the answer small. The CLI shows them with
  `--exemplars`.

## Consequences
- Metric rows grow only when senders attach exemplars (SDKs sample a few
  per point; an empty list costs a few bytes per row after compression).
- Exemplars follow the point's retention and compaction; nothing new to
  maintain.
- Older versions sharing a catalog during a rolling upgrade leave the
  column `NULL` for their rows (append by column name).
- Finding exemplars needs `unnest(exemplars)`; documented in
  docs/otel/data-model.md with an example.
