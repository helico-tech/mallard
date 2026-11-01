# Typed or promoted attributes experiment

Should frequent attributes (`http.route`, `http.response.status_code`,
`db.system.name`) move out of `attributes MAP(VARCHAR, VARCHAR)` into typed
columns or a shredded `VARIANT`? (otel issue 0003.)

Same data as the sort-order experiment: 2 h of synthetic shop traffic from
`mallard-otel-loadgen --seed 7`, 4.05 M spans, one compacted file. DuckDB 2.0.0,
DuckLake 1.1, 4 threads. 21% of the spans have `http.route`.

Timings are the median of 7 runs, two rounds. The machine was busy with
other builds (load 40-60 on 18 cores), so trust the ratios, not the
milliseconds.

## Size

| variant | size |
|---|---|
| MAP (today) | 129.9 MB |
| MAP + 3 promoted columns | 130.4 MB (+0.4%) |
| VARIANT instead of MAP (shredded) | 129.2 MB |

## Promoted column or VARIANT against the MAP (ms)

The column here reads only the column: it ignores rows written before the
column existed and values in the resource attributes, which the API filter
must not do. See the next table for the full cost.

| query | MAP | VARIANT | column |
|---|---|---|---|
| A trace search `route=/checkout`, 1 h (API query) | 337, 362 | 1185, 1380 | 36, 52 |
| B same, one service | 202, 226 | 357, 486 | 37, 45 |
| C count `status >= 500`, 1 h (SQL only; no API path) | 306, 164 | 976, 691 | 32, 30 |
| D p95 by route, 2 h (SQL only) | 470, 326 | 1957, 1481 | 190, 104 |
| E spans by `db.system.name`, 2 h (SQL only) | 294, 294 | 1797, 2322 | 42, 39 |
| F no attributes: RED by service, 2 h | 206, 191 | 181, 224 | 187, 199 |

## What the API filter would really run (ms)

`attributes=k=v` means "in the span or the resource attributes", on all
rows, also those written before a promoted column existed.

| predicate | runs |
|---|---|
| 1 MAP, today: `attributes[k]=v OR resource_attributes[k]=v` | 224, 428 |
| 2 MAP without the resource part | 166, 527 |
| 3 column only (wrong: drops old rows and resource values) | 30, 76 |
| 4 column OR resource map | 98, 264 |
| 5 column (NULL = absent), map fallback for old rows | 99, 323 |
| 6 column (`''` = absent), map fallback for old rows | 20, 55 |
| 7 as 6, plus resource (**the same answers as today**) | 92, 323 |
| 8 `variant_extract(attributes, k)::VARCHAR` | 627, 1615 |
| 9 `attributes."k"::VARCHAR` | 693, 1338 |
| 10 `variant_extract_string(attributes, k)` | 331, 803 |

## Reading

- A promoted column is 6-11× faster when the query can use only that column.
  With the same answers as today (7) it is 1.3-2.4×: the maps must still be
  read. The larger factor needs a sentinel for "absent" and a change in what
  the filter means (no resource attributes).
- The API has one path that would gain: trace (and log) search with an
  equality filter on exactly a promoted key. There is no numeric filter and
  no attribute group-by on spans in the API; RED and service views (F) don't
  touch attributes. Queries C, D and E are ad-hoc SQL. Metric queries do
  filter and group on attributes, but those tables are small (34,560 points
  next to these 4 M spans) and each query reads one metric through the sort
  key.
- At the measured rate (2 M spans in the hour) that path takes a few hundred
  milliseconds with a MAP. It scales with the rows in the window, so it
  becomes a problem at the top of the target load over long windows, not
  before.
- VARIANT is about 2-8× **slower** than the MAP on attribute queries, although
  the writer does shred it (the Parquet file has `typed_value` columns). It
  is no smaller, and the `duckdb` Rust crate cannot decode the type
  ("decoding Variant columns is not supported"), so every read would need a
  cast.

Decision: keep the MAP. See otel issue 0003 and docs/otel/data-model.md
section 5.

## Reproduce

```sh
# 1. backfill data into a lake (server on non-default ports, no inlining)
MALLARD_STORAGE__CATALOG=$WORK/catalog.ducklake MALLARD_STORAGE__DATA_PATH=$WORK/lake/ \
MALLARD_STORAGE__DATA_INLINING_ROW_LIMIT=0 MALLARD_OTEL__MAINTENANCE__ENABLED=false \
MALLARD_SERVER__OTLP_HTTP_ADDR=127.0.0.1:24318 MALLARD_SERVER__OTLP_GRPC_ADDR=127.0.0.1:24317 \
MALLARD_SERVER__HTTP_ADDR=127.0.0.1:28080 MALLARD_SERVER__API_TOKENS=dev mallard serve &
mallard-otel-loadgen --endpoint http://127.0.0.1:24318 --backfill 2h --rate 50 --batch 200 --concurrency 8 --seed 7
kill -INT %1
# 2. build the variants, then time the queries (replace ${WORK} and ${END})
duckdb < build-variants.sql
duckdb < queries.sql     # .timer on; a DuckDB 2.0 CLI
```

These runs used the workspace's bundled DuckDB 2.0.0 through a throwaway
runner, not the CLI. `http_status_code` and the `db_system_name` column exist
only in variant `p`; variant `q` (predicates 6 and 7) has `http_route` only.
