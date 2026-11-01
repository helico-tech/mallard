---
status: todo
type: question
created: 2026-11-01
---
# Sentry compaction uses all DuckDB threads inside mallard

## Problem
The capacity runs in `docs/sentry/capacity.md` dropped to 1 DuckDB thread during
Sentry compaction, because sorted merges need memory per thread. That was
`SDL_MAINTENANCE_THREADS` of the old binary. mallard passes
`Host::maintenance_threads = 0`, since `SET threads` is global and the connection
is shared, so compaction runs with `duckdb.threads`. No setting changes that.
This was already so before story 0002/11; the story only removed the unused flag.

## Notes
Question to answer with a measurement: does compaction with all threads go over a
2 GB `duckdb.memory_limit` on real data? If so, options are a separate connection
for maintenance, or a smaller `duckdb.threads`. Found during story 0002/11.
