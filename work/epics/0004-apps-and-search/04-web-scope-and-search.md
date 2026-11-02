---
status: done
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
  union of both lists, cut at runtime to the smaller `max_window_secs` both sides
  report: a preset one side refuses must not end in an error screen. Default 1h; 24h
  was the errors default, so an empty issue list offers "Show last 24h".
- "All apps" is the default and a choice in the picker (ADR 0028). The errors tab then
  asks you to pick an app. `/api/v1/apps` and `/api/v1/find` are in the proxy allowlist
  and its cross-site guard, with tests. The Status page stays global.
- No `app=` in the filter language (ADR 0028).
- Filter bar suggests keys, then values with counts, using keyboard only if wanted.
  Follow `docs/web/ui-guidelines.md`. It must not fight the existing key handling.
- Cmd-K opens a search box: paste an ID and land on it through `/api/v1/find`; type a
  name and get matching apps.
- Unit and Playwright tests; 36 existing tests still pass or changed on purpose.

## Plan
After stories 1 and 2. Regenerate API types from `docs/openapi.json`.

## Progress log
- 2026-11-01: Built. Decisions the criteria left open:
  - An address without `app` or `w` keeps the current one, and `?app=` means All apps.
    On the errors side the project in the path wins, so old links still work.
  - A `service=` filter narrows an app and cannot leave it: a service outside the app
    says so and reads nothing. `trace=` on logs is not scoped: it asks by id.
  - Issue search gets `tag=value` tokens, because the errors side had no filter to
    suggest in. A text search for `a=b` now means a tag.
  - Suggestions never take anything until you move to one, so Enter still applies and
    Tab still moves the focus (`docs/web/parity.md`). Requests wait 200 ms after typing.
    `counted_from` is shown only when it cuts the window short.
  - Operations for `op=` are read over at most 24h: they are costly on wide windows.
  - Cmd-K lands on an id at once when it has one match, and lists several. It looks over
    the longest preset (30d by default), not `find`'s 7d.
  - Removed: remembering the last project in `localStorage`, since the address holds it.
- 2026-11-01: CI failed two new browser tests: keys pressed before the screen had
  loaded were lost. That is the tests: 4 of them did not wait for data, as the others do.
  A key in that moment does nothing harmful, so the app stays as it is. Running everything
  at 6x CPU throttle then found a real bug, in the app: address writes started from the
  render's parameters, which can lag behind the address, so a quick `c` could be undone.
  They now start from the address itself (`writeParams`), with a test at that throttle.

## Follow-ups
- [issue 0142](../../issues/0142-project-slugs-with-underscore-split-apps.md) shows here
  as two apps in the picker.
