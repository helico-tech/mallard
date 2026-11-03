# Data model

All data lives in one DuckLake schema, `lake.sentry`, next to the telemetry
side's `lake.otel`. DuckLake keeps metadata (snapshots, file lists, statistics,
and small "inlined" batches) in the **catalog** database (a DuckDB file, SQLite or
PostgreSQL) and table data as **Parquet files** on disk or in object storage.

Design rules:

1. **Append-only.** Rows are never updated. Mutable state (issue status) is an
   append-only log where the newest row wins. In a lakehouse an `UPDATE`
   rewrites data or writes delete files; an `INSERT` is cheap. The exceptions
   are retention and two coordination rows in `meta` (ADR 0021).
2. **One writer per process.** All row writes go through one thread, in
   batches. One batch is one DuckLake transaction and one snapshot. Maintenance
   runs next to it and only rewrites files (ADR 0014). Several processes can
   share a catalog (ADR 0021).
3. **Prune by time, then by issue.** Almost every query filters on
   `project_id`, a time window and often `group_hash`. The physical
   layout is built for exactly that.
4. **Event time for issues, receive time for storage.** First/last seen,
   trends, tag facets and stats use the event's `timestamp`, as Sentry does.
   When an envelope's `sent_at` differs 55 minutes or more from the receive
   time, the client's clock is off and the event and its breadcrumbs shift by
   that difference. Ingest then replaces a `timestamp` more than 60 s after or
   30 days before the receive time with the receive time (`MAX_SECS_IN_FUTURE`
   / `MAX_SECS_IN_PAST` in `mallard-sentry-protocol`), so it stays close to
   `received_at`. Partitions, retention, event lists, trace lookups and the
   issue activity log use `received_at` / write time.

## Tables

| Table | Rows | Written | Purpose |
|---|---|---|---|
| `events` | one per error event | every batch | the fact table |
| `issues` | one per issue (group) | when a group is first seen | first-seen info |
| `issue_activity` | one per status change | resolve / ignore / regression | current status = newest row |
| `projects` | one per project | on create | slug, DSN public key |
| `artifact_bundles` | one per (bundle, project) | sourcemap upload | bundle object + release |
| `artifact_files` | one per file in a bundle | sourcemap upload | debug-id / url lookup |
| `releases` | one per project per `sentry-cli releases new` | `releases new` | grouped by project and version, earliest date wins |
| `release_activity` | one per finalize | `releases finalize` | date released = newest row per version |
| `deploys` | one per deploy | `deploys new` | a deploy of a version counts for each of its projects |
| `meta` | a few | startup, project creation, maintenance | schema version; `project_seq` and `maintenance_lease` are replaced (ADR 0021) |

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
can be skewed): retention (`DELETE ... WHERE received_at < cutoff`) removes
whole files. A window on `received_at` reads only the days it covers; a
window on `timestamp` is pruned by DuckLake's per-file min/max, which stays
tight because `timestamp` is clamped to near `received_at` (rule 4).
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
the telemetry side's spans table, so `sentry.events JOIN otel.spans USING
(trace_id)` works without casts when both share a catalog.

### `issues` and `issue_activity`

`issues` gets one row the first time a group hash is seen (first seen =
the earliest event `timestamp` in that batch, first release, first title).
Two processes can each write one for a new group, so reads take one row per
group (`arg_min(..., first_seen)`).
An older event in a later batch does not change the row (append-only); issue
list and detail read `least(first_seen, min(timestamp))`, so first seen moves
back as soon as the window holds that event. "New issues" in stats and the
`issues` row itself do not see it. The current title, level and culprit come from the newest event in the
queried window (`arg_max(title, timestamp)`), as in Sentry.

Status is `arg_max(status, changed_at)` over `issue_activity`. Reasons:
`user` (API) and `regression` (an event of a resolved issue reopens it when its
`timestamp` is not older than the resolve; ignored issues stay ignored).

The writer keeps `(project, group) -> status` of recently seen issues in memory
to decide "new issue" and "regression" without reading the lake on the hot
path. The cache holds at most 200,000 issues and forgets an issue without
events for the retention period; it is not loaded at startup. On a miss the
writer asks the lake, once per batch for all missing groups, so an issue that
left the cache keeps its one `issues` row and its status. The cache changes only
after a commit succeeds.

**Limit on new issues.** A project gets at most 1000 new issues per hour
(`MAX_NEW_ISSUES_PER_HOUR`, per process; not configurable). Events of further
new groups go into one issue per project, "Too many new issues"
(`OVERFLOW_GROUP`, `6d616c6c61726421`), and are counted in
`mallard_sentry_issue_overflow_events_total`. Each event keeps its own title and payload,
so nothing is lost but the grouping; the next hour those groups get their own
issue from their next event on. Known groups are never affected.
`issues` and `issue_activity` rows stay after their events are past retention;
an issue that comes back keeps its history.

### Release tables

See [ADR 0035](../adr/0035-stored-releases-and-deploys.md). Only `releases new` names projects, so the other two tables go by
version. Retention leaves all three. The release list merges them with the `release` of events in the window, so:

- A version named only by events older than the window is not listed, even with a deploy in the window: no row says which project it belongs to.
- A deploy or finalize of a version nothing else names is stored, and shown only by that version's own deploy list.

`GET /sentry/v1/deploys` gives what the charts mark: deploys finished in the window, of a project's versions or all of them. A telemetry chart reaches them through the app: a project and a service with one slug (ADR 0027). So with All apps a chart marks every deploy, and an app with no project marks none. A deploy is marked for its whole app, not per environment.

The list took about 20 ms on a lake filled at full speed for 150 s (389 MB). Larger lakes and a full 14 days were not measured.

### Sourcemap tables

`artifact_bundles` points at the zip
(`<storage.artifacts_path>/artifacts/bundles/<sha1>.zip`). `artifact_files` indexes every file
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
  `sentry.ingest.batch_window` (500 ms), so a quiet server makes at most two commits
  (snapshots) per second. When 200+ events are queued it commits at once, so
  under load latency is the commit time and throughput is not capped by the
  window.
- **Data inlining.** Batches of up to 1000 rows are stored inside the catalog
  (no Parquet file). A flush (every `sentry.maintenance.flush_interval`, or after
  `sentry.maintenance.flush_rows` inlined rows) writes them out as one sorted file per day.
  This avoids thousands of tiny Parquet files. Bigger batches (only under
  heavy load) go straight to Parquet.
- **Backpressure.** The queue is bounded (`sentry.ingest.queue_capacity`). When full,
  ingest answers `429` with `Retry-After`, which Sentry SDKs respect.

## Maintenance

The maintenance thread flushes inlined data (see above) and, every
`sentry.maintenance.compact_interval` (2 min), runs, scoped to its schema:

1. retention, on the writer thread because a `DELETE` conflicts with concurrent
   inserts ([ADR 0014](../adr/0014-sentry-maintenance-thread.md)):
   `DELETE FROM events WHERE received_at < midnight - retention`
   (`sentry.retention.events`, rounded up to days; day-aligned, so whole daily
   files are dropped without delete files)
2. `ducklake_merge_adjacent_files` and `ducklake_rewrite_data_files` per table
   (target file size 16 MB for `events`; [capacity](../otel/capacity.md#both-sides-in-one-container))

Snapshot expiry and file cleanup are catalog-wide. The telemetry side runs them
(`otel.maintenance.snapshot_retention`, ADR 0001).

`POST /sentry/v1/maintenance` runs a flush plus all of the above immediately.

## Queries (what each API call reads)

| API | Reads | Pruning |
|---|---|---|
| issue list | `events` (project, `timestamp` window): group_hash, user_key, timestamp, title, culprit, level; `issues`; `issue_activity` | files by timestamp min/max |
| issue detail | same for one group + tag facets | files + row groups by group_hash |
| events of issue | narrow columns, `received_at` window, `ORDER BY received_at DESC LIMIT` | row groups by group_hash |
| event detail | one row incl. `payload` | group_hash when `?issue=` is given; else growing windows (1 d, 7 d, 31 d, all) |
| stats | timestamp, level, group_hash, user_key | files by timestamp min/max |
| by trace | trace_id (UUID equality) | days in window |

## Changing the schema

`schema::SCHEMA_VERSION` is recorded in `meta`. Add columns with
`ALTER TABLE ... ADD COLUMN` (DuckLake supports schema evolution without
rewriting files) and bump the version. Never repurpose a column.
