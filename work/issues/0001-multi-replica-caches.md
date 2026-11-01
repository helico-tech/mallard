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

## Notes
- Today: chart defaults to 1 replica and warns; docs say scale up, not out.
- Fix sketch: refresh caches periodically and on miss (projects), make issue
  queries duplicate-tolerant (`arg_min(..., first_seen)` per group), elect one
  maintenance runner (or keep `SDL_CATALOG_WIDE_MAINTENANCE=false` on all but
  one), and rely on DuckLake conflict retries for concurrent commits.
