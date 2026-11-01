# mallard-web

The Web UI for mallard, run by `mallard web`: the same screens, keys and rules as
`mallard tui`, in a browser, with the UI built into the binary. Decisions: [ADR 0016](../../docs/adr/0016-web-ui.md).
Design rules: [docs/web/ui-guidelines.md](../../docs/web/ui-guidelines.md),
what it does compared with the TUI: [docs/web/parity.md](../../docs/web/parity.md).

## Run

```sh
cd crates/mallard-web/ui && npm ci && npm run build && cd -   # once, and after UI changes
cargo run -p mallard -- web --url http://localhost:8080 --token dev   # http://127.0.0.1:8088
```

| flag | env | meaning |
|---|---|---|
| `--url` | `MALLARD_URL` | server base URL |
| `--token` | `MALLARD_TOKEN` | API token, as the CLI. Only on a loopback address: everyone who can reach the port gets its rights |
| `--addr` | `MALLARD_WEB_ADDR` | where to listen, default `127.0.0.1:8088` |

Without a token, people sign in with an API token and it may listen anywhere. Off
this machine it needs HTTPS: the session cookie is `Secure`, so put TLS in front.
Signing in keeps the token in memory (12 hours, or until a restart); the cookie holds
only a random id.

## Develop

```sh
MALLARD_URL=… MALLARD_TOKEN=… cargo run -p mallard -- web   # the API, on :8088
cd crates/mallard-web/ui && npm run dev                  # hot reload on :5173, proxies to :8088
npm test                                                 # unit tests: formats, cleaning, filters, waterfall, tail
npm run e2e        # browser tests; first: npm run build && cargo build -p mallard
npm run api-types  # after API changes: types from docs/*/openapi.json
```

## Rules

- Kumo first (Cloudflare's design system): its components, tokens and type scale.
  The rules in `.claude/skills/kumo-design` bind. shadcn/ui only where Kumo has
  nothing (`Kbd`, `Resizable`). No hard-coded colours, sizes or one-off table CSS.
- Screens are built from `components/ui-bits.tsx`: `Page`, `Section`, `DataTable`,
  `SearchField`, `DetailHeader`, `Facts`. Columns answer to the card's width
  (`@xl:table-cell`), the main column stays visible.
- All server text goes through `api/client.ts`, which cleans it (`lib/clean.ts` =
  `mallard_term::text`). Formats live in `lib/format.ts` (= `mallard_term::fmt`).
  All times are UTC. Keys live in `app/keys.tsx`; writes are capitals.
- A new write means adding it to the proxy allowlist in `src/lib.rs` on purpose.
