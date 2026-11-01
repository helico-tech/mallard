---
status: todo
type: debt
created: 2026-11-01
---
# Ad-hoc SQL can still use a lot of CPU/memory until the timeout

## Problem
The SQL endpoint is admin-only by design (ADR 0005). Consider a separate DuckDB connection limit or result-size caps if non-admins get access.

## Notes
Found during the v0.1 build and review (epic 0001).
