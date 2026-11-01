# 0004. Inlining small inserts and two-tier compaction

Date: 2026-11-01

## Status

accepted

## Context

A flush every 5 s writes one file per table and day: 17,280 files per day per
table before compaction. During quiet hours each file holds a handful of
rows. DuckLake offers *data inlining* (small inserts stored in the catalog
database) and `ducklake_merge_adjacent_files` (merge small files). That
function re-merges any file below the target size, so running it often with
a 128 MB target rewrites the same growing file over and over.

## Decision

- `data_inlining_row_limit = 1000` by default: flushes under 1000 rows go to
  the catalog, not to Parquet.
- Every `compact_interval` (10 min): `ducklake_flush_inlined_data`, then
  merge files with `max_file_size = small_file_size` (16 MB). This only
  touches small files, so it is cheap.
- Every `full_compact_interval` (6 h): merge everything below
  `target_file_size` (128 MB).
- Retention, snapshot expiry (keep 1 h) and file cleanup (15 min grace) run
  hourly.

## Consequences

- Write amplification stays low: a byte is rewritten about 2-3 times before
  it lands in a large file.
- Recent data (last 10 min) may live inlined or in many small files. Queries
  still see it, at a small cost.
- With a PostgreSQL catalog, inlined rows briefly live in PostgreSQL. Set
  `data_inlining_row_limit = 0` if you don't want telemetry bytes in the
  catalog database.
