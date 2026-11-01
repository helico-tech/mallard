---
status: done
created: 2026-11-01
---
# tui

## Goal
TUI as embeddable component (Embedded) + standalone odl tui; overview, traces/waterfall, logs tail, metrics charts, status.

## Acceptance criteria
- Built, tested (cargo test), documented.

## Plan
See epic and docs/architecture.md.

## Progress log
- 2026-11-01: created.
- 2026-11-01: Built as `tui::Embedded` component + `run_standalone`; 5 views; 24 tests; live-checked against a 68M-span lake.
- 2026-11-01: Review found loader issues (slow server, retries, Ctrl/Alt keys); fixed in the client-fixes pass.

## Follow-ups
