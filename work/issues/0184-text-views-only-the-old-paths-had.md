---
status: todo
type: question
created: 2026-11-02
---
# Text views that only the old paths had

## Problem
Issue 0183 removed `telemetry status` and `telemetry trace -A`. What they showed is still there in JSON
(`status --json`, `trace --json`), but no longer as text: ingest counters and storage per table, and a span's
attributes and events. `errors info` also showed the catalog and storage as text.

## Notes
A choice for the owner: fine as it is, because agents read JSON and people have the TUI; or more text on `status`,
and a flag on `trace`. Not done now, because the rule for this change was no new options.
