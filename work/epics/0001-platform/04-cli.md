---
status: done
created: 2026-10-31
---
# 04 — obs CLI

## Goal
One CLI over both libraries plus correlation.

## Acceptance criteria
- obs status | trace <id> | sentry <sdl commands> | otel <odl commands> | tui.
- --output table|json; base URL and token from flags, env or config file.

## Plan

## Progress log
- 2026-11-01: `obs status`, `obs trace` (own); `obs otel …` = odl_cli (c93b8ee), `obs sentry …` = sdl_cli (b261f40), per ADR 0002. Checked against local server and the compose stack.

## Follow-ups
