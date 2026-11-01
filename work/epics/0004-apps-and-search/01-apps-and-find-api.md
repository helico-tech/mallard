---
status: todo
created: 2026-11-01
---
# 01 — Apps and find API

## Goal
One place that knows what an app is and where an ID points, so the CLI, TUI and
web UI do not each carry the rules.

## Acceptance criteria
- `GET /api/v1/apps?window=1h` in `mallard-server`, which is the only crate that
  sees both sides, with the normal API token. Each row: `slug`, `name`, `project`
  (id, or null), `services` (names, may be empty). Sorted by slug.
- Slug rule is one function, the bridge's `service_slug`, tested with the same cases.
- `GET /api/v1/find?id=<id>` answers what `mallard find` works out today: a trace,
  an event, or an issue, with what a client needs to link to it. A bad ID is 400;
  an unknown one is 404.
- `mallard find` uses it, with the same output and exit codes as before.
- `mallard apps` lists apps under the ADR 0008 contract.
- An OpenAPI document, `docs/openapi.json`, checked by a test like the other two,
  so the web UI can generate types.

## Plan
Read how `crates/mallard/src/find.rs` looks things up, and move that to the
server. Add the routes next to `/healthz` in `crates/mallard-server/src/lib.rs`.
Services for the window come from the existing query; projects are not limited by
the window, or an app with no recent data would disappear from the picker.

## Progress log

## Follow-ups
