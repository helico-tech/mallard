---
status: todo
type: bug
created: 2026-11-01
---
# An out-of-memory error in a catalog checkpoint leaves the server running but dead

## Problem
With the DuckDB-file catalog, an out-of-memory error during a checkpoint of the catalog
invalidates that database for the whole process. mallard does not notice. It keeps
running and accepting data:

- Sentry answers 503 to every event until someone restarts it.
- OTLP still answers 200, and the writer drops what it accepted, because the error counts
  as permanent (`dropping a batch that cannot be written to this table`).
- `/readyz` answers 503, `/healthz` answers 200, so a liveness probe never restarts it.

Measured on 2026-11-01 (`bench/mallard/memory`, run `e2-mod`: 768MB limit, 256MB buffer,
2 threads, `MALLOC_ARENA_MAX=2`, 2 GiB container, 1000 events/s and 11k spans/s). It
happened 5 minutes in; in the 3 minutes until the run was stopped OTel dropped 4,677,057
rows it had acknowledged and Sentry rejected 303,735 events. The first error:

```
IO Error: Failed to query most recent snapshot for DuckLake: Database "__ducklake_metadata_lake"
has been invalidated because checkpointing failed. Detach and reattach it before using it
again. Original error: failed to allocate data of size 6.0 MiB (751.6 MiB/732.4 MiB used)
```

## How to reproduce
Run `bench/mallard/memory/run.sh` with a low limit until the log shows `has been invalidated`.
It appeared in 1 of 56 runs, so it is rare; a 768MB limit under heavy load is the
way to get it.

## Notes
Seen only with the DuckDB-file catalog. A Postgres catalog has no such checkpoint, but that was
not tested for long enough to rule out a similar failure. The simplest fix is probably to exit the
process on this error, so the restart policy reattaches the catalog. Lives in
`mallard-duck` and the writers of both sides; not fixed here because another branch is
changing those crates.
