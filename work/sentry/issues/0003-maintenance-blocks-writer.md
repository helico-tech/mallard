---
status: todo
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
