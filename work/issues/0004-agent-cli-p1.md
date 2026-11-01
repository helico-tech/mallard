---
status: todo
type: idea
created: 2026-11-01
---
# Agent CLI: the P1 list

## Problem
Story 0002/08 implemented the P0 list of [the spec](../../docs/specs/agent-cli.md)
(§9). The P1 items are still open; each is small and independent:

- `mallard schema` (tables/columns) and `schema attrs` (attribute keys and
  top values per signal) — needs otel S12 and S13.
- `--wait <dur>` on `events list`, `traces list`, `logs`, `trace`, `find`:
  poll until there is a result, exit 8 on timeout (ingest is async).
- `logs -f --for 2m` / `--until-match <text>`: a follow that stops by itself.
- `ops --compare 1d`: p95 and error rate before/after (client-side join).
- `series spans|logs|errors`: counts and p95 per bucket (existing
  `spans/series`, `logs/series`, `stats` endpoints).
- `--tag k=v` filters on issues and events (sentry S9).
- Span ids in `find` (otel S8: span by id).
- `--cursor` to continue lists (sentry S10 on issues; otel S11 on logs/traces);
  `next_cursor` is already in the envelope where the server has one.
- `releases list` (sentry S14), SQL over Sentry tables (S15, needs an ADR).
- `setup --sentry-cli` (prints `SENTRY_URL`, `SENTRY_ORG`, `SENTRY_PROJECT`,
  `SENTRY_AUTH_TOKEN`), `-q/--quiet`.

## Notes
- `status` reports a side as up when its API answers; it does not call
  `/readyz` (a failing otel flush shows in `otel.info.ingest`, not in `ok`).
- `capabilities` has no OTLP URLs: the server knows its listen addresses,
  not the public host. `setup` uses the Sentry `public_url` (without
  `/sentry`), else the CLI's `--url`.
- `maintenance` and `stats` are only under the hidden `mallard errors …`.
- Spec side findings still open: breadcrumb order in `events show` text,
  release endpoints are stubs.

## Polish found in review (2026-11-01)
- Not-found errors repeat themselves: `not found: issue not found`
  (`mallard-term` describe prefix + the server message).

## Progress
- 2026-11-01, otel side (branch `feat/cli-p1-otel`) — done:
  - S8 span by id: `GET /otel/api/v1/spans/{span_id}` (default window 7d,
    newest span on a collision); `mallard find <16 hex>` looks up the issue
    and the span at the same time; a span match has `trace_id` and
    `next: mallard trace <trace id>`.
  - S11 cursors on `GET /api/v1/traces` and `/logs`: the body stays an
    array; the next page's cursor is the `x-next-cursor` header, sent back
    as `cursor=`. Traces page on the newest matching span + trace id, logs
    on time + DuckLake row id. `traces list` and
    `logs` take `--cursor`; `next_cursor` in the envelope, and in the stderr
    note for text/ndjson. `truncated` there is now exact (from the server).
  - S12 `GET /api/v1/attributes?signal=spans|logs|metrics&service=&metric=&key=&limit=`:
    keys with row counts, or the top values of `key`, from a reservoir
    sample of at most 10,000 matching rows (`total_rows`, `sampled_rows`);
    attributes and resource attributes count together. `mallard schema attrs`.
  - S13 `GET /api/v1/schema`: the SQL tables, their time column and their
    columns from the catalog. `mallard schema` (= `schema tables`). When
    Sentry SQL (S15) lands, the host's `schema` handler in
    `crates/mallard/src/main.rs` can add that side's tables.
