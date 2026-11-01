---
status: done
type: debt
created: 2026-11-01
---
# 0003. Maintenance runs on the writer thread and pauses ingest

## Problem
Flush, compaction, retention and snapshot expiry run on the single writer
thread. While they run (measured: ~10 s to compact 1M fresh events,
single-threaded under a 2 GB limit) no events are committed; requests wait,
and if the queue (`SDL_QUEUE_CAPACITY`, 20k) fills, clients get 429.

## Notes
- At the target scale (a few events/s, compaction hourly on little new data)
  this pause is short; under sustained high load it is visible as latency
  spikes once an hour.
- Fix sketch: run merge/rewrite/expire on a separate connection and thread;
  DuckLake allows compaction concurrently with inserts into new files (check
  conflict behaviour with inlined-data flushes and retention deletes first).

### What changed (done)
- Measured first: `ducklake_flush_inlined_data`, merge, rewrite, snapshot
  expiry and cleanup do not conflict with concurrent inserts. A `DELETE` that
  removes rows does, both ways, also on other files. Numbers in
  [ADR 0014](../../../docs/adr/0014-sentry-maintenance-thread.md).
- New thread `ducklake-maintenance` (`db/maintenance.rs`) with its own
  connection: flush (by time or rows), merge, rewrite, expiry, cleanup.
- Retention stays on the writer thread (`WriteCmd::Retention`), so it pauses
  ingest for its scan, once a day; the maintenance thread waits for it.
- The writer tries a commit again on a conflict (for other processes).
  New metric `sdl_write_conflicts_total`. `mallard_duck::is_conflict` is now
  shared with otel.
- Shutdown: maintenance stops first (interrupts a merge), then the writer
  drains and does the final flush.
- Test: `db::tests::ingest_continues_during_maintenance` (events keep being
  committed during maintenance, none lost, old day removed, no pause over 5 s).
