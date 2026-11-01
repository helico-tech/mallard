---
status: done
created: 2026-11-01
---
# Epic 0003 — mallard web: the TUI in a browser

## Goal
A Web UI for mallard, `mallard web` (one runnable, UI built in), that follows
the same principles as `mallard tui` and looks very good: React, Kumo
(Cloudflare's design system), shadcn/ui where Kumo has no component.

## Scope
- Feature parity with `mallard tui`: every screen, the same keys, formats,
  windows and jumps (inventory in `docs/web/parity.md`).
- UI/UX guidelines for data-heavy apps (`docs/web/ui-guidelines.md`).
- Not in scope: new query features the TUI lacks (SQL, setup, releases),
  a command palette, a Helm/compose service (issue 0070).

## Done when
- `mallard web` runs alone with the UI inside, against a real `mallard-server`.
- Every parity row is checked in a browser (Playwright tests in the repo).
- Independent review of UX, security and parity is done; findings fixed or filed.
- Build, tests, clippy, fmt pass; CI builds the UI.

## Stories
1. [01-server-and-shell](01-server-and-shell.md)
2. [02-errors](02-errors.md)
3. [03-telemetry](03-telemetry.md)
4. [04-review](04-review.md)
