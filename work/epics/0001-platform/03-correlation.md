---
status: todo
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

## Follow-ups
