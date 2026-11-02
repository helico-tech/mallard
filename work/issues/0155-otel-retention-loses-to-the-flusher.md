---
status: done
type: question
created: 2026-11-02
---
# OTel retention loses against the flusher under bulk ingest

## Problem
Found in issue 0144. OTel's retention runs `DELETE` on its maintenance thread. A `DELETE` that removes rows
conflicts with every insert into that table (ADR 0014), and the flusher inserts all the time. With a 30-day
backfill, so with rows past retention, nearly every run had one of these, on both catalogs, with and without
the fix of issue 0144:

    maintenance failed task=retention
    Transaction conflict - attempting to delete from table with index "3" - but another transaction has inserted into it

Or the flush lost instead (`attempting to insert into table … but another transaction has deleted from it`), and
was retried. It happened in OTLP alone too, so it is not about the Sentry side.

The next try comes after `retention_interval` (1 h). So the question is whether retention can keep losing on a
busy server, so that old data stays.

## How to reproduce
`mallard-otel-loadgen --backfill 30d --rate 5` against `mallard serve`, and look for `maintenance failed` about
30 s after the start.

## Notes
Not measured: rows past retention at paced rates. A backfill at full speed flushes about twice a second, while a
paced server flushes every 5 s, so it may lose rarely or never there. The Sentry side has the same problem solved
by running retention on its writer thread (ADR 0014).

## Answer
Yes, it can keep losing, and at real rates. Not because the server is busy, but because of how much there is to delete.
A `DELETE` loses when a flush commits while it runs. Flushes come every 5 s. A small `DELETE` takes 80 ms and nearly
always wins, but one that takes longer than 5 s never does. 112 M spans took 6.5 s or more. That is under one day at 2,800
spans/s (242 M), so a server from about 1,000 spans/s up may never remove a day: the rows stay, the next day adds more, and
snapshot expiry and file cleanup, which come after the `DELETE`, do not run either.

Fixed: flushes and retention's `DELETE`s take turns ([ADR 0033](../../docs/adr/0033-retention-and-flushes-take-turns.md)),
and the `DELETE` holds the commit lock of ADR 0030. In one process no try was lost after that. Replicas: [issue
0194](0194-retention-on-replicas.md). Numbers: [capacity](../../docs/otel/capacity.md#retention-next-to-the-flusher).

**The logs** of these runs are in a scratch folder of that session, not in the repo.

**How it was measured.** A release build, native, DuckDB-file catalog, local disk, 8 DuckDB threads, 8 GB. Two kinds of
run, each at least twice:

- *Small tries*: retention `1m`, a try every 10 s, so each try has about 10 s of rows. Not what a server does, but many
  tries in little time.
- *One large try*: a lake with 30 days of history (146 M spans, compacted), retention `7d`, a try every 30 s. About 112 M
  spans are past retention, as on the day it matters. The lease of the server that filled it held the first
  tries back, so these runs waited for it to run out.

**What did not help on its own, and was not tried.** A sooner retry cannot help a `DELETE` slower than the flush interval:
every retry loses too. So it was not built. In one process a lost try is now rare.

**The first version of the fix was not enough.** With turns alone 1 of 19 tries at both sides' full speed still lost, on
the snapshot id against the Sentry writer, after 20 s, while the flushes waited for it. So the `DELETE` holds the commit lock
too, and the next 40 such tries lost none.

**Not checked:** a real day of rows (extrapolated: 14 s at medium rates, about a minute at heavy rates, so a buffer may fill
for a moment at heavy rates; the largest measured try was 152 M rows in 11 s); object storage; a `DELETE` under a tight
memory limit in a container (the 1 GB limit was set natively, without a hard limit).
