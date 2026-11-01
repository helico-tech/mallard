---
status: done
created: 2026-11-01
---
# Story 01 — Server and shell

## Goal
`mallard web` (ADR 0016) and the React shell every screen sits in.

## Acceptance criteria
- `cargo build -p mallard` works without Node (a page says how to build the UI).
- Same-origin proxy for `GET /sentry/v1/*`, `GET /otel/api/v1/*` and
  `PATCH /sentry/v1/projects/{p}/issues/{id}` only.
- The token never reaches browser code. `--token` works only on loopback.
- Shell: side nav, window control, live state, keys, help, theme, login.

## Plan
1. Crate: proxy, session, embedded assets, security headers, tests.
2. UI lib: `clean`, formats, filter language, waterfall (ports with tests).
3. Shell, key map, query client, shared table and chart pieces.

## Progress log
- 2026-11-01: fixed two dangling `$ref`s in the otel OpenAPI document, which
  blocked type generation (`LogOrder`, `AttributeSignal` were not registered).

- 2026-11-01: moved the whole UI onto Kumo after the owner's feedback on consistency:
  its Table, LayerCard, Badge, Breadcrumbs, InputGroup, tokens, type scale and chart
  palette, replacing hand-made CSS. shadcn stays only for Kbd and Resizable.
- 2026-11-01: browser tests found what screenshots could not: toasts blocked every
  shortcut (a toast is a `role=dialog`), and the sign-in field never updated.
- 2026-11-01: security review: the cookie held the token, and cookies are not isolated
  by port, so it now holds a random id; `Secure` off this machine; iterative span tree
  and an error boundary; virtualised JSON; cross-site reads refused.
- 2026-11-01: owner chose `mallard web`: the crate is a library, no binary of its own.

## Follow-ups
- [issue 0070](../../issues/0070-web-deploy-and-mallard-web-command.md)
