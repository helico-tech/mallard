---
status: done
created: 2026-11-01
---
# Epic 0004 — One app concept, and search that helps

## Goal
A person thinks in programs, not in "projects" and "services". Show one list of
apps, scope everything to one app and one time range, and suggest values while
typing a filter. The owner agreed this on 2026-11-01. ADR 0027 and ADR 0028 have
the decisions.

## Scope
- Server: list of apps, tag suggestions for the errors side, one find endpoint.
- TUI and CLI: `--app`, suggestions in the filter bar.
- Web UI: global app and time range, suggestions, Cmd-K, app home page.
- Not in scope, from the same brainstorm (later epics if wanted): click to
  filter, errors and traces inline, release markers, better empty states, copy
  as CLI command, compare and exemplars, ingest health banner, calmer live views.
- Not in scope: a stored mapping between projects and services (ADR 0027).

## Done when
- One app picker scopes every tab in the web UI and the TUI.
- Filter bars suggest keys and values on both sides, fast on a large lake.
- Paste any trace, event or issue ID in Cmd-K and land on it.
- A new app has a home page with errors, latency and worst operations.
- Existing flags, API paths and DSNs still work. Tests pass; CI is green.

## Stories
1. [01-apps-and-find-api](01-apps-and-find-api.md): server. Owner: mallard-core.
2. [02-suggestions-api](02-suggestions-api.md): server. Owner: mallard-core.
3. [03-tui-and-cli](03-tui-and-cli.md): TUI, CLI. Owner: mallard-core.
4. [04-web-scope-and-search](04-web-scope-and-search.md): web. Owner: mallard-web.
5. [05-web-app-home](05-web-app-home.md): web. Owner: mallard-web.

Stories 1 and 2 come first; the rest use them.
