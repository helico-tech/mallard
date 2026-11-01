# End-to-end tests

These tests prove that real Sentry tools work against sentry-ducklake when
you change only the DSN / URL:

| File | What it proves |
|---|---|
| `tests/node-sdk.test.mjs` | `@sentry/node`: cause chains, messages with level, breadcrumbs, user/tags/contexts, custom fingerprints (`{{ default }}` too), release/environment, 3 events -> 1 issue, resolve -> regression, `trace_id` inside a span, uncaught exception and unhandled rejection in a child process. |
| `tests/browser-sourcemaps.test.mjs` | A minified Vite + TypeScript app (`browser-app/`) in headless Chromium, served from a different origin than the server (CORS, `fetch` transport). Sourcemaps uploaded with the Rust `sentry-cli` 3.x (and 2.x): debug IDs (`sourcemaps inject`), `--release`, release-only (no debug IDs) and `--url-prefix`. Asserts original `.ts` file names, function names, line numbers and source context, `raw_stacktrace` with the minified frames, and that a rebuild with a new bundle hash stays in the same issue. |
| `tests/vite-plugin.test.mjs` | `@sentry/vite-plugin` (uploads with the new TypeScript `sentry` CLI) with `url`, `authToken`, `org`, `project`; release injection; symbolication after the maps were deleted from the build. |
| `tests/unhappy.test.mjs` | Wrong key -> 401 (SDK keeps running), unknown project -> 404, gzip envelopes, bad gzip -> 400, query-string and envelope-DSN auth, legacy `/store/` (JSON and base64+zlib), CORS preflight, bearer token on the API. |

Every assertion reads stored data back through the `/v1` API.

## Run

Prerequisites: Node.js 22+ (tested with 24), a Rust toolchain (to build the
server), and network access for `npm ci` and the Chromium download.

```sh
cd e2e
npm ci
npx playwright install chromium
npm test
```

`npm test` first type-checks `browser-app/` with `tsc`. Run one file:
`node --test tests/browser-sourcemaps.test.mjs`.

## How the harness works

- `lib/server.mjs` starts one server per test file on a random free port
  (never 8765) with its own temp dir, waits for `GET /readyz`, and kills it
  at the end. The server binary is:
  1. `$SDL_BIN` if set, else
  2. `../target/debug/sentry-ducklake`, built with `cargo build -p sdl-server`
     if it is missing. It is **not** rebuilt when it exists: run
     `cargo build -p sdl-server` after changing server code.
- Events are committed before the ingest request returns, so tests read
  them right after `Sentry.flush()`. Where a process exits on its own
  (uncaught exception), tests poll with a timeout instead of sleeping.
- `browser-app/` is built with Vite's JS API into `browser-app/.dist/<name>`
  (git-ignored, removed after each test). The app reads the DSN and release
  from its query string, so one build works with any server.
- `sentry-cli` 2.x is installed as the npm alias `sentry-cli-v2`; tests
  resolve each package's own binary, so `npx sentry-cli` may be either one.

## Environment variables

| Variable | Effect |
|---|---|
| `SDL_BIN` | Server binary to test. |
| `SDL_E2E_TMPDIR` | Parent dir for the per-server temp dirs (default: OS temp dir). |
| `SDL_E2E_KEEP=1` | Keep the server temp dirs (storage, catalog) after the run. |
| `SDL_E2E_VERBOSE=1` | Print server logs. |
| `RUST_LOG` | Server log filter (default `info`). |
