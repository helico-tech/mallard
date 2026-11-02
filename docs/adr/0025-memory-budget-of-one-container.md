# 0025 — The memory budget of one container, and what it takes to keep it

## Status
accepted (by the owner, 2026-11-02)

## Context
Both sides share one process and one `duckdb.memory_limit`. The rule of the old binaries counted one
side, and their numbers were not measured on `mallard`. Measured on 2026-11-01 with both sides at
once ([capacity](../otel/capacity.md#both-sides-in-one-container)):

- The old settings of a 2 GiB container were killed at 145-190k events, at any load above light. Fewer
  threads did not help.
- Compaction of `events` up to the old 64 MB target file ran out of memory at 1 GB with any thread count,
  and at 2 GB with 4 or 8 threads. The answer of issue 0120 is yes. A failed merge also fails the writer's
  batch.
- With one thread 2 GB was enough for one compaction but not the next, and 1 GB never was, so a separate
  maintenance connection with fewer threads would not have fixed this.

Options for compaction (costs in issue 0120): fewer threads for everything; `SET threads` during
compaction; a second DuckDB instance for maintenance; a smaller target file; more memory.

## Decision
- **The rule:** container ≥ `duckdb.memory_limit` + `otel.ingest.max_buffer_bytes` + 0.9 GiB, measured with
  both sides on `mallard`. A setting goes in a default or in the docs only if it was measured this way, with
  `bench/mallard/memory`.
- **`events` merges to 16 MB** instead of 64 MB, the smallest change that made compaction work at 1 GB.
  It costs up to 2x on Sentry queries, which stay near 200 ms at 1M events.
- **Sentry does less at once:** `maintenance.flush_rows` 10000 (was 50000) and `compact_interval` 2m (was 1h).
  Flushing 50k events got a 2 GiB container killed, also at 100 events/s, and merging what piled up in 10 minutes
  at 300 events/s ran out of memory at 1 GB. Costs: more small files between merges, and retention's pruned
  scan on the writer thread every 2 minutes instead of every hour.
- **The image sets `MALLOC_ARENA_MAX=2`.** Peak memory falls 0.5-1.1 GiB. It costs some throughput at heavy
  load, where 4 GiB is the setting to use anyway.
- **Compose runs 2 GiB, 1GB + 128MB, 4 threads.** The thread count is fixed because `threads = 0` follows the
  host, and memory grows with threads.
- The connection stays shared (ADR 0001).

## Consequences
- 2 GiB holds 300 events/s + 2,800 spans/s (1.84 GiB on compose). 4 GiB holds about three times that
  (2GB + 256MB, 3.02 GiB). Above, give it more memory.
- Measure on the shipped intervals, not only shortened ones. Runs with a 30 s flush passed where the
  defaults got killed.
- Changing a table's target file, or adding memory outside DuckDB, means measuring again.
- `events` files already merged stay as they are; only new merges use 16 MB.
- The memory above the limit is not explained (issue 0133). If a better allocator or a fix lowers it, this rule
  can change.
