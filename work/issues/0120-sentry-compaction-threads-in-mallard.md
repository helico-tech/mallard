---
status: done
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

Question: does compaction with all threads go over a 2 GB `duckdb.memory_limit` on
real data?

## Notes
Found during story 0002/11.

### Answer, measured 2026-11-01 on `mallard`
**Yes.** With 8 threads and a 2 GB limit, compaction of 1M fresh events failed in all 4 runs, within a second:

```
compacting events: Out of Memory Error: failed to allocate data of size 32.0 KiB (1.8 GiB/1.8 GiB used)
```

4 threads failed 3 of 3. **Threads are not the whole story:** 1 thread worked once (25 s, as in the old runs)
and then failed twice, and at 1 GB and 768MB it failed every time on 500k events. Numbers:
[Sentry capacity, Memory](../../docs/sentry/capacity.md#memory). Scripts: `bench/mallard/memory/compaction.sh`,
release build in Docker, 4 GiB container (3 GiB at 1 GB and 768MB), 8 CPUs, next to 100 traces/s.

### What was done
The target file of `events` went from 64 MB to 16 MB. With 16 MB, 8 threads and 2 GB: 3 of 3 worked. In the
periodic runs at a 1 GB limit, 12 of 12 worked with 8 threads and 13 of 13 on compose with 4 threads. One run
with 8 threads on compose failed 2 of 13. See
[ADR 0025](../../docs/adr/0025-memory-budget-of-one-container.md).

### Options, with what each costs
| option | adds | costs |
|---|---|---|
| **16 MB target file (done)** | works at 1 GB and at 2 GB with 8 threads; one line | queries up to 2x slower on 1M events (still about 200 ms); more files per day |
| lower `duckdb.threads` for everything | no code | not enough by itself: 1 thread fails at 1 GB; queries slower |
| `SET threads` during compaction (`Host::maintenance_threads`, exists) | no second connection | slows ingest and queries while it runs (ADR 0014); not enough by itself, same as above |
| a separate DuckDB instance for maintenance, with its own limit | memory of compaction apart from the writer, so a failed merge no longer fails ingest | a second buffer pool in the same container, so less memory for each; a second catalog attach; more code; 1 thread still needs over 1 GB at 64 MB |
| more memory | no code | 4 GiB with 64 MB files and 2 GB still failed (run `d2-mod`: killed, 2 of 4 compactions failed) |

Preference: the 16 MB target, as done. A separate instance is not needed now. Reconsider it if a failed merge
still hurts ingest after issue 0132.

Not checked: whether a 16 MB merge fits when a day has far more than 1M events, and files that are already over
16 MB from a heavy burst (these are not merged again, which is why a burst compacts in seconds).
