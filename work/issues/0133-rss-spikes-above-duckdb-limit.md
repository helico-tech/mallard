---
status: done
type: question
created: 2026-11-01
---
# What uses the memory above DuckDB's limit?

## Problem
Process memory goes 0.6-1 GiB above `duckdb.memory_limit` + `otel.ingest.max_buffer_bytes`, and jumps
by 0.5 GiB within 2 seconds. Most out-of-memory kills in the runs of
[issue 0123](0123-chart-memory-defaults-below-the-rule.md) came from such a jump, without a DuckDB
error. The cause was not found. What the runs show:

- Most of it sits in glibc's heaps. `MALLOC_ARENA_MAX=2` lowered peak RSS by 0.5-1.1 GiB (run `b-low`
  1.97 GiB and killed, `e8-low` 1.44 GiB; `d-mod` 3.84 GiB, `d3-mod` 2.70 GiB).
- Large Sentry flushes matter: `flush_rows` 50000 got a 2 GiB container killed at 100 events/s, 10000 did
  not (`dd-100`, `dd-f10`). How a flush of 260 MB of rows turns into a 0.5 GiB jump is not known.
- Kills also came within a few seconds of a query round, but not always. Each query alone added less
  than 60 MB on a server with 250k events, and a run without queries (`b-low-noq`) still reached 1.95 GiB.
- The peak rises with the limit: a 4 GiB container with a 2 GB limit reached 4.01 GiB and was killed
  (`d2-mod`).

## Notes
Next step: a heap profile of one jump. Also worth a try: jemalloc in the build instead of
`MALLOC_ARENA_MAX`, since `MALLOC_ARENA_MAX=2` costs some throughput at heavy load
(`d-mod` 0 throttled requests, `d3-mod` 1669).

## Plan (2026-11-03)
A session of 2026-11-02 measured this but was stopped before it reported, and its logs are gone; only its transcript is left.
It points at a second allocator: the DuckLake extension has its own jemalloc, with about 300 MiB of dirty pages.

1. Repeat the comparison on `main` 18cc063 with `bench/mallard/memory/run.sh`: 2 GiB, 1GB + 128MB, 4 threads, 300 events/s +
   2,800 spans/s, 30 minutes, two rounds of four. Shipped settings against `DUCKDB_JE_MALLOC_CONF=dirty_decay_ms:0,muzzy_decay_ms:0`,
   each with the shipped `flush_rows` and with 50000.
2. Write down the answer, with what was seen once and what twice.
3. If the setting holds, propose it for the image. It goes on a branch: the owner decides.

## Answer (2026-11-03)
Three things sit above or next to the limit, and two of them are allocators keeping freed memory:

- **The DuckLake extension's own jemalloc.** The extension links its own copy, so its freed pages are in the process's memory and
  outside `duckdb.memory_limit`: 330 MiB resident, of which about 290 MiB dirty, and 52 MiB live (one instrumented run, `p5-gl`).
- **glibc's heaps.** After a merge they held about 950 MiB free of 1,190 MiB, partly returned to the system (the same run).
- **The jumps are mostly DuckDB's pool filling up, inside its limit.** In that run's jump a Sentry compaction took the pool from
  470 to 953 MiB within a second (`ORDER_BY` 353, `COLUMN_DATA` 344), and RSS went from 990 to 1,484 MiB. The room for such a
  jump is what the two allocators above had taken. This is one jump in one run.

**Fix, in the image:** `DUCKDB_JE_MALLOC_CONF=dirty_decay_ms:0,muzzy_decay_ms:0`. Numbers:
[capacity](../../docs/otel/capacity.md#the-ducklake-extensions-own-allocator). Twice at 2 GiB on `main`: 0.15-0.25 GiB lower at medium load, and
`flush_rows` 50000 went from killed to finished. Once at 4 GiB heavy: 0.12 GiB lower. No throughput cost.

**jemalloc for the whole process does not help** (one run each, on 625bf66, 2 GiB medium): defaults killed at 8 min, tuned killed at
11 min, tuned plus the setting above 1.89 GiB. So `MALLOC_ARENA_MAX=2` stays.

**Not answered:**
- What a flush of 50,000 rows allocates. Both kills came when about 50,000 events had come in (300/s for 2.7 min), so the flush is the likely
  trigger, from the timing only. Memory rose by up to 1.1 GiB within a second, too fast for samples once a second. The default of 10000
  avoids it, so no follow-up unless someone needs a larger flush.
- Heavy load on 2 GiB, above what ADR 0025 supports. On 625bf66 one run with the setting ended in the invalidated catalog of
  [issue 0170](0170-stop-after-invalidated-catalog-hangs.md), and one without had 8 of 15 compactions fail but kept going. One run each
  says little. Its pool was full in both, and the setting works outside the pool.
- The compose stack (Postgres + RustFS), and amd64: only checked that its extension has jemalloc too.

**Where this comes from.** The instrumented numbers (`p5-gl`, the whole-process jemalloc runs, 625bf66) come from a session of 2026-11-02
that was stopped before it reported. Its logs and its probe (an `LD_PRELOAD` library reading allocator statistics) are gone; those numbers
are from its transcript and cannot be checked again. The paired runs ran on 18cc063 with the bench script as it is. Another session's light
tests ran next to some of them (round 2 and the 4 GiB pair), with machine load at 3-5 on 18 CPUs and both sides of a pair at the same time.
Other projects' containers ran too.
