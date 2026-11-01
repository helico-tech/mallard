# Sort-order experiment

Compares three `SORTED BY` orders for the `spans` table on the same data
(2 h of synthetic shop traffic, 4.04 M spans, fully compacted, DuckDB 2.0.0,
DuckLake 1.1, 4 threads, Apple M-series laptop).

| layout | size |
|---|---|
| a: `start_time` | 107.9 MB |
| b: `service_name, start_time` | 114.6 MB |
| c: `date_trunc('hour', start_time), service_name, start_time` | 114.0 MB |

Median of 5 runs:

| query | a | b | c |
|---|---|---|---|
| RED by service, last 15 min | 29 ms | 33 ms | 27 ms |
| RED by service, 2 h | 115 ms | 91 ms | **76 ms** |
| errors of one service, last 15 min | 14 ms | 13 ms | 17 ms |
| per-minute series of one service, 2 h | 62 ms | **20 ms** | 24 ms |
| trace by id, 2 h | **25 ms** | 36 ms | 41 ms |
| attribute filter, 1 h | 74 ms | 70 ms | **54 ms** |

Decision: layout **c** for `spans` and `logs` (best overall; see
docs/data-model.md). Metric tables use `metric_name, series_id, time` because
metric queries always filter on one metric name.

## Reproduce

```sh
# 1. backfill data into a lake (server on non-default ports, no inlining)
MALLARD_STORAGE__CATALOG=$WORK/bench/catalog.ducklake MALLARD_STORAGE__DATA_PATH=$WORK/bench/lake/ \
MALLARD_STORAGE__DATA_INLINING_ROW_LIMIT=0 MALLARD_OTEL__MAINTENANCE__ENABLED=false \
MALLARD_SERVER__HTTP_ADDR=127.0.0.1:18080 MALLARD_SERVER__OTLP_GRPC_ADDR=127.0.0.1:14317 \
MALLARD_SERVER__OTLP_HTTP_ADDR=127.0.0.1:14318 MALLARD_SERVER__API_TOKENS=bench mallard serve &
odl-loadgen --endpoint http://127.0.0.1:14318 --backfill 2h --rate 50 --batch 200 --concurrency 8 --seed 7
kill -INT %1
# 2. build the three variants (replace ${WORK}), then time the queries
duckdb < build-variants.sql
python3 query-bench.py "$WORK"   # set the duckdb CLI path inside the script
```
