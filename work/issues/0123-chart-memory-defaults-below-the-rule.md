---
status: todo
type: question
created: 2026-11-01
---
# Helm defaults leave less memory than the documented rule

## Problem
`docs/otel/capacity.md` says: container ≥ `duckdb.memory_limit` +
`otel.ingest.max_buffer_bytes` + 1 GB, and that a 2 GiB pod should run 768 MB + 256 MB.
That page used to say the chart defaults to those numbers. The chart now has
`duckdb.memoryLimit: 1GB`, `resources.limits.memory: 2Gi`, and does not set
`max_buffer_bytes`, so the default of 512 MB applies: 1 GB + 512 MB + 1 GB = 2.5 GB
in a 2 GiB limit. Sentry's rule, about half the container, is met. Both sides now
share that memory, and the rule only counted telemetry.

## Notes
Question to answer: what should a 2 GiB pod run, with both sides in one process?
Options: lower `duckdb.memoryLimit` and render `max_buffer_bytes = "256MB"` in the
chart, or raise the default limit. A measurement under load would settle it; the
capacity numbers are from the old binaries. Found during issue 0016. The docs state
the mismatch and link here.
