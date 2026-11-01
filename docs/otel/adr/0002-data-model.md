# 0002. Telemetry data model in DuckLake

Date: 2026-11-01

## Status

accepted; amended by [ADR 0012](../../adr/0012-store-metric-exemplars.md)
(exemplars are stored)

## Context

The table layout determines query speed, size on disk and write cost. The
full reasoning and measurements are in [docs/data-model.md](../data-model.md).

## Decision

- Wide, denormalized tables per signal: `spans` (events and links nested),
  `logs`, and four metric tables by point shape.
- `service_name` promoted to a column; attributes as `MAP(VARCHAR, VARCHAR)`
  with stringified values.
- `trace_id UUID`, `span_id UBIGINT`; times as `TIMESTAMP` (µs, UTC) and
  `duration_ns BIGINT`.
- `PARTITIONED BY (epoch_day(time))`; retention aligned to whole days.
- `SORTED BY (date_trunc('hour', time), service_name, time)` for spans and
  logs, `SORTED BY (metric_name, series_id, time)` for metrics, chosen by
  measurement (bench/sort-order).
- `series_id` = xxh3 of resource attributes + instrumentation scope + metric
  name + data point kind + point attributes (the OTel stream identity).
- Columns are append-only; new columns are added at the end. Rows are
  appended by column name, so versions that differ by added columns can write
  to the same tables.

## Consequences

- Typical queries prune by day partition, by file statistics, and by row
  group statistics on hour and service.
- Retention never rewrites data and creates no delete files.
- Attribute values lose their OTLP type (all strings). Numeric filters need
  `TRY_CAST`.
- Exemplars and metric metadata are not stored.
- Changing a sort key or partitioning later only affects new files (DuckLake
  applies the new layout on write and compaction).
