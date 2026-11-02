---
status: done
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

## What changed
The suggestion holds: the process exits and its restart attaches the catalog. Not with `process::exit` in
a writer, which would leave threads inside DuckDB (a crash, issue 0018, ADR 0014) and is a global side effect
in a library. `serve_on` already stops everything and returns the error when a side's `run_background` ends
early, and `mallard serve` then exits 1, so the writers only make that happen:

- `mallard_duck::is_invalidated` recognises DuckDB's message. It is not short memory, even when the message
  quotes that as its cause, so nothing retries it.
- OTel: `is_permanent` is false for it, so no rows are dropped as refused by a table. The flusher stops, pushes
  are refused from then on (429 / `UNAVAILABLE`, which exporters retry, so they reach the restarted server),
  and `run_background` ends with the error.
- Sentry: a failed batch, or the 30 s read of what other processes wrote (for an idle server), ends
  `run_background` the same way.

Attaching the catalog again inside the process (what DuckDB's message suggests) was not chosen: every
connection of both sides, the reader pool and the staging database hang on it, and it would have to be
coordinated with running statements. A restart does that with code that already exists.

What it does not do: acknowledged OTLP rows still in the buffer are lost, up to one `flush_interval` plus
what arrives until the next flush notices, as with a killed process. Reads and maintenance do not detect it,
only the writers. The stop has no deadline ([issue 0152](0152-stopping-has-no-deadline.md)).

Tests use DuckDB's own invalidation (`SET debug_checkpoint_abort`, then `CHECKPOINT` of the catalog), which gives
the error text of this issue; before the fix the flusher dropped the rows. **A real out-of-memory invalidation
was not triggered.** Checked once by hand, with a temporary patch: the real binary logged it, exited 1 under a
second after the next flush, and started fine on the same catalog afterwards. `bench/mallard/memory` and
`deploy/compose/test.sh` were not run, so the restart policy of compose did not act on it.
