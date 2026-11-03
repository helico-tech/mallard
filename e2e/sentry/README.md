# End-to-end tests

These tests prove that real Sentry tools work against `mallard serve` when
you change only the DSN / URL (the Sentry side is under `/sentry`):

| File | What it proves |
|---|---|
| `tests/node-sdk.test.mjs` | `@sentry/node`: cause chains, messages with level, breadcrumbs, user/tags/contexts, custom fingerprints (`{{ default }}` too), release/environment, 3 events -> 1 issue, resolve -> regression, `trace_id` inside a span, uncaught exception and unhandled rejection in a child process. |
| `tests/browser-sourcemaps.test.mjs` | A minified Vite + TypeScript app (`browser-app/`) in headless Chromium, served from a different origin than the server (CORS, `fetch` transport). Sourcemaps uploaded with the Rust `sentry-cli` 3.x (and 2.x): debug IDs (`sourcemaps inject`), `--release`, release-only (no debug IDs) and `--url-prefix`. Asserts original `.ts` file names, function names, line numbers and source context, `raw_stacktrace` with the minified frames, and that a rebuild with a new bundle hash stays in the same issue. |
| `tests/vite-plugin.test.mjs` | `@sentry/vite-plugin` (uploads with the new TypeScript `sentry` CLI) with `url`, `authToken`, `org`, `project`; release injection; symbolication after the maps were deleted from the build. |
| `tests/node-tracing.test.mjs` | `@sentry/node` with `tracesSampleRate: 1` in an HTTP service whose route throws inside a child span, called by a traced fetch (`fixtures/node-tracing-app.mjs`): with the SDK 11 default, span streaming (`span` items, ADR 0013), and with `traceLifecycle: "static"` (`transaction` items, ADR 0009) all spans show up in `/otel/api/v1/traces/{id}` through the bridge with the right parents, kind, status, service and release, and the error event carries the trace and the throwing span's id, a child of the request span. |
| `tests/releases.test.mjs` | `sentry-cli` 3.x and 2.x: `releases new`, `finalize`, `list`, `info`, `deploys new`, `list` are stored and read back; an unknown project and an unknown release fail |
| `tests/unhappy.test.mjs` | Wrong key -> 401 (SDK keeps running), unknown project -> 404, gzip envelopes, bad gzip -> 400, query-string and envelope-DSN auth, legacy `/store/` (JSON and base64+zlib), CORS preflight, bearer token on the API. |

Every assertion reads stored data back through the `/sentry/v1` API (and
`/otel/api/v1` for traces).

## Run

Prerequisites: Node.js 22+ (tested with 24), a Rust toolchain (to build the
server), and network access for `npm ci`, the Chromium download and DuckDB's
extensions (installed on first start).

```sh
cd e2e/sentry
npm ci
npx playwright install chromium
npm test
```

`npm test` first type-checks `browser-app/` with `tsc`. Run one file:
`node --test tests/browser-sourcemaps.test.mjs`. The Node-only files
(`node-sdk`, `node-tracing`, `unhappy`) need no browser.

## How the harness works

- `lib/server.mjs` starts one `mallard serve` per test file with its own
  temp dir and config file (`mallard.toml` there: `http_addr` on a random
  free port, never 8765; OTLP listeners on `127.0.0.1:0`; DuckDB-file
  catalog, lake and sourcemap artifacts in the temp dir; token
  `e2e-token`), waits for `GET /readyz`, and kills it at the end.
  `server.url` is `<base>/sentry` (DSNs, `/v1` API, the vite plugin's
  `url`), `server.base` the root (the Rust `sentry-cli` refuses a URL with a
  path; mallard serves its `/api/0` on the root too);
  `server.otel(method, path)` calls `/otel/...`.
  `TestServer.start(name, { bridge: { dedupe_window: "1s" } })` sets
  `[bridge]` keys. `MALLARD_*` variables of the shell are not passed on.
  The binary is:
  1. `$MALLARD_BIN` if set, else
  2. `../../target/debug/mallard`, built with `cargo build -p mallard` if it
     is missing. It is **not** rebuilt when it exists: run
     `cargo build -p mallard` after changing server code (also after a
     `--no-default-features` build, which has no `serve`).
- Events are committed before the ingest request returns, so tests read
  them right after `Sentry.flush()`. Spans go through the OTel write buffer
  (flushed every few seconds), so trace tests poll. Where a process exits on its own
  (uncaught exception), tests poll with a timeout instead of sleeping.
- `browser-app/` is built with Vite's JS API into `browser-app/.dist/<name>`
  (git-ignored, removed after each test). The app reads the DSN and release
  from its query string, so one build works with any server.
- `sentry-cli` 2.x is installed as the npm alias `sentry-cli-v2`; tests
  resolve each package's own binary, so `npx sentry-cli` may be either one.

## Environment variables

| Variable | Effect |
|---|---|
| `MALLARD_BIN` | `mallard` binary to test. |
| `MALLARD_E2E_TMPDIR` | Parent dir for the per-server temp dirs (default: OS temp dir). |
| `MALLARD_E2E_KEEP=1` | Keep the server temp dirs (config, catalog, lake) after the run. |
| `MALLARD_E2E_VERBOSE=1` | Print server logs. |
| `RUST_LOG` | Server log filter (default `info`). |
