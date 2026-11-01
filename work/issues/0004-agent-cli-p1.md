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

## Progress
- 2026-11-01, Sentry side (branch `feat/cli-p1-sentry`), done:
  - S9: `issues?tag=k:v` (repeat, all must match, max 10) and
    `events?tag=&release=&environment=&query=` (window flags already worked);
    CLI `--tag k=v` on `issues list` and `events list`, plus `--release`,
    `--env`, `-q` on `events list`. Text search is `query` (like `issues`),
    not `q`.
  - S10: `issues` pages: stable order (sort value, then id) and
    `next_cursor`; the cursor carries the window and the sort. CLI
    `--cursor` on `issues list` and `events list` (events already had one).
  - S14: `GET /v1/projects/{p}/releases` (from events; `new_issues` = issues
    with that `first_release`); CLI `releases list`. The sentry-cli release
    endpoints are still stubs (noted in `docs/sentry/compatibility.md` and
    the agent guide).
  - S15: `POST /v1/sql` over `events`, `issues`, `issue_activity`, off by
    default (`[sentry.query] enable_sql`, `sql_timeout`); the guard moved to
    `mallard_duck::sql` and the otel side uses it too (ADR 0010). CLI
    `mallard sql --errors "<select>"`.
  - Not done here: `status` does not report the Sentry side's SQL switch
    (exit 7 tells); `odl-cli`'s SQL table printer could move to
    `mallard-term` and be shared with `sdl-cli`'s.

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
