---
status: done
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

Steps (2026-11-01):
1. New crate `mallard-api`: the types of `/api/v1/apps` and `/api/v1/find` and the
   id parser, so `mallard-server` and the slim CLI share them without DuckDB.
2. `mallard-otel-server`: `Server::service_names` (`SELECT DISTINCT service_name` per
   table in the window, capped), counts and first time of one trace, the span lookup.
   `mallard-sentry-server`: `Server::projects` (fails when the read fails),
   events of a trace, event and issue lookup. All on the sides' own read paths.
3. `mallard-server/src/api.rs`: the two routes behind `server.api_tokens`,
   `service_slug` moved there and used by the bridge. A window above otel's
   `max_window` is cut to that length plus a warning; projects ignore the window.
4. Repeated `service=`: one extractor in the otel API takes all values out of the raw
   query, the rest goes to the normal query struct. One value keeps `service_name = ?`.
5. `mallard find` calls the endpoint and maps to the same `Match` as before;
   `mallard apps` is new. Client calls in `mallard-client`.
6. `docs/openapi.json` with a test, docs, tests, measurement on a large lake.

## Progress log
- 2026-11-01: Built as planned. Decisions the criteria left open:
  - `/api/v1/find` also takes `project` and returns issues, because `mallard find`
    does: without them its output would change. An unknown project is a 404 with code
    `unknown_project`, so the CLI can tell it from "no match" (`not_found`).
  - Trace counts stop at 100000 spans, 50 errors and 1000 logs, the limits of the calls
    `mallard find` used, so its numbers cannot change. Each is read on its own: a failed
    logs read still shows the trace, with a warning.
  - Default window of `/apps` and `mallard apps`: 24h, so an app that reports now and
    then stays listed. Pickers send their own.
  - A window above `otel.query.max_window` is cut to its last `max_window`, plus a
    warning. Projects ignore the window, so Sentry's `max_window` does not matter.
  - Service names come from all six tables, so an app with only logs or metrics is
    listed; at most 1000, then a warning. They come from SDKs.
  - Both failing is the first error (5xx): there is no list to give.
  - One behaviour change in `mallard find`, as the story asks: before, a failed errors
    or logs lookup next to empty answers read as "not found" (exit 4). Now it is the
    side's error (exit 1 or 6).
- Measured on 2026-11-01. Lake: 150 M spans, 43 M logs, 12.6 M metric points over 30 days
  (`mallard-otel-loadgen --backfill 30d --rate 5`), 5 M events (`mallard-sentry-loadgen`),
  14 GB, DuckDB-file catalog on a local disk. `mallard serve` on an Apple M5 Pro with
  `duckdb.threads = 4`, `memory_limit = 2GB`, one request at a time, median of 3, ms.
  With ingest on: 2,200 spans/s plus 120 events/s.

  | window | `/api/v1/apps` quiet | with ingest | `/otel/api/v1/services` quiet |
  |---|---:|---:|---:|
  | 1h | 31 | 36 | 15 |
  | 24h | 41 | 51 | 130 |
  | 7d | 125 | 134 | 979 |
  | 30d | 443 | 455 | 6256 |

  `/api/v1/find`: 88 ms for a random 32 hex id (no match, every source read), 68 ms for a
  random 16 hex id, 82 ms for a trace, 85 ms for an event, 112 ms for an issue.
  With ingest on and 4 clients asking wide questions in a loop, `/apps?since=30d` is
  1.4 s (max 2.2 s); with 16 clients 5.3 s (max 6.9 s), queued behind the 4 reader
  connections. Effect on ingest: see story 02, which ran them together.
- Checked live against `mallard serve`: `curl` and the CLI for apps and find (all kinds),
  a 400-day window (9 apps and the warning), no token (401).

## Follow-ups
- `mallard-otel-api`'s query structs and so the client still hold one `service`: the
  query string format cannot serialize a list. Story 03 needs a way to send several for
  `--app`, for example the `#[serde(skip)]` plus extra `.query()` the `tag` filters use.
- `mallard-web` (stories 04, 05): `/api/v1/*` is not yet in its proxy (`allowed()` and
  the routes in `crates/mallard-web/src`), its `api-types` script lacks
  `docs/openapi.json`, and `qs()` in `api/client.ts` cannot repeat a parameter.
- [issue 0142](../../issues/0142-project-slugs-with-underscore-split-apps.md): a project
  slug with `_` never joins its service.
- [issue 0143](../../issues/0143-find-cli-hides-warnings.md): `mallard find` does not show
  `warnings`.
