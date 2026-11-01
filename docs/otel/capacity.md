# Capacity and sizing

**Short answer:** a single instance with **1-2 vCPU and 2 GB RAM** handles
small and mid-sized companies (thousands to tens of thousands of visitors a
day) with a large margin. Ingest is not the bottleneck: a 2-CPU container
ingests about **80,000 spans/s**. What grows with volume is the cost of
queries over long time windows. Queries over the last minutes or hours stay
in the tens of milliseconds.

All numbers below were measured on 2026-11-01 with otel-ducklake 0.1.0,
DuckDB 2.0.0, DuckLake 1.1. Hardware: an Apple M-series laptop (local NVMe
SSD; Docker Desktop VM for container tests). Data came from
[`mallard-otel-loadgen`](../crates/mallard-otel-loadgen): a simulated web shop, 9 services,
4-18 spans per trace, about 0.3 log records per span, metrics every 15 s,
and high-cardinality attributes (`url.full` with ids, `user.id`, request
ids). Scripts: [`bench/capacity`](../bench/capacity). Cloud VMs with network
storage will be slower in absolute terms; the ratios hold.

## What "small/mid" means in spans

| company | visitors/day | page views | spans per page view* | spans/day | peak spans/s** |
|---|---|---|---|---|---|
| small | 1,000 | 10,000 | 30 | 0.3 M | ~20 |
| mid | 10,000 | 150,000 | 50 | 7.5 M | ~500 |
| upper mid | 50,000 | 750,000 | 50 | 37 M | ~2,500 |

\* All backend spans for one page view, including API, DB and cache calls.
\*\* Peak hour ≈ 3× average, plus bursts.

## Ingest

### Steady state, 2 CPU / 2 GB container

`bench/capacity/steady-state.sh 2 2g 1200MB ...`: 60 s per rate, the load
generator on the host, compaction every 2 min. CPU is in % of one core,
sampled every 5 s.

| offered traces/s | spans/s accepted | logs/s | CPU avg | CPU peak | memory peak | throttled |
|---|---|---|---|---|---|---|
| 100 | 1,128 | ~320 | 2% | 7% | 137 MiB | 0 |
| 500 | 5,613 | ~1,600 | 4% | 17% | 211 MiB | 0 |
| 1,500 | 16,883 | ~4,800 | 11% | 48% | 598 MiB | 0 |
| 3,000 | 33,801 | ~9,600 | 21% | 79% | 893 MiB | 0 |
| 6,000 | 67,374 | ~19,000 | 39% | 89% | 843 MiB | 0 |
| 10,000 | **83,241** (saturated) | ~24,000 | 163% | 207% | 1.93 GiB | 152 requests |

At saturation the receivers answer 429. The load generator, like any OTLP
exporter, retries, and nothing was lost (0 failed). Memory at saturation =
DuckDB buffer (1.2 GB) + the full ingest buffer (512 MB) + overhead. That is
why the Helm chart defaults to 768 MB + 256 MB for a 2 GiB pod.

The "upper mid" peak of ~2,500 spans/s uses **about 2-5% of one core**.

### Sustained, native process

3,400 spans/s + 1,000 logs/s for 8 minutes, DuckDB limited to 2 threads and
1 GB, compaction every minute: 0 throttled, 0 flush errors; one file per
table after each compaction. Resident memory rose to ~1.4 GB during the first
compactions (DuckDB filling its 1 GB buffer pool) and then **stayed flat
(1.35-1.43 GB over 13 compaction runs)**: no leak.

### Bulk (backfill), native process, no limits

| run | volume | time | throughput |
|---|---|---|---|
| 2 h of history | 4.0 M spans + 1.2 M logs | 13.7 s | 296k spans/s + 84k logs/s |
| 7 days of history | 68 M spans + 19.4 M logs + 2.9 M points | 407 s | 167k spans/s + 48k logs/s average |

## Storage

| data | rows | Parquet size | per row |
|---|---|---|---|
| spans (7 days, compacted) | 68.0 M | 2.0-2.2 GB | **~30-32 bytes** |
| logs | 19.4 M | 570-640 MB | **~30-33 bytes** |
| metric points | 2.2 M | 8-11 MB | ~5 bytes |
| histogram points (15 buckets) | 726 k | 5-6 MB | ~8 bytes |

So a mid-sized company (7.5 M spans/day) needs about **250 MB/day for
spans**, i.e. ~2 GB for 7 days of retention. Real attributes are usually
more varied than synthetic ones: plan for 2-3×.

## Query latency

The 7-day lake above (68 M spans, 19.4 M logs, compacted), queried over HTTP
with DuckDB limited to **2 threads and 1 GB**. Median of 5 runs;
`bench/capacity/query-latency.sh`.

| endpoint | last 15 min | last 1 h | last 24 h | 7 days |
|---|---|---|---|---|
| services (RED stats) | 16 ms | 27 ms | 529 ms | 4.5 s |
| operations | 17 ms | 27 ms | 370 ms | 2.1 s |
| span series, one service | 10 ms | 9 ms | 45 ms | 247 ms |
| trace search: service + errors | 21 ms | 22 ms | 164 ms | 1.0 s |
| trace search: min duration | 21 ms | 24 ms | 239 ms | 1.5 s |
| trace search: attribute | 36 ms | 52 ms | 833 ms | 5.6 s |
| trace by id | 10 ms | 10 ms | 66 ms | 270 ms |
| log search: text | 11 ms | 15 ms | 151 ms | 968 ms |
| logs: errors of one service | 13 ms | 13 ms | 29 ms | 31 ms |
| log volume series | 5 ms | 6 ms | 36 ms | 217 ms |
| metric list | 10 ms | 10 ms | 12 ms | 27 ms |
| histogram p95 by service | 13 ms | 16 ms | 39 ms | 135 ms |
| counter rate | 9 ms | 9 ms | 11 ms | 20 ms |

How to read it:

- Short windows are fast at any total size: partitions, file statistics and
  the sort order skip everything else.
- Full scans grow linearly. With 2 threads: about **65 ms per million spans**
  for RED stats, **80 ms per million** for attribute filters. More threads
  scale almost linearly.
- 7 days at the "mid" size (≈50 M spans) is ~3 s for the heaviest view. At
  the "upper mid" size, keep heavy dashboards at ≤ 24 h, give DuckDB 4
  threads, or lower `query.max_window`.

## Maintenance cost

- Compaction of one day (9.7 M spans in hundreds of small files) with 2
  threads and a 1 GB DuckDB limit: **29 s**, peak RSS 1.84 GB (DuckDB spills
  to `temp_directory` instead of failing). In normal operation compaction
  runs every 10 min over small files and takes well under a second at these
  volumes.
- Retention drops whole days without rewriting anything.

## Sizing guide

| | small | mid | upper mid |
|---|---|---|---|
| CPU | 0.5 vCPU | 1 vCPU | 2 vCPU |
| memory (container) | 1.5 GiB | 2 GiB | 3-4 GiB |
| `duckdb.memory_limit` | 256MB | 768MB | 1-2.5GB |
| `ingest.max_buffer_bytes` | 128MB | 256MB | 256-512MB |
| disk, 7 days (spans + logs) | < 1 GB | 3-6 GB | 15-30 GB |
| catalog | DuckDB file | DuckDB file | DuckDB file, or PostgreSQL for HA |

Memory rule: **container ≥ `duckdb.memory_limit` + `ingest.max_buffer_bytes`
+ 1 GB.** The 1 GB covers Parquet writers and allocator overhead; the worst
measured case was compaction of a whole unmerged day (0.84 GB above DuckDB's
limit).

When to scale out: not for ingest, which can handle 10-50× these volumes per
replica. Run 2+ replicas with a PostgreSQL catalog and object storage when
you need high availability, or when query load from many users needs more
cores. Every replica can serve queries over all data.

## Reproducing

The numbers above were measured with the standalone `otel-ducklake`
binary, which is retired; this runs the same load against `mallard serve`:

```sh
cargo build --release -p mallard -p mallard-otel-loadgen
docker build -f deploy/docker/Dockerfile -t mallard:dev .
LOADGEN=./target/release/mallard-otel-loadgen bench/otel/capacity/steady-state.sh 2 2g 1200MB 100 500 1500 3000 6000 10000

# query latency: backfill 7 days into a lake, then query it
MALLARD_SERVER__API_TOKENS=dev MALLARD_DUCKDB__THREADS=2 MALLARD_DUCKDB__MEMORY_LIMIT=1GB \
  ./target/release/mallard serve &
./target/release/mallard-otel-loadgen --endpoint http://127.0.0.1:4318 --backfill 7d --rate 10 --batch 500 --concurrency 8
kill %1; wait   # the DuckDB-file catalog allows one process at a time
MALLARD_SERVER__API_TOKENS=dev MALLARD_DUCKDB__THREADS=2 MALLARD_DUCKDB__MEMORY_LIMIT=1GB \
  ./target/release/mallard maintenance --full   # merge all files, as the measured run did
MALLARD_SERVER__API_TOKENS=dev MALLARD_DUCKDB__THREADS=2 MALLARD_DUCKDB__MEMORY_LIMIT=1GB \
  ./target/release/mallard serve &
MALLARD_TOKEN=dev bench/otel/capacity/query-latency.sh http://127.0.0.1:8080/otel <data end, RFC 3339> <a trace id>
```
