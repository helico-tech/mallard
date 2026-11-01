# Data model

All data lives in one DuckLake schema (default `lake.sentry`). DuckLake keeps
metadata (snapshots, file lists, statistics, and small "inlined" batches) in
the **catalog** database (a DuckDB file or PostgreSQL) and table data as
**Parquet files** in object storage.

Design rules:

1. **Append-only.** Rows are never updated. Mutable state (issue status) is an
   append-only log where the newest row wins. In a lakehouse an `UPDATE`
   rewrites data or writes delete files; an `INSERT` is cheap.
2. **One writer.** All writes go through one thread, in batches. One batch is
   one DuckLake transaction and one snapshot.
3. **Prune by time, then by issue.** Almost every query filters on
   `project_id`, a `received_at` window and often `group_hash`. The physical
   layout is built for exactly that.

## Tables

| Table | Rows | Written | Purpose |
|---|---|---|---|
| `events` | one per error event | every batch | the fact table |
| `issues` | one per issue (group) | when a group is first seen | first-seen info |
| `issue_activity` | one per status change | resolve / ignore / regression | current status = newest row |
| `projects` | one per project | on create | slug, DSN public key |
| `artifact_bundles` | one per (bundle, project) | sourcemap upload | bundle object + release |
| `artifact_files` | one per file in a bundle | sourcemap upload | debug-id / url lookup |
| `meta` | a few | startup | schema version |

### `events`

```sql
CREATE TABLE lake.sentry.events (
  project_id INTEGER, received_at TIMESTAMP, timestamp TIMESTAMP, event_id UUID,
  group_hash UBIGINT,                      -- issue id (xxh3 of the fingerprint)
  level, platform, environment, release, dist, server_name, transaction_name,
  logger, title, culprit, exception_type, exception_value, message VARCHAR,
  handled BOOLEAN, user_key VARCHAR,       -- "id:42" / "email:a@b" / "ip:1.2.3.4"
  trace_id UUID, span_id UBIGINT,          -- join keys for traces (OTel)
  sdk_name, sdk_version VARCHAR,
  tags MAP(VARCHAR, VARCHAR),              -- explicit + derived tags
  size_bytes INTEGER,
  payload VARCHAR                          -- full normalized event JSON
)
PARTITIONED BY (epoch_day(received_at))
SORTED BY (project_id, group_hash, received_at);
```

**Partitioning by day of `received_at`** (not the client `timestamp`, which
can be skewed): a time window query reads only the days it covers, and
retention (`DELETE ... WHERE received_at < cutoff`) removes whole files.
Daily is the right grain for the target scale: hourly would create too many
small files, monthly would make retention coarse and windows read too much.

**Sorting by `(project_id, group_hash, received_at)`** inside each file gives
tight min/max statistics per row group, so "events of this issue" and "tag
facets of this issue" skip most row groups. Files are sorted when inlined data
is flushed and when files are merged.

**Typed hot columns + one `payload` column.** List and aggregate queries touch
only small typed columns (Parquet reads columns independently). The full event
(stack frames, breadcrumbs, contexts) is only read for one event at a time.
`payload` is excluded from statistics (`skip_stats_columns`) and compressed
with zstd. Measured: about 0.5 KB per event on disk for a 2.5 KB JS event with
30 breadcrumbs (see [capacity.md](capacity.md)).

**`tags` as a MAP**, not a separate long table: facet queries
(`unnest(map_keys(tags)), unnest(map_values(tags))` for one issue) are fast
enough at this scale and keep the write path to one table.

**`trace_id UUID` / `span_id UBIGINT`** use the same encoding as
otel-ducklake's spans table, so `sentry.events JOIN otel.spans USING
(trace_id)` works without casts when both share a catalog.

### `issues` and `issue_activity`

`issues` gets one row the first time a group hash is seen (first seen, first
release, first title). The current title, level and culprit come from the
newest event in the queried window (`arg_max(title, received_at)`), as in
Sentry.

Status is `arg_max(status, changed_at)` over `issue_activity`. Reasons:
`user` (API) and `regression` (a new event for a resolved issue reopens it;
ignored issues stay ignored).

The writer keeps `(project, group) -> status` in memory to decide "new issue"
and "regression" without reading the lake on the hot path. It is loaded at
startup and updated only after a commit succeeds.

### Sourcemap tables

`artifact_bundles` points at the zip in object storage
(`<storage>/artifacts/bundles/<sha1>.zip`). `artifact_files` indexes every file
in it by `debug_id` (sorted on `(project_id, debug_id)`) and by
`(release, dist, url)` for the release-based fallback. Bundles are uploaded
per project; lookups are always scoped to the event's project.

## Write path

```
request -> normalize -> symbolicate -> group -> queue ──┐
                                                         v
writer thread: take everything queued (group commit)
   1. append rows to an in-memory staging table (DuckDB appender, fast)
   2. BEGIN
        INSERT INTO events SELECT ... FROM staging
        INSERT INTO issues          (new groups)
        INSERT INTO issue_activity  (regressions)
      COMMIT                          -> one DuckLake snapshot
   3. answer every request of the batch (200 only after commit)
```

- **Group commit.** After the first event the writer collects for up to
  `SDL_BATCH_WINDOW` (500 ms), so a quiet server makes at most two commits
  (snapshots) per second. When 200+ events are queued it commits at once, so
  under load latency is the commit time and throughput is not capped by the
  window.
- **Data inlining.** Batches of up to 1000 rows are stored inside the catalog
  (no Parquet file). A flush (every `SDL_FLUSH_INTERVAL`, or after
  `SDL_FLUSH_ROWS` inlined rows) writes them out as one sorted file per day.
  This avoids thousands of tiny Parquet files. Bigger batches (only under
  heavy load) go straight to Parquet.
- **Backpressure.** The queue is bounded (`SDL_QUEUE_CAPACITY`). When full,
  ingest answers `429` with `Retry-After`, which Sentry SDKs respect.

## Maintenance

Every `SDL_COMPACT_INTERVAL` (1 h) the writer runs, scoped to its schema:

1. retention: `DELETE FROM events WHERE received_at < midnight - SDL_RETENTION_DAYS`
   (day-aligned, so whole daily files are dropped without delete files)
2. `ducklake_merge_adjacent_files` and `ducklake_rewrite_data_files` per table
   (target file size 64 MB for `events`; DuckDB threads lowered to
   `SDL_MAINTENANCE_THREADS` because sorted merges need memory per thread)
3. if `SDL_CATALOG_WIDE_MAINTENANCE`: `ducklake_expire_snapshots` and
   `ducklake_cleanup_old_files` older than `SDL_SNAPSHOT_RETENTION` (1 h)

`POST /v1/maintenance` runs a flush plus all of the above immediately.

## Queries (what each API call reads)

| API | Reads | Pruning |
|---|---|---|
| issue list | `events` (project, window): group_hash, user_key, received_at, title, culprit, level; `issues`; `issue_activity` | days in window |
| issue detail | same for one group + tag facets | days + row groups by group_hash |
| events of issue | narrow columns, `ORDER BY received_at DESC LIMIT` | row groups by group_hash |
| event detail | one row incl. `payload` | group_hash when `?issue=` is given; else growing windows (1 d, 7 d, 31 d, all) |
| stats | received_at, level, group_hash, user_key | days in window |
| by trace | trace_id (UUID equality) | days in window |

## Changing the schema

`schema::SCHEMA_VERSION` is recorded in `meta`. Add columns with
`ALTER TABLE ... ADD COLUMN` (DuckLake supports schema evolution without
rewriting files) and bump the version. Never repurpose a column.
