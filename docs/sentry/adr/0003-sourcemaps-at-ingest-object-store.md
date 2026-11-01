# 0003. Sourcemaps: symbolicate at ingest, bundles in object storage

## Status
accepted (2026-10-31)

## Context
JS stack traces are minified; grouping on minified frames splits issues on
every release. Sentry symbolicates at ingest. sentry-cli and the bundler
plugins upload "artifact bundles" (zip + manifest, files keyed by debug id).

## Decision
- Store uploaded bundles as-is in object storage (`object_store` crate:
  S3 default, GCS, Azure, local) under `artifacts/bundles/<sha1>.zip`; index
  their files in DuckLake (`artifact_files`).
- Symbolicate at ingest, before grouping: debug id lookup first, then release +
  dist + url. Cache lookups (60 s), bundles and parsed maps (LRU by size).
  Keep the original frames in `raw_stacktrace`.
- Recover original function names with the sourcemap crate's minified-name
  heuristic (needs the minified file, which bundles include).

## Consequences
- Issues are stable across releases; events are stored already symbolicated.
- Maps uploaded after an error do not fix that stored event (same as Sentry
  without reprocessing).
- Projects without bundles pay nothing (checked in memory before any lookup).
