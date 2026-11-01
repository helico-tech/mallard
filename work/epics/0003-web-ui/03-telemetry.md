---
status: done
created: 2026-11-01
---
# Story 03 — Telemetry tab

## Goal
Overview, traces, waterfall, logs, metrics and status, as in the Telemetry tab.

## Acceptance criteria
- Same filter language, window presets, cadences and drill-downs as the TUI.
- Waterfall with span panel, Sentry error markers, jumps both ways.
- Logs: volume, follow (2s poll with dedupe, gap row), record panel, 5000 cap.

## Plan
Port screen by screen from `docs/web/parity.md`.

## Progress log
- 2026-11-01: all screens built on Kumo, covered by `ui/e2e`.

## Follow-ups
