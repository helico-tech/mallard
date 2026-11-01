---
status: todo
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
