---
status: todo
created: 2026-11-01
---
# 04 — Web: global scope, suggestions, Cmd-K

## Goal
One app picker and one time range for the whole web UI, a filter bar that suggests,
and a box that opens any ID.

## Acceptance criteria
- App picker in the shell, from `/api/v1/apps`. It scopes both sides, and a side an
  app lacks shows a clear empty state with the fix (DSN or `OTEL_SERVICE_NAME`).
- App and window live in the URL, stay the same across tabs, and shared links work.
- **Rule change:** one window for both sides replaces "each side keeps its own"
  (`src/app/window.tsx`, its Playwright test, `docs/web/parity.md`). Presets are the
  union of both lists. Check each side's `max_window` first: a preset one side
  refuses must not end in an error screen. Default 1h; 24h was the errors default,
  so check that an app with older issues does not look empty.
- Filter bar suggests keys, then values with counts, using keyboard only if wanted.
  Follow `docs/web/ui-guidelines.md`. It must not fight the existing key handling.
- Cmd-K opens a search box: paste an ID and land on it through `/api/v1/find`; type a
  name and get matching apps.
- Unit and Playwright tests; 36 existing tests still pass or changed on purpose.

## Plan
After stories 1 and 2. Regenerate API types from `docs/openapi.json`.

## Progress log

## Follow-ups
