# Capacity and sizing

Except for [Both sides in one container](#both-sides-in-one-container), these numbers
were measured on the old standalone `otel-ducklake` binary, which is gone. They were not
measured again on `mallard serve`, where the same code shares its DuckDB with the Sentry
side. They count telemetry alone.

**Short answer:** a 2 GiB container runs both sides for small and mid-sized companies, with
`duckdb.memory_limit = "1GB"`, a `128MB` buffer and 4 DuckDB threads. Measured on `mallard`, this
holds 300 events/s plus 2,800 spans/s (peak 1.84 GiB). For more, give it 4 GiB. Telemetry ingest
alone is not the bottleneck: a 2-CPU container
ingests about **80,000 spans/s**. What grows with volume is the cost of
queries over long time windows. Queries over the last minutes or hours stay
in the tens of milliseconds.

All numbers below were measured on 2026-11-01 with otel-ducklake 0.1.0,
DuckDB 2.0.0, DuckLake 1.1. Hardware: an Apple M-series laptop (local NVMe
SSD; Docker Desktop VM for container tests). Data came from
[`mallard-otel-loadgen`](../../crates/mallard-otel-loadgen): a simulated web shop, 9 services,
4-18 spans per trace, about 0.3 log records per span, metrics every 15 s,
and high-cardinality attributes (`url.full` with ids, `user.id`, request
ids). Scripts: [`bench/otel/capacity`](../../bench/otel/capacity). Cloud VMs with network
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

`bench/otel/capacity/steady-state.sh 2 2g 1200MB ...`: 60 s per rate, the load
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
DuckDB buffer (1.2 GB) + the full ingest buffer (512 MB) + overhead. This was telemetry
alone; with both sides see [Both sides in one container](#both-sides-in-one-container).

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
`bench/otel/capacity/query-latency.sh`.

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
  threads, or lower `otel.query.max_window`.

## Maintenance cost

- Compaction of one day (9.7 M spans in hundreds of small files) with 2
  threads and a 1 GB DuckDB limit: **29 s**, peak RSS 1.84 GB (DuckDB spills
  to `duckdb.temp_directory` instead of failing). In normal operation compaction
  runs every 10 min over small files and takes well under a second at these
  volumes.
- Retention drops whole days without rewriting anything.

## Sizing guide

| | small | mid | upper mid |
|---|---|---|---|
| CPU | 0.5 vCPU | 1 vCPU | 2 vCPU |
| memory (container) | 1.5 GiB | 2 GiB | 4 GiB |
| `duckdb.memory_limit` | 256MB | 1GB | 2GB |
| `otel.ingest.max_buffer_bytes` | 128MB | 128MB | 256MB |
| `duckdb.threads` | | 4 | 8 |
| disk, 7 days (spans + logs) | < 1 GB | 3-6 GB | 15-30 GB |
| catalog | DuckDB file | DuckDB file | DuckDB file, or PostgreSQL for HA |

The 2 GiB and 4 GiB columns were measured with both sides on `mallard` (below). The 1.5 GiB
column is an estimate from telemetry alone.

Memory rule, counting both sides: **container ≥ `duckdb.memory_limit` +
`otel.ingest.max_buffer_bytes` + 0.9 GiB**, which leaves about half the container for DuckDB. The 0.9 GiB
covers Parquet writers, both sides' queues and allocator overhead: the most measured with today's defaults was
0.92 GiB, in the 4 GiB run below. It needs the image's `MALLOC_ARENA_MAX=2`. History: on the old binary,
telemetry alone, this was 1 GB, with the worst case in compaction of a whole unmerged day (0.84 GB above
DuckDB's limit).

When to scale out: not for ingest, which can handle 10-50× these volumes per
replica. Run 2+ replicas with a PostgreSQL catalog and object storage when
you need high availability, or when query load from many users needs more
cores. Every replica can serve queries over all data.

## Both sides in one container

Measured on `mallard` on 2026-11-01. A release build in a Docker container with a hard memory limit
(`--memory` = `--memory-swap`) on an arm64 Mac; the Sentry and OTel load generators ran at once on
the host, with a small query load. The DuckDB-file catalog and local Parquet files unless it says compose
(Postgres catalog + S3, `deploy/compose`). The machine was also building other code, so treat throughput
as rough. Scripts: [`bench/mallard/memory`](../../bench/mallard/memory/README.md).

Loads: **light** 20 events/s + 560 spans/s; **medium** 300 events/s + 2,800 spans/s (about the "upper
mid" peak above); **heavy** 1,000 events/s + 11,200 spans/s. Peak is the process's peak RSS. "Compactions"
is Sentry ok/failed; OTel is noted if any failed. Limits are decimal: 1GB is 0.93 GiB. "Not recorded": the
server was dead before its counters could be read.

Today's image and defaults, nothing overridden but what the table says:

| container | limit + buffer | threads | load | ran | events + OTel rows stored | peak | killed | DuckDB out-of-memory lines | compactions | events rejected |
|---|---|---|---|---|---|---|---|---|---|---|
| 2 GiB, compose | **1GB + 128MB** | **4** | medium | 31 min | 563k + 6.5M | 1.84 GiB | no | 0 | 15/0 | 0 |
| 2 GiB | 1GB + 128MB | 4 | medium | 31 min | 563k + 6.5M | 1.90 GiB | no | 0 | 15/0 | 0 |
| 4 GiB | **2GB + 256MB** | **8** | heavy | 31 min | 1.8M + 26M | 3.02 GiB | no | 0 | 15/0 | 0 |
| 2 GiB | 1GB + 128MB | 4 | all both sides accept | 11 min | 840k + 0.5M | 1.86 GiB | no | 413 | **0/5** | 404 |

In the 4 GiB run OTLP also got 4,852 throttled requests and 184 failed at the load generator, but it reached
the offered 11,200 spans/s and dropped no rows. At full speed OTLP is held back hard and compaction does not
keep up, but the 2 GiB container stayed up for the 10 minutes tested.

How the defaults were found. All of these ran with `MALLOC_ARENA_MAX=2` and 16 MB target files:

| container | limit + buffer | threads | `flush_rows` | `compact_interval` | load | ran | peak | killed | compactions |
|---|---|---|---|---|---|---|---|---|---|
| 2 GiB | 1GB + 128MB | 4 | 50k (old) | 1 h (old) | medium | 10 min, 176k events | 1.99 GiB | **yes** | - |
| 2 GiB | 1GB + 128MB | 4 | 50k (old) | 1 h (old) | 100 events/s + 1,000 spans/s | 35 min, 210k events | 1.98 GiB | **yes** | - |
| 2 GiB | 1GB + 128MB | 4 | 50k (old) | 1 h (old) | light | 76 min | 1.44 GiB | no | 1/0 |
| 2 GiB | 1GB + 128MB | 4 | 10k | 1 h (old) | medium | 76 min, 1.35M events | 1.94 GiB | no | **0/1** |
| 2 GiB | 1GB + 128MB | 4 | 10k | 10 min | medium | 22 min | 1.84 GiB | no | **0/2** |
| 2 GiB, compose | 1GB + 128MB | 4 | 10k | 10 min | medium | 19 min | 2.02 GiB | **yes** | 0/1 |
| 4 GiB | 2GB + 256MB | 8 | 50k (old) | 1 h (old) | heavy | 58 min, 3.1M events | 4.01 GiB | **yes** | - |
| 4 GiB | 2GB + 256MB | 8 | 10k | 1 h (old) | heavy | 76 min, 4.1M events | 3.21 GiB | no | **0/1** |
| 4 GiB | 2GB + 256MB | 8 | 10k | 10 min | heavy | 22 min | 3.06 GiB | no | **0/2** |

Before the changes (image without `MALLOC_ARENA_MAX`, 64 MB target files), on short intervals:

| container | limit + buffer | threads | load | ran | peak | killed | DuckDB out-of-memory lines | compactions | events rejected |
|---|---|---|---|---|---|---|---|---|---|
| 2 GiB | 1GB + 512MB | 8 | heavy | 2.5 min | 1.86 GiB | **yes** | 0 | 1/0 | not recorded |
| 2 GiB | 1GB + 512MB | 8 | medium | 8 min | 1.92 GiB | **yes** | 0 | 3/0 | not recorded |
| 2 GiB | 1GB + 512MB | 8 | light, fresh lake | 21 min | 1.48 GiB | no | 0 | 10/0 | 0 |
| 2 GiB | 1GB + 512MB | 8 | light, on 500k unmerged events | 21 min | 1.99 GiB | no | 10 | **0/10** | 0 |
| 2 GiB | 768MB + 256MB | 8 | heavy | 3 min | 2.01 GiB | **yes** | 210 | 0/1 | not recorded |
| 2 GiB | 768MB + 256MB | 8 | medium | 10 min | 1.97 GiB | **yes** | 4 | 2/2 | not recorded |
| 2 GiB | 768MB + 256MB | 8 | medium, no queries | 26 min | 1.95 GiB | no | 212 | **2/10** | 191 |
| 2 GiB | 768MB + 256MB | 8 | light, fresh lake | 21 min | 1.28 GiB | no | 0 | 10/0 | 0 |
| 2 GiB | 768MB + 256MB | 8 | light, on 500k unmerged events | 21 min | 1.85 GiB | no | 11 | **0/10** | 0 |
| 2 GiB | 768MB + 256MB | 2 | medium | 8 min | 2.00 GiB | **yes** | 202 | 2/1 | not recorded |
| 2 GiB | 768MB + 256MB | 4 | heavy | 3 min | 1.91 GiB | **yes** | 1 | 0/1 | not recorded |
| 4 GiB | 1GB + 512MB | 8 | heavy | 16 min | 3.84 GiB | no | 1226 | **1/7** | 1124 |
| 4 GiB | 2GB + 512MB | 8 | heavy | 9 min | 4.01 GiB | **yes** | 264 | 2/2 | not recorded |

With `MALLOC_ARENA_MAX=2` and 16 MB target files, on short intervals (flush every 30 s, compact every 2 min):

| container | limit + buffer | threads | load | ran | peak | killed | DuckDB out-of-memory lines | compactions | events rejected |
|---|---|---|---|---|---|---|---|---|---|
| 2 GiB, compose | 1GB + 128MB | 4 | medium | 26 min | 1.94 GiB | no | 0 | 13/0 | 0 |
| 2 GiB, compose | 1GB + 128MB | 8 | medium | 26 min | 1.99 GiB | no | 194 | 11/2 (OTel 1 failed) | 187 |
| 2 GiB, compose | 1GB + 128MB | 8 | light | 16 min | 1.15 GiB | no | 0 | 8/0 | 0 |
| 2 GiB, compose | 1200MB + 128MB | 8 | medium | 15 min | 1.99 GiB | **yes** | 0 | not recorded | not recorded |
| 2 GiB | 1GB + 128MB | 8 | medium | 26 min | 1.81 GiB | no | 0 | 12/0 | 0 |
| 2 GiB | 1GB + 128MB | 8 | light, on a lake with 500k events | 21 min | 1.33 GiB | no | 0 | 10/0 | 0 |
| 2 GiB | 1200MB + 128MB | 8 | medium | 26 min | 1.82 GiB | no | 0 | 13/0 | 0 |
| 2 GiB | 1200MB + 128MB | 4 | medium | 26 min | 1.99 GiB | no | 0 | 12/0 | 0 |
| 2 GiB | 1200MB + 128MB | 18 | medium | 26 min | 2.04 GiB | no | 1 | 12/0 (OTel 1 failed) | 136 |
| 2 GiB | 768MB + 256MB | 8 | medium | 26 min | 1.55 GiB | no | 656 | **4/8** | 890 |
| 2 GiB | 1200MB + 128MB | 8 | heavy | 7 min | 1.99 GiB | **yes** | 0 | 3/0 | not recorded |
| 2 GiB | 768MB + 256MB | 8 | heavy | 6 min | 1.99 GiB | **yes** | 1 | 1/1 | not recorded |
| 2 GiB | 1GB + 128MB | 4 | all both sides accept | 6 min | 1.99 GiB | **yes** | 259 | 0/2 | not recorded |
| 4 GiB | 2GB + 256MB | 8 | heavy | 16 min | 3.00 GiB | no | 0 | 8/0 | 0 |

What the runs show:

- **The old settings did not hold.** With the old image and defaults a 2 GiB container running both sides was
  killed at 145-190k events at medium or heavy load, with limits of 768MB and 1GB and 2, 4 or 8
  threads. Fewer threads did not help. A fresh lake at light load holds, but those runs reached only
  27k events. One side alone survived 10 min at heavy load, at up to 1.77 GiB (the Sentry side without queries).
- **Memory outside DuckDB's limit decides it.** `MALLOC_ARENA_MAX=2` lowers peak RSS by 0.5-1.1 GiB,
  so the image sets it. It costs throughput at heavy load: 1,669 throttled OTLP requests against 0
  (exporters retry). A limit above half the container gets killed: a 4 GiB container with 2 GB + 512 MB
  was killed without it, and even with `MALLOC_ARENA_MAX`, 1200MB in 2 GiB left too little room on
  compose (killed at 280k events).
- **Sentry compaction decides the limit.** The merge up to the old 64 MB target file does not fit 1 GB at
  any thread count, nor 2 GB with 4 or 8 threads. Once it fails, a backlog stays and it keeps failing. A
  failed merge also fails the writer's batch, about 250 events answered 503
  ([issue 0132](../../work/issues/0132-sentry-writer-fails-on-short-memory.md)). `events` now merges to 16
  MB; see the [Sentry numbers](../sentry/capacity.md#memory). 768MB is still too little.
- **How much Sentry does at once decides too.** Flushing 50k inlined events at once got the 2 GiB container
  killed, even at 100 events/s; 10k did not. Merging what piled up in 10 minutes or an hour at 300 events/s ran
  out of memory at 1 GB; every 2 minutes it did not. So `sentry.maintenance.flush_rows` is now 10000 and
  `compact_interval` 2m. At heavy traffic this keeps each step small; at light traffic a run has little to do.
- **Overload is not fully safe.** With both sides at full speed today's defaults stayed up for the 10 minutes
  tested, at 1.86 GiB, but with a flush every 30 s this setting was killed after 6 minutes, and the old image
  within 0.5-2.5 minutes. Longer runs at full speed were not tested.
- **More threads, more memory:** the same setting peaked at 1.82 GiB with 8 threads and 2.04 GiB with
  18. `threads = 0` means every core of the host, so a 2 GiB container on a big host needs a fixed number:
  compose sets 4. A container CPU limit (`--cpus`) lowers the default too; `--cpuset-cpus` does not.
- After an out-of-memory error in a DuckDB-file catalog's checkpoint, the catalog stays dead until a restart
  ([issue 0130](../../work/issues/0130-catalog-invalidated-after-out-of-memory.md)). It happened once, at 768MB.

The memory above the limit is not explained
([issue 0133](../../work/issues/0133-rss-spikes-above-duckdb-limit.md)), and the 2 GiB margin is small, so
watch the container's memory.

Not measured: x86, runs longer than 76 minutes, days of data, OTel's 6-hour full compaction at its default
interval (5-minute runs were fine), many queries at once, OTLP/gRPC (a gRPC request reserves
`max_request_bytes` from the budget, so a 128MB buffer allows 4 at once), an S3 store over the network
(compose ran RustFS on the same machine). Compressed OTLP/HTTP at 2,800 spans/s was checked for 2 minutes
with a 128MB buffer: 0 throttled. Each setting ran once.

## Reproducing

This runs the same load against `mallard serve`:

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
