# 0016 — The Web UI is a client like the TUI: `mallard web`

## Status
accepted

## Context
mallard needs a Web UI that follows the same principles as `mallard tui`
(ADR 0002, 0006, 0008). The query APIs have no CORS layer, so a browser can
only call them from the same origin, and both require a bearer token. Holding
a token in browser code would let any injected script read it.

## Decision
- A new library crate `mallard-web` holds the Web UI; `mallard web` runs it. It speaks
  HTTP to `mallard-server`, exactly like the TUI: no DuckDB, no server code,
  and the server gains no route, no CORS and no static files (ADR 0007).
- The React UI lives in `crates/mallard-web/ui` and is compiled into the binary
  (`build.rs` generates `include_bytes!` for `ui/dist`). Without a built UI the
  crate still compiles and serves a page saying how to build it, so
  `cargo build --workspace` does not need Node.
- The proxy passes only what the TUI uses: `GET /sentry/v1/*`,
  `GET /otel/api/v1/*`, `PATCH /sentry/v1/projects/{p}/issues/{id}`.
  SQL, maintenance, project creation and ingest are not reachable.
- Credentials, two modes:
  - `--token` or `MALLARD_TOKEN` (as the CLI): the proxy adds it. Only allowed on a
    loopback address, because anyone who can reach the port gets the token's rights.
    The Host must be a loopback name, which stops DNS rebinding.
  - No token: users sign in with an API token. It is checked against the server
    and kept in memory for 12 hours. The `HttpOnly`, `SameSite=Strict` cookie holds
    only a random id, because cookies are not isolated by port: another service on
    the host may see it. Off this machine the cookie is `Secure` and `__Host-`
    prefixed, so plain HTTP is refused by the browser and the page says so.
- Writes need a custom header and a same-origin `Origin`, which a cross-site
  form or fetch cannot produce. Cross-site and same-site requests may not reach
  the API at all (`Sec-Fetch-Site`), so a timing difference tells them nothing.
  Every page has a strict CSP without inline scripts.
- UI stack: Kumo first (Cloudflare's design system; components and tokens).
  shadcn/ui only where Kumo has nothing, on the same Base UI primitives and
  mapped onto Kumo's tokens, so the two read as one system.
- API types are generated from `docs/*/openapi.json`.
- Each side has its own back stack, as in the TUI (`app/history.ts`); browser
  history is not what Esc follows.
- TUI principles carry over, each in one place: all times UTC; one format
  module (`lib/format.ts`, same outputs as `mallard_term::fmt`); all server text
  cleaned in the API client (`lib/clean.ts`, same rules as `text::clean_text`);
  one key map; writes are capitals; failed refreshes keep the data.

## Consequences
- `mallard` gains axum and the embedded UI, a few MB; it stays free of DuckDB
  (`cargo tree -p mallard` has no duckdb). A release build needs the UI built first
  (Docker and CI do so), or `mallard web` shows how to build it.
- Sign-in needs HTTPS off this machine. A restart signs everyone out, and two
  replicas behind a balancer do not share sessions.
- Kumo's light theme misses WCAG AA in three text tokens and its primary button
  (white on brand, 4.0:1). `index.css` overrides the three text tokens; the button
  is left to upstream.
- Adding a write to the UI means adding it to the proxy allowlist on purpose.
