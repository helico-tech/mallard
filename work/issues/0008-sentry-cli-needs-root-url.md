---
status: done
type: bug
created: 2026-11-01
---
# sentry-cli cannot use mallard: it needs `/api/0` at the URL root

## Problem
mallard serves the sentry-cli routes (`/api/0/organizations/{org}/…`) only
under `/sentry` (`mallard-server` nests the whole `sdl-server` router there).
The README says `sentry-cli --url http://host:8080/sentry`, but sentry-cli
2.x and 3.x refuse a URL with a path: `get_base_url` fails with
"bad sentry url: not on URL root" when the URL (without trailing `/`) has
more than two `/` (getsentry/sentry-cli `src/config.rs`; see also
`docs/sentry/references/sentry-sourcemap-protocol.md`, "Base URL"). The
`e2e/sentry` suite does not catch it: it runs the standalone
`sentry-ducklake`, which serves `/api/0` at its root. The Vite plugin runs
the same binary.

`mallard setup --sentry-cli` (issue 0004) prints `SENTRY_URL=<server root>`,
the only form sentry-cli accepts; uploads with it get 404 until this is fixed.

## How to reproduce
```sh
mallard setup web --sentry-cli >> .env && set -a && . ./.env
npx sentry-cli sourcemaps upload ./dist          # 404 on /api/0/organizations/default/chunk-upload/
npx sentry-cli --url "$SENTRY_URL/sentry" sourcemaps upload ./dist   # bad sentry url: not on URL root
```

## Notes
- Fix: `sdl-server` exposes the `/api/0` compat routes as their own router;
  `mallard-server` merges them at the root as well as under `/sentry`. The
  `chunk-upload` `url` the server returns (from `public_url`) can stay as it is.
- Then fix the README line and add a `mallard-server` test that calls
  `/api/0/organizations/x/chunk-upload/` at the root.

Done 2026-11-01 in feat/e2e-mallard (8244369): mallard-server also serves `/api/0/...` on the root of the HTTP port (`nest_service`, prefix restored); `/sentry/api/0` still works. Test: `crates/mallard-server/tests/sentry_cli_root.rs`; the e2e sourcemap tests run sentry-cli 2.x and 3.x against the root URL.
