# 0030 — One lake commit at a time on a DuckDB instance

## Status
proposed (the owner decides when merging)

## Context
Both sides commit to one catalog from their own threads (ADR 0001). Every DuckLake commit takes the next
snapshot id; of two at once, one fails on the catalog's primary key and DuckLake tries it again: 10 times,
waiting 100 ms and 1.5 times longer each time.

At full speed the Sentry writer commits about every 43 ms (216 events per batch, 5,000 events/s). An OTLP flush
commit takes about 118 ms, because it writes more to the catalog. So the flush commit nearly always overlaps a
Sentry commit and loses. After 8-11 s DuckLake gives up, the flush fails, and its Parquet files are written
again. OTLP fell from about 335,000 spans/s to 5,000-21,000, and Sentry barely noticed (issue 0144).

ADR 0014's "inside one process only retention could cause one" was true of the Sentry side alone. It does not
hold on a shared catalog.

Options, measured with both load generators at full speed, 90 s, twice each, on a DuckDB-file catalog:

| option | spans/s | events/s | conflicts | what it costs |
|---|---:|---:|---:|---|
| nothing | 22,463-24,229 | 4,933-5,313 | 5 | |
| OTel waits 100 ms after a conflict, not 1-30 s | 21,718-34,160 | 5,245-5,534 | 5-6 | no clear gain: the flush still loses |
| DuckLake retries 100 times, every 10 ms | 42,992-48,112 | 5,178-5,766 | 5-6 | a global setting, more work on the catalog, still a tenth of the speed |
| **a lock around `COMMIT`** | 265,726-267,515 | 3,407-3,684 | 0 | a static lock; Sentry waits for OTel's commits |
| the lock, and Sentry's flush of inlined rows under it | 268,708-274,135 | 3,391-3,643 | 0 | one more place, no clear gain |

## Decision
`mallard_duck::CommitLock`: one lock per DuckDB instance. `mallard-server` gives the same one to both sides
(`Host.commit_lock`, `otel::Config.commit_lock`, which is not a setting). The Sentry writer and the OTel flusher
hold it around `COMMIT`, and nothing else. Parquet files are written before that, so they do not wait for
it.

Maintenance statements commit by themselves, and the lease commits rarely. Neither takes it. When one overlaps a
commit, DuckLake's retries handle it. No setting: there is nothing to choose.

Not a `static`. That was tried first: a commit retrying against another process holds the lock for up to 11 s, so
every other lake in the test process waited (the Sentry tests went from 7 s to 198 s, and two failed), and tests
that stand for two processes would no longer compete.

## Consequences
- At full speed the two sides now share what the machine can do: 260,000-270,000 spans/s plus 3,700-4,200
  events/s. OTLP alone does about 335,000 and Sentry alone about 5,200. Sentry gets slower than before at
  full speed: its p99 went from 515-631 ms to 650-939 ms. At the paced rates measured nothing changed
  ([capacity](../otel/capacity.md#both-sides-at-full-speed)).
- A new writer that commits to the lake must take the lock, or it starves large commits again.
- It does nothing between processes. Replicas on one catalog still race on the snapshot id, with the retries
  as before. And a commit retrying against another replica holds the lock while it does, so that replica's other
  side waits too, for up to 11 s. Not measured
  ([issue 0156](../../work/issues/0156-replicas-race-on-the-snapshot-id.md)).
- A `DELETE` that removes rows still conflicts with inserts. That is about the rows, not the snapshot id, so the
  lock cannot help ([issue 0155](../../work/issues/0155-otel-retention-loses-to-the-flusher.md)).
