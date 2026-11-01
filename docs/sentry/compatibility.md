# Sentry compatibility

Point any Sentry SDK at a DSN from mallard and errors arrive. Nothing else in
your app changes. A DSN is `<public_url>/sentry/<project id>` with the project's
key:

```js
Sentry.init({ dsn: "https://<public_key>@mallard.example.com/sentry/1" });
```

## Verified clients

Tested end to end in CI (`e2e/sentry/`, `crates/mallard-sentry-server/tests/`):

| Client | Version | What is verified |
|---|---|---|
| `@sentry/node` | 11.10 | exceptions with cause chains, messages, breadcrumbs, user/tags/contexts, fingerprints, uncaught exceptions + unhandled rejections (flushed on exit), trace ids inside spans |
| `@sentry/browser` | 11.10 | errors from a Vite + TypeScript app in headless Chromium, cross-origin (CORS, `text/plain` fetch transport), symbolicated to original `.ts` files |
| `sentry-cli` | 3.8.2 and 2.58.6 | `sourcemaps inject` + `sourcemaps upload` (debug ids, with/without `--release`, release-only with `--url-prefix`) |
| `@sentry/vite-plugin` | 11.10 (uses the TypeScript `sentry` CLI 0.45) | upload + release injection, symbolication by debug id |
| `sentry` (Rust) | 0.49 | errors, messages, tags, users, trace context |

Other SDKs (Python, Go, Java, .NET, PHP, Ruby, mobile) speak the same
envelope protocol and are expected to work; they are not in the test suite.

## Ingest endpoints

| Endpoint | Notes |
|---|---|
| `POST /sentry/api/{project_id}/envelope/` | all current SDKs |
| `POST /sentry/api/{project_id}/store/` | legacy JSON events, incl. base64+zlib bodies of very old SDKs |

- Auth: `X-Sentry-Auth`, `Authorization: Sentry ...`, `?sentry_key=`, or the
  `dsn` in the envelope header (tunnels).
- `Content-Encoding`: gzip, deflate, br, zstd. Any `Content-Type`.
- CORS: any origin (the DSN key is public by design, as in Sentry).
- Responses: `200 {"id": "..."}` after the event is committed;
  `400/401/404/413` with `{"detail": ...}` and `X-Sentry-Error`;
  `429` with `Retry-After` and `X-Sentry-Rate-Limits` when the write queue is
  full; `503` while shutting down.
- Duplicate event ids (SDK retries) within an hour are acknowledged once.

## Envelope items

| Item type | Handling |
|---|---|
| `event` | stored (errors and messages) |
| `transaction` | stored as OTel spans through the bridge (ADR 0009) |
| `span` | stored as OTel spans through the bridge: v2 items (span streaming, the SDK 11 default; ADR 0013) and standalone spans of older SDKs (ADR 0022, checked against a capture from `@sentry/browser` 10.78) |
| `profile`, `profile_chunk` | accepted, dropped |
| `session`, `sessions` | accepted, dropped (no release health) |
| `attachment`, `replay_event`, `replay_recording`, `replay_video` | accepted, dropped |
| `client_report`, `check_in`, `log`, `otel_log`, `otel_span`, `statsd`, `metric_buckets`, `feedback`, `user_report` | accepted, dropped |

With `bridge.transactions = false`, `transaction` and `span` items are dropped too.
Dropped items are counted in `mallard_sentry_ingest_items_dropped_total{type=...}`.
You can keep replays enabled; set their sample rates to 0 to save bandwidth.

## Event processing

What Sentry does that we also do:

- normalization of all SDK shapes; when the envelope's `sent_at` is 55 minutes
  or more off the receive time, the event and breadcrumb timestamps shift by that
  difference (Relay's clock drift; spans are not shifted); an event `timestamp`
  outside [received - 30d, received + 60s] is then replaced by the receive time
  (Relay: 60 s future, retention past); first/last seen, trends and stats use it
- an event older than the resolve does not reopen a resolved issue
- grouping by fingerprint / exception type + in-app frames / masked values /
  message template, with `{{ default }}` support
- derived tags: level, environment, release, dist, server_name, transaction,
  logger, browser, os, runtime (+ `.name`), device, url, handled, mechanism,
  user; browser/os parsed from the User-Agent when the SDK did not send them
- the client's address, only when the SDK asks for it: `{{auto}}` in `user.ip_address`
  of an event or transaction or in `client.address` of a span, or `infer_ip: auto` in
  `sdk.settings` of an event or transaction or `ingest_settings` of a span container.
  It is the address of the request (`X-Forwarded-For` only with
  `sentry.ingest.trust_forwarded_for`); with no address known it is left out. Events get
  `user.ip_address`, which counts as the user when there is no id, email or username.
  Spans get `client.address`, on the root span of a transaction or a segment span only.
  Differences from Relay: `infer_ip: never` stores nothing, also an address the SDK
  sent; and without `infer_ip` or `{{auto}}` nothing is added, also for JavaScript
  (`@sentry/browser` 8 asks for nothing, so it gets no address here)
- JS sourcemaps: debug ids and release artifacts; original file, line, column,
  function name and source context; `raw_stacktrace` keeps the original
- regression: a new event reopens a resolved issue

Not implemented: data scrubbing / PII rules, inbound filters, rate limits per
project, quotas, release health, alerts, native symbolication (dSYM, PDB,
ProGuard), event reprocessing.

## Sourcemap upload endpoints (bearer token)

These are served under `/sentry/api/0/...` and on the root, `/api/0/...`, because
`sentry-cli` takes only a URL without a path.

| Endpoint | Notes |
|---|---|
| `GET /api/0/organizations/{org}/chunk-upload/` | options: sha1, gzip, 8 MiB chunks |
| `POST /api/0/organizations/{org}/chunk-upload/` | multipart chunks (`file`, `file_gzip`), checksums verified |
| `POST /api/0/organizations/{org}/artifactbundle/assemble/` | `not_found` + missing chunks, then synchronous `ok` |
| releases create / update / deploys | answered, not stored (stubs: `sentry-cli releases list` shows nothing; releases come from events, see `GET /sentry/v1/projects/{p}/releases`) |

Any `org` slug is accepted. Use one of `server.api_tokens` as the auth token. See
[sourcemaps.md](sourcemaps.md).

## Not supported

The Sentry web API (`/api/0/projects/...`, `/api/0/issues/...`) and therefore
Sentry's own UI and `sentry-cli issues` / `events`. Use the `/sentry/v1` API, the
`mallard` CLI or the TUI.
