---
status: todo
type: question
created: 2026-11-02
---
# Replicas on one catalog still race on the snapshot id

## Problem
Issue 0144 found that a large commit loses against a stream of small ones, and fixed it with a lock inside one
process (ADR 0030). Replicas on one Postgres catalog do not share that lock. So an OTLP flush on one replica may
still lose against a Sentry writer at full speed on another, for the 8-11 s until DuckLake gives up. It holds its
replica's lock while it retries, so the Sentry writer of that replica waits too. Before the lock it did not.

## How to reproduce
Not tried. Two `mallard serve` on one Postgres catalog, with OTLP on one and Sentry at full speed on the other.

## Notes
In one process, Postgres catalog, before the lock: 105,845-116,123 spans/s next to 5,493-5,512 events/s, against
270,591-306,042 next to 2,696-3,470 with it. So two replicas may do better than one did on a DuckDB file, but
this was not measured.

If it matters, the options to weigh are: fewer Sentry commits at full speed (a larger batch), or a shorter wait in
the flusher's retry. A shorter wait alone did not help in one process (ADR 0030).
