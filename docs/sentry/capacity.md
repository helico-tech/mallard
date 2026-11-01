# Capacity

**Short answer:** one `sentry-ducklake` process ingests **~4,500-6,000 error
events/s sustained** and answers every API query in **under 60 ms** with 1-3
million events stored (after compaction), within a **2 GB DuckDB memory limit
and 4 threads**. Events take **~265 bytes each** on disk.

A company with tens of thousands of visitors a day sends a few error events
per second at peak (and maybe 1k-100k per day), so this is two to three
orders of magnitude of headroom. The limit you meet first is retention x
storage cost, and that is cheap: 1M events/day for 90 days is about 24 GB of
Parquet.

## Setup

- Machine: Apple M5 Pro (18 cores), 64 GB RAM, local NVMe. Server and load
  generator on the same machine (the generator competes for CPU).
- Build: `--release`, DuckDB 2.0.0, DuckLake 1.1, DuckDB-file catalog, local
  storage, default settings unless noted.
- Load: `mallard-sentry-loadgen` sends realistic browser JS errors: 15 frames, 30
  breadcrumbs, tags, user, request, trace context; ~2.5 KB raw JSON, gzip'd
  to ~1.2 KB on the wire, 5.2 KB after normalization. 200 distinct issues
  with a skewed (Zipf-like) distribution, 5,000 users.
- Every request waits until its event is committed (that is what the server
  guarantees), so throughput depends on how many clients send in parallel.
  Real traffic comes from many independent browsers and servers.

Reproduce (the numbers below were measured with the standalone
`sentry-ducklake` binary, which is retired; this runs the same load against
`mallard serve`):

```sh
cargo build --release -p mallard -p mallard-sentry-loadgen
MALLARD_SERVER__API_TOKENS=dev MALLARD_STORAGE__CATALOG=/tmp/mallard/catalog.ducklake \
MALLARD_STORAGE__DATA_PATH=/tmp/mallard/lake/ MALLARD_DUCKDB__MEMORY_LIMIT=2GB MALLARD_DUCKDB__THREADS=4 \
  ./target/release/mallard serve &
./target/release/mallard-sentry-loadgen --url http://127.0.0.1:8080 --token dev ingest --rate 0 --events 1000000 --concurrency 1024
curl -XPOST -H 'Authorization: Bearer dev' http://127.0.0.1:8080/sentry/v1/maintenance   # flush + compact now
./target/release/mallard-sentry-loadgen --url http://127.0.0.1:8080 --token dev query
```

## Ingest

| Scenario | Throughput | p50 | p95 | p99 |
|---|---|---|---|---|
| 1M events, 1,024 clients, **2 GB / 4 threads** | **6,121/s** | 150 ms | 229 ms | 427 ms |
| saturation, 1,024 clients, no memory limit | 4,578/s | 218 ms | 296 ms | 337 ms |
| paced 1,000/s, 512 clients | 992/s | 171 ms | 303 ms | 354 ms |
| 2.5M events, 512 clients (builds running in parallel) | 3,224/s | 140 ms | 276 ms | 373 ms |
| paced 50/s, 16 clients (quiet server) | — | 524 ms | 535 ms | 537 ms |

- **Bound:** the single writer thread. A commit of a few hundred events takes
  20-80 ms. Above saturation the queue fills and clients get `429` with
  `Retry-After`, which Sentry SDKs honour; nothing is lost silently.
- **Quiet server latency (~0.5 s)** is the batch window: the writer waits up
  to `SDL_BATCH_WINDOW` (500 ms) after the first event so that a quiet server
  makes at most two DuckLake snapshots per second instead of one per event.
  SDKs send in the background, so users never wait for it. Under load,
  batches of 200+ commit immediately.

## Queries

`mallard-sentry-loadgen query`, p50 of 10 runs, one project, 30-day window:

| Query | 755k, not compacted | 755k compacted | 3.26M, not compacted | 3.26M compacted | 1M compacted, 2 GB / 4 threads |
|---|---|---|---|---|---|
| issue list (24h, unresolved) | 143 ms | 59 ms | 474 ms | 51 ms | 45 ms |
| issue list (14d) | 147 ms | 58 ms | 468 ms | 52 ms | 43 ms |
| issue search + sort by count | 146 ms | 58 ms | 441 ms | 47 ms | 35 ms |
| issue detail + tag facets | 282 ms | 126 ms | 761 ms | 48 ms | 40 ms |
| 50 newest events of an issue | 101 ms | 23 ms | 185 ms | 15 ms | 11 ms |
| event detail (with issue hint) | 66 ms | 42 ms | 227 ms | 32 ms | 27 ms |
| event detail (no hint) | 57 ms | 42 ms | 224 ms | 40 ms | 28 ms |
| stats 24h hourly | 122 ms | 45 ms | 397 ms | 24 ms | 16 ms |
| stats 30d daily | 115 ms | 42 ms | 394 ms | 24 ms | 17 ms |
| events by trace id (30d) | 86 ms | 30 ms | 249 ms | 25 ms | 16 ms |

"Not compacted" means right after a burst: thousands of small files (one per
large batch) plus inlined rows. Compaction runs hourly (or on
`POST /v1/maintenance`) and merges them into a few sorted 64 MB files per day;
that is what makes queries 5-15x faster and keeps them flat as data grows.
At normal traffic the data never gets that fragmented.

## Storage

| | |
|---|---|
| Parquet per event | **~265 bytes** (1,000,624 events in 265 MB, 6 files) |
| Raw normalized JSON per event | 5.2 KB (~20x compression: zstd + columnar) |
| Catalog (DuckDB file) | ~14 MB; bounded by `SDL_FLUSH_ROWS` |
| 1M events/day x 90 days | ~24 GB |

## Memory

- The DuckDB memory limit (`SDL_DUCKDB_MEMORY_LIMIT`) covers most but not all
  of DuckDB's memory: with a 2 GB limit, RSS peaked at 3.4 GB during
  compaction and ran at 1.3-2.3 GB otherwise. **Set the limit to about half
  the container's memory.** Without a limit DuckDB may use up to 80% of RAM
  (we saw 13.5 GB RSS on the 64 GB machine).
- Compaction sorts data; sorting on several threads needs more memory than
  2 GB allows, so maintenance temporarily drops to `SDL_MAINTENANCE_THREADS`
  (1). One compaction of 1M fresh events took 10 s.

## Lessons that shaped the defaults

- A fixed batch window before every commit capped throughput at clients /
  window (~280/s for 64 clients). Group commit with an early commit at 200
  events removed that cap.
- Inlined rows sit uncompressed in the catalog until flushed. Flushing only
  every 5 minutes let the catalog grow to 540 MB under load and made commits
  slow during its checkpoints. Flushing every 50k rows keeps it ~14 MB.
- Compaction with large target files and many threads ran out of memory under
  2 GB; 64 MB target files and single-threaded maintenance fixed it.

## Sizing guidance

| Traffic | Events/day | Container | `SDL_DUCKDB_MEMORY_LIMIT` | Storage (90 days) |
|---|---|---|---|---|
| ~10k visitors/day | 1k-20k | 0.5 CPU, 1-2 GB | 512MB-1GB | < 1 GB |
| ~100k visitors/day | 10k-200k | 1 CPU, 4 GB | 2GB | 1-5 GB |
| bigger, or error storms | 1M+ | 2 CPU, 4-8 GB | 2-4GB | ~24 GB per 1M/day |

Error volume depends far more on code quality than on visitors; the
`sdl_events_stored_total` metric tells you the real rate.

## Not measured

- Remote object storage (S3): flushes, compaction and cold queries pay
  network round trips. Inlined writes only touch the catalog.
- PostgreSQL as catalog under load (functionally verified, not benchmarked).
- Linux/x86, multi-day soak tests.
