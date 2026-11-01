---
status: todo
created: 2026-11-01
---
# 03 — Sentry transactions and spans become traces

## Goal
Sentry SDK performance data (transaction events with spans) is stored as OTel spans in otel.spans, so apps that only use a Sentry SDK get traces next to their errors. Today sdl-server drops transactions.

## Acceptance criteria
- A transaction envelope from @sentry/node shows up in `mallard telemetry traces` and `mallard trace <id>` with its child spans, status and attributes.
- Errors from the same request appear under the right span.
- Mapping documented (op/description/status/tags/data → OTel fields).

## Plan

## Progress log

## Follow-ups
