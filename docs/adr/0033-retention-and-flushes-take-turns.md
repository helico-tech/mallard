# 0033 — OTel retention and flushes take turns

## Status
proposed (the owner decides when merging). Amends ADR 0030: retention's `DELETE` takes the commit lock.

## Context
A `DELETE` that removes rows conflicts with every insert into its table that commits while it runs (ADR 0014). OTel
ran retention on its maintenance thread, next to the flusher. So whoever committed last lost. A flush that lost was
tried again. A `DELETE` that lost waited for the next `retention_interval` (1 h), and skipped snapshot expiry and file
cleanup too.

Whether that matters depends on how long the `DELETE` takes against the time between two flushes (5 s, shorter at
full speed). It scans the rows past retention, about 17 M rows/s here:

- A small `DELETE` (60-100 ms) nearly always wins: 234 of 235 tries at paced rates.
- A `DELETE` slower than 5 s never wins, and the same rows wait for it again an hour later, with a day more of them.
  112 M spans took 6.5 s or more, and lost all 24 tries at paced rates. That is under one day at 2,800 spans/s
  (242 M), so from about 1,000 spans/s (86 M a day, 5 s) a server may never remove anything, and it gets worse each day.

Options, for one process:

- **Try again sooner** after a conflict: cheap, but no help for a `DELETE` that is slower than the flush interval. It
  loses every try.
- **Run the `DELETE` on the flusher thread**, as the Sentry side does on its writer (ADR 0014): works, but the flusher
  needs a queue of commands and a way to answer them.
- **A lock that flushes and `DELETE`s take**: the same effect as the Sentry side's pattern, in two lines per side.
- Delete in small slices, so each one is shorter than the time between flushes: no pause in the flushes, and it would
  help on replicas too. But slices smaller than a day write delete files, where a whole day now drops whole files
  (`maintenance.rs`). More code, more snapshots, and at full speed a slice still loses. Not tried.

## Decision
`store::Locks::turn`: the flusher holds it for a whole flush, from the start of its transaction to the commit, and
retention for each `DELETE`. Neither can then commit inside the other's transaction. It takes turns strictly (two
mutexes): with a plain mutex a flusher with a full buffer takes the lock again before the `DELETE` wakes up.

The lock is per table, so flushes run between the `DELETE`s of two tables. Compaction, snapshot expiry and file cleanup
do not take it: they do not conflict with inserts (ADR 0014).

Retention's `DELETE` also holds the commit lock around its `COMMIT`, unlike what ADR 0030 says of maintenance. At both
sides' full speed it lost on the snapshot id too (1 of 19 tries, after 20 s, with the flushes waiting for it).

No setting: there is nothing to choose. The next try still comes after `retention_interval`: in one process a lost try
is now rare, so it needs no sooner retry.

## Consequences
- Flushes wait while a `DELETE` runs, as Sentry's writer does (ADR 0014), and the buffer takes what comes in. Most tries
  find nothing to delete and hold the lock for milliseconds. Once a day, after midnight UTC, a try removes a day. That
  grows with the rows: linear from 17 M rows/s, 14 s at medium rates and about a minute at heavy rates. If the buffer
  fills meanwhile, OTLP senders get a 429 and try again; nothing is lost. See
  [capacity](../otel/capacity.md#retention-next-to-the-flusher).
- A new writer into OTel's tables must take the turn, or retention loses against it again.
- Replicas do not share these locks. A `DELETE` on one still conflicts with the flushes of the others
  ([issue 0194](../../work/issues/0194-retention-on-replicas.md)).
- Measured on a DuckDB-file catalog only. Not on SQLite or Postgres: on SQLite, Sentry's flush under the commit lock failed
  with `database is locked` while another writer waited ([issue 0193](../../work/issues/0193-red-ci-after-0192.md)), so look there first if this misbehaves.
