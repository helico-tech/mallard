# 0014 — Sentry maintenance runs next to the writer, retention stays on it

## Status
accepted

## Context
Flush, compaction, retention and snapshot expiry ran on the single Sentry
writer thread. While they ran (about 10 s to compact 1M fresh events) no event
was committed; requests waited, and a full queue meant 429 (issue
`work/sentry/issues/0003`). DuckLake's docs do not say which of these may
overlap with inserts, so we measured it: one connection committing small
batches all the time, one running each step in a loop, on one DuckDB instance
and on two instances sharing a Postgres catalog.

| step, next to inserts into the same table | result |
|---|---|
| `ducklake_flush_inlined_data` | no conflict |
| `ducklake_merge_adjacent_files` | no conflict |
| `ducklake_rewrite_data_files` | no conflict |
| `ducklake_expire_snapshots`, `ducklake_cleanup_old_files` | no conflict |
| `DELETE` that removes no rows | no conflict |
| `DELETE` that removes rows | conflicts both ways, also when the rows are in other files: "attempting to insert into table … another transaction has deleted from it", and the reverse |

No acknowledged row was lost or duplicated in any run. A merge and flush of
1M rows next to a writer: median commit 8 ms, slowest 79 ms.

## Decision
- A second thread (`ducklake-maintenance`, `db/maintenance.rs`) with its own
  connection runs flush, merge, rewrite, snapshot expiry and file cleanup.
- Retention stays on the writer thread (`WriteCmd::Retention`): on another
  thread its `DELETE` would lose against every commit under load, or fail the
  batches. It removes rows once a day, by whole daily files; the other runs
  find nothing and cost one pruned scan. The maintenance thread waits for it
  before merging, because a merge and a delete of the same files conflict.
- Every lake write of the writer is tried again on a conflict (7 times, 20 ms
  doubling, then the batch fails). Inside one process only retention could
  cause one and it runs on the same thread; the retries are for other
  processes (ADR 0021).
- `POST /v1/maintenance` still goes through the writer's queue, so what was
  queued before it is committed first; the writer only hands it on.
- On shutdown maintenance stops first, interrupting a running merge (rolled
  back, nothing lost); then the writer drains and does the final flush.

## Consequences
- Compaction and flushes no longer pause ingest. Retention still does, for
  the length of its scan, once a day.
- Rule change: rows are still written only by the writer thread; maintenance
  rewrites files and never adds or removes rows.
- `maintenance_threads` (`SET threads`, standalone only) is global, so it now
  also slows the writer while compaction runs. mallard leaves it at 0.
- This depends on DuckLake's conflict rules. A DuckDB upgrade that changes
  them shows up in `db::tests::ingest_continues_during_maintenance`.
