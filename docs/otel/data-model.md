# Data model

How mallard stores OpenTelemetry data in DuckLake, and why. The table
definitions live in [`crates/mallard-otel-server/src/store/schema.rs`](../../crates/mallard-otel-server/src/store/schema.rs);
this document explains them.

## Goals

1. **Fast for the questions people actually ask.** Almost every query has a
   time window ("last 15 minutes", "last day") and usually a service. Some look
   up one trace by id. Few scan everything.
2. **Small on disk.** Telemetry is repetitive. Columnar Parquet with dictionary
   encoding and zstd compresses it 10-30×. The layout should help, not fight,
   that compression.
3. **Cheap to write and to expire.** Ingest appends only. Retention removes
   whole days. Neither should rewrite data.
4. **Plain SQL.** Anyone with DuckDB can open the lake and query it without
   our server: no opaque encodings, no join maze.

## Overview

All tables live in one DuckLake schema, `lake.otel`, next to the Sentry side's
`lake.sentry`:

| table | one row per | time column |
|---|---|---|
| `spans` | span (events and links nested) | `start_time` |
| `logs` | log record | `time` |
| `metric_points` | gauge or sum data point | `time` |
| `metric_histograms` | explicit-bucket histogram data point | `time` |
| `metric_exp_histograms` | exponential histogram data point | `time` |
| `metric_summaries` | summary data point | `time` |

Every table is:

```sql
PARTITIONED BY (epoch_day(<time column>))
SORTED BY (<see below>)
```

and written as Parquet with zstd compression and 122,880-row row groups.

One small extra table, `meta`, holds the maintenance lease
([operations](operations.md#several-replicas)). It has no telemetry, is not
partitioned and is not part of the query API.

## Design decisions

### 1. Wide, denormalized tables

Each row carries its resource attributes (`resource_attributes`) and the
promoted `service_name`, instead of pointing to a separate `resources` table.

- **Why:** the same resource repeats on thousands of rows, but Parquet
  dictionary-encodes repeated values and zstd removes the rest. Measured on
  synthetic shop traffic: **≈27 bytes per span on disk** including all
  attributes. A normalized model would save little space and would cost a
  join in every query.
- **Trade-off:** changing a resource attribute after the fact is not
  possible (it never is for telemetry).

### 2. `service_name` is a real column

`service.name` is copied out of the resource attributes into `service_name`
(missing → `unknown_service`). It is in nearly every filter and `GROUP BY`,
and it is part of the sort key (see 6). The value also stays in
`resource_attributes`, so nothing is lost.

### 3. IDs as native binary types

| OTLP field | column type | size | why |
|---|---|---|---|
| `trace_id` (16 bytes) | `UUID` | 16 B fixed | Parquet `FIXED_LEN_BYTE_ARRAY(16)`; equality and joins compare two 64-bit words |
| `span_id`, `parent_span_id` (8 bytes) | `UBIGINT` | 8 B | big-endian u64 of the 8 bytes; one integer compare |

A hex string (`VARCHAR`) would be 32 or 16 characters plus a length prefix,
roughly twice the size. Comparing strings is also slower.

Converting in SQL:

```sql
-- hex -> stored value
WHERE trace_id = '0af7651916cd43dd8448eb211c80319c'::UUID
WHERE span_id  = ('0x' || '00f067aa0ba902b7')::UBIGINT
-- stored value -> hex
SELECT replace(trace_id::VARCHAR, '-', '') AS trace_id_hex,
       lpad(lower(to_hex(span_id)), 16, '0') AS span_id_hex
```

Invalid IDs (wrong length or all zero) become `NULL`. A span without a valid
trace or span id is rejected, and the OTLP response reports it as partial
success.

### 4. Timestamps: `TIMESTAMP` (µs, UTC) plus nanosecond durations

- Times are `TIMESTAMP` (microsecond precision, no time zone, always UTC).
  `TIMESTAMPTZ` would make partition values and `date_trunc` depend on the
  session time zone. `TIMESTAMP_NS` is supported by fewer functions.
- Span duration is kept exactly as `duration_ns BIGINT`. The end time is
  `start_time + duration_ns`; it is not stored, because that would be one more
  8-byte column that can be derived.
- A zero OTLP timestamp falls back to the observed time (logs) or the receive
  time.

### 5. Attributes as `MAP(VARCHAR, VARCHAR)`

`attributes` and `resource_attributes` are string-to-string maps. OTLP
`AnyValue`s are stringified:

| AnyValue | stored as |
|---|---|
| string | as is |
| int, double, bool | `Display` form (`42`, `1.5`, `true`); non-finite doubles as `NaN`, `Infinity`, `-Infinity` |
| bytes | base64 |
| array, key-value list | JSON (`[1,"a"]`, `{"k":"v"}`); non-finite doubles as the strings `"NaN"`, `"Infinity"`, `"-Infinity"` |

A log `body` is stringified the same way. The non-finite spellings are the
OTLP/JSON (protojson) ones, and `TRY_CAST(... AS DOUBLE)` reads them back.

Duplicate keys keep the last value, and keys are stored sorted. Query with
`attributes['http.route']` (it returns `NULL` when the key is missing); use
`TRY_CAST` for numeric comparisons:

```sql
SELECT count(*) FROM spans
WHERE start_time > now()::TIMESTAMP - INTERVAL 1 HOUR
  AND TRY_CAST(attributes['http.response.status_code'] AS INTEGER) >= 500;
```

**Alternatives considered** (see [ADR 0002](adr/0002-data-model.md)):

- `VARIANT` (typed, can be shredded into columns with statistics). DuckLake 1.1
  supports it and dotted keys work (`variant_extract(attributes, 'http.route')`),
  but it is measured about 2-8× slower than the map on attribute queries, shredded
  and all, at the same size. The Rust `duckdb` crate cannot decode the type
  either ([bench/otel/typed-attributes](../../bench/otel/typed-attributes/README.md)).
- `JSON` text: larger, and needs parsing on every read.
- Promoting semantic-convention attributes to columns: faster filters, but a
  large and ever-changing schema. Today only `service_name` is promoted.
  Measured for `http.route`, `http.response.status_code` and `db.system.name`:
  a query that reads only the column is 6-11× faster, but the API filter must
  also see resource attributes and rows from before the column, and then the
  gain is 1.3-2.4× (from about 200-400 ms to 100-300 ms for one hour of 2 M
  spans), for one API path and three keys. Not worth a permanent schema
  change until real installs hit this path. More can be appended later
  without rewriting old data (see 12).

### 6. Sort order inside files

DuckLake sorts rows on insert and on compaction (`SORTED BY`). The order
decides how well the min/max statistics of each Parquet row group prune
reads.

| table | sort key |
|---|---|
| `spans` | `date_trunc('hour', start_time), service_name, start_time` |
| `logs` | `date_trunc('hour', time), service_name, time` |
| `metric_*` | `metric_name, series_id, time` |

For spans and logs we measured three layouts on 4 M spans
([bench/otel/sort-order](../../bench/otel/sort-order/README.md)):

| query | time only | service, time | **hour, service, time** |
|---|---|---|---|
| RED per service, 2 h | 115 ms | 91 ms | **76 ms** |
| one service per minute, 2 h | 62 ms | **20 ms** | 24 ms |
| attribute filter, 1 h | 74 ms | 70 ms | **54 ms** |
| trace by id, 2 h | **25 ms** | 36 ms | 41 ms |

Sorting only by time maximizes time pruning, but every row group mixes all
services. Sorting by service first destroys time locality after compaction
(a merged file would cover a whole day per service). The hour bucket keeps
each hour together, and within the hour groups rows by service: good at both
filters. Trace lookups get a bit slower, but they stay well under 100 ms.

Metric queries always name one metric, so `metric_name` comes first. Then
`series_id`, so that one series' points are adjacent (good for the
reset-aware `lag()` in counter rates).

### 7. Partitioning by day

`epoch_day(time)` partitions (DuckLake 1.1 transform: days since 1970, the
same as Iceberg).

- **Retention is free.** Retention cut-offs are aligned to whole UTC days, so
  `DELETE ... WHERE time < cutoff` covers whole partitions. DuckLake then only
  marks files as removed and writes **no delete files**. (Verified in
  `crates/mallard-otel-server/tests/e2e.rs`, `compaction_and_retention`.)
- **Pruning:** queries with a time window skip other days via partition
  values, and skip files within a day via per-file min/max in the catalog.
- **Compaction never crosses days**, so merged files stay time-local.
- **Why not hourly?** At small/mid scale a day is 50 MB to a few GB of
  Parquet. Hourly partitions would produce 24× more small files to manage,
  without better pruning, because per-file statistics already narrow the time
  range.
- **Why not by service?** The number of services is unbounded, and
  service × day multiplies the file count. Service locality comes from the
  sort key instead.

### 8. Nested span events and links

`events STRUCT(time, name, attributes)[]` and
`links STRUCT(trace_id UUID, span_id UBIGINT, trace_state, attributes)[]` are
lists inside the span row, not separate tables.

- A trace view needs a span's events (exceptions!) every time; nesting avoids
  a join, and Parquet stores the nested columns separately, so queries that
  don't touch events don't read them.
- Searching exceptions:

  ```sql
  SELECT service_name, e.attributes['exception.type'] AS type, count(*)
  FROM spans, unnest(events) AS u(e)
  WHERE start_time > now()::TIMESTAMP - INTERVAL 1 DAY AND e.name = 'exception'
  GROUP BY ALL ORDER BY 3 DESC;
  ```

### 9. Low-cardinality enums as strings

`kind` (`SERVER`, `CLIENT`, ...), `status_code` (`UNSET`, `OK`, `ERROR`),
`temporality`, and `metric_type` are `VARCHAR`, not small integers. Parquet
dictionary-encodes them to a few bits per row, and DuckDB filters on the
dictionary, so they cost about the same as integers. They are much easier to
read in ad-hoc SQL.

### 10. Metrics

- One table per data point shape (number, histogram, exponential histogram,
  summary). Each has the right typed columns (`bucket_counts UBIGINT[]`,
  `explicit_bounds DOUBLE[]`, `scale`, `quantiles STRUCT(...)[]`, ...) and
  no sparse nullable columns. Gauges and sums share `metric_points`,
  distinguished by `metric_type` and `is_monotonic`.
- `series_id UBIGINT` = xxh3 hash of (resource attributes, instrumentation
  scope, metric name, data point kind, point attributes), i.e. the OTel stream
  identity. It identifies a time series without comparing maps. Rates use
  `lag(value) OVER (PARTITION BY series_id ORDER BY time)`. Every string and
  list is hashed with its length first, so no two different identities share
  an encoding. (Versions before this rule hashed with separator bytes, which
  crafted keys could collide; series written by them get new ids after an
  upgrade, so a rate across the upgrade moment starts over.)
- `temporality` is kept per point (`cumulative`, `delta`, or empty for
  gauges). The query API handles both: increase = difference of last values
  per bucket for cumulative (a drop counts as a counter reset), and sum of
  values for delta. One metric may mix both: each series is handled by
  its own temporality.
- Integer values are stored as `DOUBLE`, which is exact up to 2^53.
- Non-finite values (`NaN`, `±Infinity`; in OTLP/JSON the strings `"NaN"`,
  `"Infinity"`, `"-Infinity"`) are stored as the DOUBLE values `nan` and
  `±inf`. JSON has no such numbers: the query API returns them as `null`.
- **Exemplars** (sample measurements that link a point to a trace) are kept
  per point in `exemplars STRUCT(time TIMESTAMP, value DOUBLE, trace_id UUID,
  span_id UBIGINT, attributes MAP(VARCHAR, VARCHAR))[]`, the last column of
  `metric_points`, `metric_histograms` and `metric_exp_histograms` (OTLP
  summaries have none). Same conventions as span links: ids as `UUID` /
  `UBIGINT` (invalid → `NULL`), `attributes` are the exemplar's filtered
  attributes, a zero time falls back to the point's time. Nested like span
  events, so queries that don't touch exemplars don't read them
  ([ADR 0012](../adr/0012-store-metric-exemplars.md)). Jump to the traces:

  ```sql
  SELECT e.time, e.value, replace(e.trace_id::VARCHAR, '-', '') AS trace_id
  FROM metric_histograms, unnest(exemplars) AS u(e)
  WHERE metric_name = 'http.server.request.duration'
    AND time > now()::TIMESTAMP - INTERVAL 1 HOUR AND e.trace_id IS NOT NULL
  ORDER BY e.value DESC LIMIT 10;
  ```
- **Not stored:** metric `metadata`. Exponential histogram
  negative buckets are kept as stored, but folded into one "≤ 0" bucket when
  computing percentiles.

### 11. Small inserts are inlined

With `storage.data_inlining_row_limit` (default 1000), DuckLake stores
inserts below that many rows inside the catalog database instead of writing a
tiny Parquet file. Quiet periods therefore don't create thousands of
kilobyte-sized files. The `otel.maintenance.compact_interval` task flushes inlined
rows into Parquet. Queries read inlined and Parquet data together
transparently.

### 12. Schema evolution

Rows are appended by column name. New columns are only ever added at the
end. On start-up, missing columns are added with `ALTER TABLE ... ADD
COLUMN`, and old files simply return `NULL` for them, with no rewrite. Columns
the running version does not know (added by a newer version) are left
`NULL`, with a warning in the log, so mixed versions can share a catalog
during a rolling upgrade.

## Write path, briefly

1. A receiver decodes the OTLP request and converts it to Arrow record
   batches (one per table touched), off the async runtime.
2. The batches go into an in-memory buffer (bounded; full → HTTP 429 / gRPC
   `UNAVAILABLE`).
3. Every `otel.ingest.flush_interval` (5 s) or `flush_rows` (200k), one flusher
   appends all buffered batches through DuckDB's Arrow appender in **one
   transaction**. That gives one DuckLake snapshot, with at most one new file
   (or one inlined insert) per table and day.
4. Maintenance merges small files (`compact_interval`, files < 16 MB), merges
   up to 128 MB files (`full_compact_interval`), deletes expired days, expires
   old snapshots and removes unreferenced files.

## Example queries

```sql
-- Slowest endpoints of the last hour
SELECT service_name, name, count(*) AS n,
       approx_quantile(duration_ns, 0.95) / 1e6 AS p95_ms
FROM spans
WHERE start_time > now()::TIMESTAMP - INTERVAL 1 HOUR AND kind = 'SERVER'
GROUP BY ALL ORDER BY p95_ms DESC LIMIT 10;

-- Logs of one trace, joined with the span that wrote them
SELECT l.time, l.severity_text, l.body, s.name AS span
FROM logs l LEFT JOIN spans s USING (trace_id, span_id)
WHERE l.trace_id = '0af7651916cd43dd8448eb211c80319c'::UUID
  AND l.time > now()::TIMESTAMP - INTERVAL 1 DAY
  AND s.start_time > now()::TIMESTAMP - INTERVAL 1 DAY
ORDER BY l.time;

-- Error rate per service per 5 minutes
SELECT time_bucket(INTERVAL 5 MINUTE, start_time) AS t, service_name,
       avg((status_code = 'ERROR')::INT) AS error_rate
FROM spans WHERE start_time > now()::TIMESTAMP - INTERVAL 6 HOUR
GROUP BY ALL ORDER BY t;

-- Requests per second from a cumulative counter (per series, reset-aware)
WITH p AS (
  SELECT series_id, time, value,
         value - lag(value) OVER (PARTITION BY series_id ORDER BY time) AS d,
         epoch(time) - epoch(lag(time) OVER (PARTITION BY series_id ORDER BY time)) AS dt
  FROM metric_points
  WHERE metric_name = 'http.server.requests' AND time > now()::TIMESTAMP - INTERVAL 1 HOUR
)
SELECT time_bucket(INTERVAL 1 MINUTE, time) AS t,
       sum(CASE WHEN d < 0 THEN value ELSE d END / dt) AS rps
FROM p WHERE d IS NOT NULL GROUP BY 1 ORDER BY 1;
```

## Opening the lake without the server

Use DuckDB 2.0, the version mallard is built with: an older CLI refuses the catalog
file. The lake holds both schemas, `lake.otel` and `lake.sentry`, so errors and spans
join on `trace_id`.

```sql
INSTALL ducklake; LOAD ducklake;
-- DuckDB-file catalog (stop the server first: a DuckDB file has one writer)
ATTACH 'ducklake:data/catalog.ducklake' AS lake (DATA_PATH 'data/lake/', READ_ONLY);
-- or a Postgres catalog (safe while the server runs)
ATTACH 'ducklake:postgres:host=pg dbname=ducklake user=mallard' AS lake (READ_ONLY);
USE lake.otel;
SELECT service_name, count(*) FROM spans GROUP BY 1;
```

The `DATA_PATH` must match the path stored in the catalog. DuckLake refuses a
different one (`OVERRIDE_DATA_PATH true` forces it, for example after moving
a bucket).
