---
status: in-progress
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

## Polish found in review (2026-11-01; done, see Progress)
- Not-found errors repeat themselves: `not found: issue not found`
  (`mallard-term` describe prefix + the server message).

## Progress
- 2026-11-01, client-side items done (branch `feat/cli-p1-client`); the
  server-backed items (`schema`, `--tag`, span ids in `find`, cursors,
  `releases list`, SQL over Sentry) are separate work:
  - `--wait <dur>` on `events list`, `traces list`, `logs`, `trace`/`traces
    show`, `find` (`mallard_term::wait`): polls every second, the window
    moves along; on timeout the empty result is printed and the exit code is
    8 (`Kind::Unmet`, `condition_not_met`). Errors end the wait at once.
  - `logs -f --for <dur>` (exit 0) and `--until-match <text>` (body,
    case-insensitive; exit 0 at the first match; with `--for` and no match:
    exit 8). Both need `-f`; `-f` and `--wait` exclude each other.
  - `ops --compare <dur>`: two `operations` calls, joined on service, name
    and kind; adds `span_count_before`, `p95_ms_before`, `p95_change_pct`,
    `error_rate_before`, `error_rate_change` (null without spans then).
  - `series spans|logs|errors` (`crates/mallard/src/series.rs`): the list
    envelope, one item per bucket with `time`; default window 1h.
  - `setup --sentry-cli`: `SENTRY_URL` = the server root (sentry-cli refuses
    a path), `SENTRY_ORG=default` (the server ignores the org),
    `SENTRY_PROJECT`, `SENTRY_AUTH_TOKEN` = the CLI's token. Found: mallard
    has no `/api/0` at the root, so sentry-cli cannot upload yet →
    [issue 0007](0007-sentry-cli-needs-root-url.md).
  - `--quiet` (global; no `-q`: that is `--query` on `issues list`, `logs`,
    `metrics list`): drops `note:` lines (`Output::note`): truncation,
    waiting, the 24h fallback, `setup`'s "project exists".
  - Polish: a 404 with a server message prints the message alone
    (`issue not found`, not `not found: issue not found`), both sides.

