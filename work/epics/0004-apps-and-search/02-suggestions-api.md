---
status: done
created: 2026-11-01
---
# 02 — Suggestions API

## Goal
Everything a filter bar needs to suggest. The telemetry side has it
(`/api/v1/services`, `/operations`, `/attributes`). The errors side has no tag
list, only tag filters on issues and events.

## Acceptance criteria
- Errors side: tag keys with counts for a project and window, and the top values
  of one key, with an optional prefix. In `docs/sentry/openapi.json`.
- Telemetry `/attributes` has a prefix filter for values if it lacks one.
- Limited by window and row count; a wide query cannot slow ingest or take the
  connection for long. State the measured time on a large lake.
- Reads go through the existing read path, with the same limits as other queries.

## Plan
See how tags are stored for `tag_filters` in `crates/mallard-sentry-server/src/api.rs`
and read `docs/sentry/data-model.md`. Copy the shape of the telemetry attributes
endpoint, so both sides look alike to a client.

Steps (2026-11-01):
1. `GET /sentry/v1/projects/{project}/tags?key=&prefix=&limit=` plus the window, with
   the same answer as `/attributes`: `total_rows`, `sampled_rows`, `items`. Runs on
   `read_for`, so a timeout interrupts it and the connection comes back.
2. `/otel/api/v1/attributes` gets `prefix`: keys, or with `key` the values, that start
   with it (case-insensitive, `starts_with`, a bound parameter, no LIKE).
3. Measure both on a large lake with ingest on, compare the writer with and without
   the queries, then decide if the sample needs a row cap on the scan.

## Progress log
- 2026-11-01: `GET /sentry/v1/projects/{project}/tags` and `prefix` on
  `/otel/api/v1/attributes`, as planned. Both match any case, with `starts_with` on a
  bound parameter, so `%` and quotes are plain text. `prefix` also applies to keys.
- First measurement, on the lake of story 01: `/attributes?since=30d&key=…` took **12.4 s**
  with 4 threads, every one reading the maps of 150 M rows. That is what the criteria
  rule out, so both endpoints now read only the newest part of the window with at
  least 500,000 rows, at the steps 1h, 6h, 1d, 7d (`Window::newest_first`). The rows
  are counted first, on cheap columns only. The answer says where it starts:
  `counted_from`, and `total_rows` counts from there. A value seen only before that is
  not suggested: a choice of cost over completeness, and the filter still accepts it.
  A narrow window, a quiet lake or a quiet service reads the whole window as before.
  `/attributes` also gives this to `mallard schema attrs`: on a busy lake its "sample of
  N of M rows" counts from there, and `--json` gets `counted_from`.
- Limits: 10,000 sampled rows, `limit` at most 1000, 500,000 rows before stopping. Time:
  `read_for` with 10 s on the errors side, `otel.query.timeout` (30 s) on the telemetry
  side. Both interrupt the query and give the connection back.
- Measured with the setup of story 01 (median of 3, ms), window 30d. On the errors side
  the 5 M events all fall in the newest hour, so this is 5 M rows, the worst case:

  | endpoint, `since=30d` | before the change, quiet | quiet | with ingest |
  |---|---:|---:|---:|
  | attributes, keys | 5822 | 104 | 120 |
  | attributes, `key=http.route` | 12227 | 148 | 171 |
  | attributes, `key=` + `prefix=/api` | 12417 | 146 | 182 |
  | attributes, `key=` + `prefix=zzz` (no match) | 12421 | 139 | 175 |
  | attributes, `service=frontend&key=` | 5039 | 240 | 359 |
  | tags, keys | 201 | 226 | 238 |
  | tags, `key=tenant` | 373 | 456 | 434 |
  | tags, `key=url&prefix=https` | 410 | 519 | 498 |
  | tags, `key=tenant&prefix=zzz` (no match) | 364 | 435 | 427 |

- Do they starve the writer (ADR 0001)? Live ingest for 120 s each (2,242 spans/s, 121
  events/s, 4 DuckDB threads), with clients asking the wide `/apps`, `/attributes` and
  `/tags` questions in a loop:

  | clients | OTLP flush, mean | Sentry batch, mean | Sentry request p50 / p99 | throttled, failed | queries, median |
  |---:|---:|---:|---:|---:|---:|
  | 0 | 80 ms | 25 ms | 527 / 617 ms | 0, 0 | |
  | 4 | 132 ms | 25 ms | 529 / 555 ms | 0, 0 | 723 answered, 0.2-1.4 s |
  | 0 | 86 ms | 25 ms | 527 / 665 ms | 0, 0 | |
  | 16 | 81 ms | 24 ms | 534 / 582 ms | 0, 0 | 621 answered, 1.4-5.3 s |

  No: the same events and spans per second, nothing throttled. Queries wait for one
  another, not the writer. Not measured: a saturated writer, or object storage.

## Follow-ups
- [issue 0144](../../issues/0144-otel-flush-conflicts-with-sentry-ingest.md): while filling
  the lake, OTLP ingest ran far slower next to saturated Sentry ingest.
- Story 03 and 04 can show `counted_from`, or leave it out. Debounce typing anyway: a
  value query is 0.15-0.5 s.
