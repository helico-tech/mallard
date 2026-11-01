---
status: done
created: 2026-10-31
---
# 03 — Correlation API

## Goal
Show errors and traces together by trace id.

## Acceptance criteria
- `obs_client::Client::trace_overview(trace_id)`: spans + Sentry events + logs.
- Issue/event → trace via the event's trace_id.

## Plan
Done client-side in obs-client on top of both library APIs (sentry added `GET /v1/events?trace_id=` at our request). No server endpoint: keeps obs-server free of the libraries' table layout.

## Progress log
- 2026-11-01: `obs_client::Client::trace_overview` (spans + errors + logs in parallel, 404 → empty) and `obs trace <id>` (span tree with errors and logs under their span). Covered by the e2e test.

## Follow-ups
