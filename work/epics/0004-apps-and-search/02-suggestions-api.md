---
status: todo
created: 2026-11-01
---
# 02 — Suggestions API

## Goal
Everything a filter bar needs to suggest. The telemetry side has it
(`/api/v1/services`, `/operations`, `/attributes`). The errors side has no tag
list, only tag filters on issues and events.

## Acceptance criteria
- Errors side: tag keys with counts for a project and window, and the top values
  of one key, with an optional prefix. In `docs/sentry/openapi.json`.
- Telemetry `/attributes` has a prefix filter for values if it lacks one.
- Limited by window and row count; a wide query cannot slow ingest or take the
  connection for long. State the measured time on a large lake.
- Reads go through the existing read path, with the same limits as other queries.

## Plan
See how tags are stored for `tag_filters` in `crates/mallard-sentry-server/src/api.rs`
and read `docs/sentry/data-model.md`. Copy the shape of the telemetry attributes
endpoint, so both sides look alike to a client.

## Progress log

## Follow-ups
