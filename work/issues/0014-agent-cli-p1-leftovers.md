---
status: done
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
- Done 2026-11-01:
  - `/v1/info` has `sql`; `mallard status` shows it as `capabilities.sql_errors`.
  - New `GET /sentry/v1/schema`; `mallard schema` lists both sides, with a
    `side` per table (`crates/mallard/src/schema.rs`).
  - One printer: `mallard_term::table::sql_result`.
  - Breadcrumbs: captured from `@sentry/node` 11 (SDK order is oldest first),
    so `events show` ("newest first") is right. `sdl-loadgen` sent newest
    first; fixed there.
  - Release endpoints stay stubs, documented: storing them needs a table, a
    writer command, a schema step with no migrations yet, and a rule for
    merging with the derived list, and nothing needs it now. See
    [issue 0060](0060-store-sentry-cli-releases.md).
