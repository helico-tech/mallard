# 0010 — Read-only SQL over the error tables

## Status
accepted

## Context
Agents use ad-hoc SQL for questions the fixed endpoints don't answer
(spec `docs/specs/agent-cli.md`, J12, S15). The OTel side has had
`POST /api/v1/sql` since its ADR 0005 (`docs/otel/adr/0005-sql-endpoint.md`):
off by default, one SELECT checked with DuckDB's own parser, lake tables
only, a timeout. The Sentry tables (`events`, `issues`, `issue_activity`)
had no such endpoint, and `mallard sql` only reached the telemetry tables.

Constraints: each side owns its schema and API (ADR 0001, 0008); shared
infrastructure lives once in `mallard-common` / `mallard-duck` (ADR 0005);
the libraries never depend on each other.

## Decision

**Endpoint.** The Sentry side gets its own `POST /v1/sql`
(`/sentry/v1/sql` in mallard), with the same request and answer shape as
the OTel one (`{query, limit}` → `{columns, rows, truncated, elapsed_ms}`).
Tables: `events`, `issues`, `issue_activity` in `lake.sentry` (the readers'
default schema, so `FROM events` works). `projects` (public keys) and the
sourcemap tables are left out.

**One guard.** The checks of otel ADR 0005 move to `mallard_duck::sql`
(`validate`, `run`); both sides call it with their schema and table list
and map its two errors (`Rejected` → 400, `Internal` → 500) to their own
API errors. Wire types stay per side (`odl_api::Sql*`, `sdl_types::Sql*`),
like every other API type.

**Switches.** `[sentry.query] enable_sql = false` by default (standalone:
`--enable-sql` / `SDL_ENABLE_SQL`); off answers 403 `disabled`, which the
CLI maps to exit 7. The Sentry API always requires a token, so there is no
"open" case to forbid. `sql_timeout = "30s"`: the query is interrupted
after it (504 `timeout`); the reader goes back to the pool only once the
query stopped (`Db::read_for`). Rows: default 1000, max 100000.

**CLI.** One command, `mallard sql "<select>"`, with `--errors` to query the
error tables instead of the telemetry tables (`mallard errors sql` also
works). Options considered:

- `--errors` flag (chosen): explicit, one line in the help, no guessing;
  the default stays what `mallard sql` did before.
- Route by table name in the query: no flag, but the CLI would have to
  parse SQL (or try one side, then the other), and a query that names
  tables of both sides could only fail in a confusing way.
- `--side sentry|otel`: the same as `--errors` with more typing and the
  internal side names that ADR 0008 hides.
- A separate `mallard errors-sql` command: one more noun for the same job.

Joins across sides are not possible: each query runs on one side.

## Consequences
- One place to fix the guard; a new risky DuckDB function is denied for
  both sides at once.
- The same caveat as otel ADR 0005: defense in depth, not a sandbox. CPU
  and memory are bounded only by `sql_timeout` and `duckdb.memory_limit`;
  treat API tokens as admin access when SQL is on.
- `mallard status` still reports only the OTel side's `sql` capability; an
  agent learns that the error tables are off from exit 7.
- Cross-side SQL (errors joined with spans in one query) would need a
  mallard-level endpoint over both schemas; not done.
