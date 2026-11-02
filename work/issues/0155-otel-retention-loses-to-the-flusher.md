---
status: todo
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
