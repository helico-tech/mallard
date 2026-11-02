---
status: done
type: question
created: 2026-11-01
---
# OTLP ingest is much slower next to saturated Sentry ingest

## Problem
Seen while filling a lake for story 0004/01 (`mallard serve`, 10 DuckDB threads, DuckDB-file
catalog, local disk): `mallard-otel-loadgen --backfill` reached 15,000-90,000 spans/s while
`mallard-sentry-loadgen ingest --rate 0 --concurrency 512` ran at 3,000-4,000 events/s, and
300,000-385,000 spans/s as soon as that finished. The server log has 55 of

    flush conflicted with another transaction; will retry
    Failed to flush changes into DuckLake: Duplicate key "snapshot_id: N" violates primary key constraint

The flushes retried and `flush_errors` stayed 0, but the buffer filled: 5,843 throttled and 71
failed requests at the load generator. Their cause was not looked into.

Paced traffic does not show it: 2,242 spans/s with 121 events/s had nothing throttled.

## How to reproduce
Run both load generators at once, as above, and compare with OTLP alone.

## Notes
Not measured with a Postgres catalog. Each side commits on its own, so this may be the price of
one catalog (ADR 0001), or a retry that waits too long. Worth knowing before
`docs/otel/capacity.md`, whose numbers come from the old binary without a Sentry side, is
measured again.

## Answer
A bug, not the price of one catalog and not mainly the retry's wait. A large commit starved next to a stream of
small ones. Fixed with one lake commit at a time on a DuckDB instance
([ADR 0030](../../docs/adr/0030-one-lake-commit-at-a-time.md)). What is left at full speed is the real price of one
catalog: on a DuckDB file OTLP loses about 20% and Sentry about 24% against running alone. Numbers:
[capacity](../../docs/otel/capacity.md#both-sides-at-full-speed). The logs of these runs are in a scratch folder of
that session, not in the repo.

**The cause.** Every DuckLake commit takes the next snapshot id; of two at once one fails on the primary key, and
DuckLake retries (`ducklake_max_retry_count` 10, `ducklake_retry_wait_ms` 100, `ducklake_retry_backoff` 1.5). A Sentry
batch commits about every 43 ms at full speed. A flush commit takes about 118 ms (p50 of 264 commits with the lock).
So it nearly always loses: 8.2-11.2 s later (20 failed commits) DuckLake says "Exceeded the maximum retry count", and
the flush starts over, Parquet files included. Then the flusher sleeps 1-30 s on top. Sentry saw no conflict at all.

**The log line** is that same error: the full text starts with "Exceeded the maximum retry count of 10", and
`Duplicate key "snapshot_id: N"` is its last line. 3 of 35 were a different one, retention against the flush
([issue 0155](0155-otel-retention-loses-to-the-flusher.md)). `flush_errors` did not stay 0 in these runs: it counts
every conflict (7-10 per run).

**The retry's wait is not the cause.** The flusher waited 12-91 s of 150 s, but with a 100 ms wait OTLP stayed at
21,718-34,160 spans/s, against 22,463-24,229 with no change. Faster DuckLake retries gave 42,992-48,112. The lock
gave 265,726-267,515. So the wait stays as it is.

**The failed requests.** Reproduced as 12-22 per 150 s run, not the same run as the 71. Every one is on stderr as
`gave up after 8 tries, the last: HTTP 429 Too Many Requests`, on traces, logs and metrics alike; there was no other
kind. The 429 comes from the full write buffer (`WriteBuffer::push` → `Backpressure::Full` → `otlp/http.rs`): throttled
items per throttled request match the size of a whole request (traces 51,193 / 92 = 556; OTLP alone had 557 spans per
request), so the requests' bodies were read and refused, and the other budget, in-flight bytes, was not the cause. The
limit of 8 tries is the load generator's own, spread over about 18 s (it ignores `Retry-After: 2`). So for about 18 s
every try found the buffer full, while a flush was failing and the flusher sleeping. The server dropped nothing
(`dropped_rows` 0, `invalid` 0): those rows were never accepted. An exporter that retries longer would have got
them in. With the fix: 0 failed.

**Not checked:** whether the 184 failed requests of the 4 GiB container run in capacity.md had this cause, since
the same rates natively had no conflict before or after; memory-limited containers; two replicas
([issue 0156](0156-replicas-race-on-the-snapshot-id.md)).
