# 0002. Sentry protocol scope: errors only, everything else accepted and dropped

## Status
accepted (2026-10-31)

## Context
The goal is "swap the DSN and it works" for error tracking. Modern SDKs also
send transactions, sessions, client reports, replays, profiles, check-ins,
logs and attachments in the same envelopes. Rejecting them makes SDKs log
errors or retry; storing them is a much larger product (APM, replay).

## Decision
- Implement `/api/{id}/envelope/` and legacy `/api/{id}/store/`, all auth
  styles, all request compressions, CORS for browsers.
- Store `event` items (errors and messages). Accept every other item type
  with `200` and drop it (counted in `sdl_ingest_items_dropped_total`).
- Implement the sentry-cli / bundler plugin sourcemap upload protocol
  (chunk upload + artifact bundle assemble, synchronous `ok`), and answer
  release create/finalize/deploy calls without storing releases.
- Do not implement the Sentry web API (`/api/0/projects/...` issues etc.); our
  own `/v1` API (OpenAPI documented) serves queries.

## Consequences
- Existing SDK configs (incl. tracesSampleRate, replays) keep working unchanged.
- Tools that read the Sentry web API (e.g. Sentry's own UI, sentry-cli
  `issues list`) do not work against this server.
- Traces can be linked through `trace_id` with an OTel backend (obs-ducklake).
