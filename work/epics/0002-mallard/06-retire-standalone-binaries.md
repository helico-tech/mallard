---
status: todo
created: 2026-11-01
---
# 06 — Retire the standalone binaries

## Goal
The e2e suite and load generators run against `mallard`, so `sentry-ducklake`, `otel-ducklake`, `sdl` and `odl` binaries can go.

## Acceptance criteria
- e2e/sentry runs against `mallard serve` (paths under /sentry).
- Load generators target mallard.
- Only the `mallard` binary is built.

## Plan

## Progress log

## Follow-ups
