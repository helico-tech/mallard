---
status: todo
type: bug
created: 2026-11-01
---
# OTel writer drops acknowledged rows on a DuckDB allocation error

## Problem
`store/writer.rs` retries errors whose text matches `TRANSIENT`, which has `Out of Memory`.
DuckDB's message for the same condition does not always have those words. These were
treated as permanent and the rows dropped:

```
flushing appender for spans: Failed to append: failed to allocate data of size 256.0 KiB (731.1 MiB/732.4 MiB used)
opening appender for spans: Failed to get table information from DuckLake: could not allocate block of size 256.0 KiB (732.1 MiB/732.4 MiB used)
```

The memory is short only for a moment, usually while a Sentry compaction runs, so a retry would
very likely work. Measured in run `b-low` (`bench/mallard/memory`, 2026-11-01): one batch of
197 rows dropped; the rest of that flush was written one batch at a time.

## How to reproduce
Run `bench/mallard/memory/run.sh` with a limit Sentry compaction cannot fit, for example
768MB, and grep the log for `dropping a batch`.

## Notes
Probably two more entries in `TRANSIENT` (`failed to allocate`, `could not allocate`) and a
test. `crates/mallard-otel-server` is being changed on another branch, so it is not fixed
here. See also issue 0130, where an invalidated catalog also lands in the permanent branch.
