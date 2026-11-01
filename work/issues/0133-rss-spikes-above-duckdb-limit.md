---
status: todo
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
- Kills came within a few seconds of a query round or a Sentry flush, but not always the same one.
  Each query alone added less than 60 MB on a server with 250k events, and a run without queries
  (`b-low-noq`) still reached 1.95 GiB.
- The peak rises with the limit: a 4 GiB container with a 2 GB limit reached 4.01 GiB and was killed
  (`d2-mod`).

## Notes
Next step: a heap profile of one jump. Also worth a try: jemalloc in the build instead of
`MALLOC_ARENA_MAX`, since `MALLOC_ARENA_MAX=2` costs some throughput at heavy load
(`d-mod` 0 throttled requests, `d3-mod` 1669).
