---
status: done
created: 2026-11-01
---
# Story 04 — Review with Playwright

## Goal
Independent subagents check the running app in a browser and judge whether it
makes sense: parity, UX against the guidelines, security.

## Acceptance criteria
- Playwright e2e tests in `crates/mallard-web/ui/e2e` against a real server.
- Findings fixed or filed as issues. Screenshots reviewed in light and dark.

## Plan
1. Seed a real server. 2. Reviewers drive it with Playwright. 3. Fix. 4. Re-check.

## Progress log
- 2026-11-01: three independent reviews with Playwright: design, parity, security.
  Findings fixed, regressions covered in `ui/e2e`. Not fixed, on purpose: Kumo's own
  primary-button contrast and hover transitions (upstream); filed items in issue 0070.

## Follow-ups
