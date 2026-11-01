---
status: done
type: debt
created: 2026-11-01
---
# 0001. More than one server process per catalog is not supported

## Problem
Projects, issue status and known bundles are cached in memory, loaded at
startup and updated only by the local writer. With two pods on one
(PostgreSQL) catalog, a project created on pod A is unknown on pod B (404 on
API and ingest, verified on Kubernetes), both pods can insert the same issue
(duplicate `issues` rows -> duplicate rows in issue lists), regressions are
detected per pod, and both pods run catalog-wide maintenance by default.

## How to reproduce
Helm chart with `catalog.type=postgres` and `replicaCount=2`; create a project
through one pod, ingest through the other.

More from the data-model review (2026-11-01): `create_project` allocates ids
as local max+1, so two processes create duplicate ids; after a restart one
of them disappears from the cache and its DSN gets 401. `project_has_bundles`
is per process, so symbolication is skipped for bundles uploaded through
another process. Retention and compaction also run in every process
(`SDL_CATALOG_WIDE_MAINTENANCE` only guards expiry and cleanup), and
concurrent flushes of one table conflict.

## Notes
- Today: chart defaults to 1 replica and warns; docs say scale up, not out.
- Fix sketch: refresh caches periodically and on miss (projects), make issue
  queries duplicate-tolerant (`arg_min(..., first_seen)` per group), elect one
  maintenance runner (or keep `SDL_CATALOG_WIDE_MAINTENANCE=false` on all but
  one), and rely on DuckLake conflict retries for concurrent commits.

### What changed (done)
Decisions and reasons: [ADR 0021](../../../docs/adr/0021-sentry-several-processes.md).
- Maintenance: one process, by a lease row in `meta` (a transaction that deletes
  and inserts it, so the second commit conflicts). No config flag: pods of one
  Deployment share their config. Safety does not rely on the lease.
- Project ids count up, allocated with the `project_seq` row in the same
  transaction: no duplicate id or slug across processes.
- Project cache: refresh on a miss (at most once a second) and every 30 s.
  `GET /v1/projects` refreshes first.
- Writer thread reads projects, bundles and status changes every 30 s
  (bundle flag and regressions across processes).
- Queries take one row per group: issue list, stats, releases (detail already
  did).
- Startup tries again on a conflict.
- Helm NOTES and `docs/sentry/deployment.md` say what is true now.
- Tests: `db::tests::two_processes_on_sqlite` (two DuckDB instances on one
  catalog: project known at once, no id or slug twice under concurrent creates,
  no event lost under concurrent ingest and maintenance, status and bundles reach
  the other side), `two_processes_on_postgres` (same; ignored, needs
  `SDL_TEST_POSTGRES`; ran against postgres:17 in Docker),
  `one_process_holds_the_maintenance_lease`, `a_lease_race_has_one_winner`,
  `duplicate_issue_rows_count_once`. Also by hand with two `mallard-server`
  processes on Postgres, starting at once: a project created on A takes events on
  B at once, 60 events at once on both end up as one issue with count 60, a resolve
  on A is followed by a regression from an event on B after the sync, one process
  did all flushes.
- Follow-up: `work/issues/0051-otel-maintenance-on-every-replica.md`.
