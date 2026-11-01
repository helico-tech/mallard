---
status: done
created: 2026-11-01
---
# 05 — Web: app home page

## Goal
Open an app and see at once whether it is healthy.

## Acceptance criteria
- One page for the selected app: error rate, latency, new issues, worst operations,
  each linking to its detail view, for the global window.
- Uses existing endpoints: `spans/series?service=` and `operations?service=` for error
  rate and latency (`/stats` is ingest counters, `/services` has no filter), and the
  project's `stats` and `issues?new=true`. A new endpoint only if a measured need shows up.
- It is the Overview screen with an app selected. Overview across services stays for
  All apps, so there are not two nearly equal pages.
- A one-sided app shows what it has, not a broken page.
- Follows `docs/web/ui-guidelines.md`; has Playwright coverage.

## Plan
After story 04.

## Progress log
- 2026-11-01: Built on those endpoints; no new one. Each number links to what it counts.
  "Worst" is failing first, then slowest. Overview moved out of the Telemetry group in the
  side nav, because it now shows errors too; keys and address did not change.

## Follow-ups
