---
status: todo
type: debt
created: 2026-11-01
---
# Agent CLI P1: leftovers

## Problem
[Issue 0004](0004-agent-cli-p1.md) is done; these small gaps stayed open:

- `mallard status` reports only the otel side's `sql` capability. Whether
  `mallard sql --errors` works (`[sentry.query] enable_sql`, ADR 0011) is
  only learned from exit 7. Add it to the Sentry `/v1/info` and to
  `capabilities` (e.g. `sql_errors`).
- `mallard schema` lists only the telemetry tables. Once the Sentry side
  can describe its SQL tables (`events`, `issues`, `issue_activity`), the
  host's `schema` handler in `crates/mallard/src/main.rs` can add them.
- Two copies of the SQL result table printer: `odl-cli`'s `sql_text`
  (`crates/odl-cli/src/fmt.rs`) and `sdl-cli`'s `sql`
  (`crates/sdl-cli/src/cli/commands.rs`). Move one into `mallard-term`
  (columns + JSON rows → table) and use it from both.
- Spec side findings (`docs/specs/agent-cli.md` §10) still open:
  - breadcrumb order in `events show` text ("newest first" showed ascending
    times with `sdl-loadgen` data; check with a real SDK);
  - the sentry-cli release endpoints are stubs (`releases new`/`deploys`
    answer but store nothing). `mallard releases list` derives releases
    from events instead; storing them would need a table.

## Notes
- None of these blocks an agent: each has a workaround (exit 7, `sql`
  errors name the tables, the printers behave the same).
