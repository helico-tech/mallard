---
status: done
type: debt
created: 2026-11-01
---
# Ad-hoc SQL can still use a lot of CPU/memory until the timeout

## Problem
The SQL endpoint is admin-only by design (ADR 0005). Consider a separate DuckDB connection limit or result-size caps if non-admins get access.

## Notes
Found during the v0.1 build and review (epic 0001).

Done 2026-11-01, [ADR 0015](../../../docs/adr/0015-sql-resource-limits.md). The
row limit and both timeouts existed; the shared guard now also stops at 32 MiB
of row data per answer (`truncated`), on both sides. DuckDB has no per-query
memory limit (`memory_limit` is global and the host's), so memory stays bounded
by `duckdb.memory_limit` only. SQL access stays admin access.
