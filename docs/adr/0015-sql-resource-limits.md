# 0015 — Resource limits for ad-hoc SQL

## Status
accepted

## Context
Both sides' SQL endpoints share one guard, `mallard_duck::sql` (ADR 0011,
otel ADR 0005). Before this ADR the limits were: `limit` rows (default 1000,
max 100000) with a `truncated` flag, and a timeout per side (otel
`query.timeout`, Sentry `query.sql_timeout`; both interrupt the query).
Nothing bounded the size of an answer: 100000 rows of Sentry `payload` can be
gigabytes, held in the server as JSON values and sent in one response.

DuckDB has no per-query or per-connection memory limit. `memory_limit` is a
global setting of the database, which `mallard-server` owns (`[duckdb]`), and
libraries must not change global settings on the shared connection (ADR 0001,
0005).

Options considered for the memory side:

- Row and byte cap in the guard (chosen): one constant, both sides at once,
  bounds what the server keeps and sends. Does not bound what DuckDB uses
  to compute the answer.
- A second DuckDB instance with its own `memory_limit`: a real memory bound,
  but a second catalog attachment and buffer pool, and a DuckDB-file catalog
  is meant for one instance.
- A config setting for the cap: nobody asked for one; a constant can become
  a setting later.

## Decision
The guard stops collecting rows once they hold `MAX_RESULT_BYTES` (32 MiB) of
JSON text and sets `truncated`, the same flag the row limit uses; the row that
crosses the cap is not returned. It applies to both sides and is not
configurable. The timeouts and the row limit stay as they are.

Memory stays bounded only by DuckDB's global `duckdb.memory_limit`, and CPU by
the timeout and `duckdb.threads`. SQL access stays admin access: this is no
sandbox for untrusted users.

## Consequences
- `truncated: true` now means "more rows than `limit`, or the answer reached
  32 MiB". An agent reacts the same way: narrow the query.
- An expensive query can still use all of `memory_limit` until its timeout,
  competing with ingest. If non-admins ever get SQL access, that needs a
  separate process or instance; this ADR does not provide it.
