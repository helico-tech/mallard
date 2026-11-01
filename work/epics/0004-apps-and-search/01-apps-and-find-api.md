---
status: todo
created: 2026-11-01
---
# 01 — Apps and find API

## Goal
One place that knows what an app is and where an ID points, so the CLI, TUI and
web UI do not each carry the rules.

## Acceptance criteria
Decisions: ADR 0027, ADR 0028.

Apps:
- `GET /api/v1/apps` in `mallard-server`, which is the only crate that sees both
  sides, with the normal API token. Window by `since`/`from`/`to`, like every other
  endpoint. Each row: `slug`, `name`, `project` (id, or null), `services` (names, may
  be empty). Sorted by slug.
- Slug rule is one function, the bridge's `service_slug`, tested with the same cases.
- It uses a cheap list of distinct service names, not the aggregate behind
  `/api/v1/services`: pickers call it on every window change, up to 30 days. State
  the measured time on a large lake.
- One side failing does not fail the list: return the other side plus `warnings`. A
  window above one side's `max_window` does not become a 400 for the whole list.
- Telemetry endpoints with `service` accept it repeated (attributes, logs,
  logs/series, metrics, metrics/query, operations, spans/series, traces).
  A single value behaves as before.

Find:
- `GET /api/v1/find?id=<id>` returns a list: 32 hex can be both a trace and an event,
  and 16 hex finds spans. A bad ID is 400, no match is 404.
- A side that failed is a 5xx if it may have hidden the answer, as `find.rs` reports
  today. It never turns into a 404.
- Event matches carry the project slug as well as the id, since links use slugs.
  Trace matches carry a time, because the trace page only looks back 7 days without one.
- `mallard find` uses it, with the same output and exit codes as before.

Also:
- `mallard apps` lists apps under the ADR 0008 contract.
- An OpenAPI document, `docs/openapi.json`, checked by a test like the other two,
  so the web UI can generate types.

## Plan
Read how `crates/mallard/src/find.rs` looks things up, and move that to the
server. Add the routes next to `/healthz` in `crates/mallard-server/src/lib.rs`.
Projects are not limited by the window, or an app with no recent data would disappear from the picker.

## Progress log

## Follow-ups
