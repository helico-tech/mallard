# Sentry compatibility

Point any Sentry SDK at a DSN from this server and errors arrive. Nothing
else in your app changes.

```js
Sentry.init({ dsn: "https://<public_key>@errors.example.com/1" });
```

## Verified clients

Tested end to end in CI (`e2e/`, `crates/sdl-server/tests/`):

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
| `POST /api/{project_id}/envelope/` | all current SDKs |
| `POST /api/{project_id}/store/` | legacy JSON events, incl. base64+zlib bodies of very old SDKs |

- Auth: `X-Sentry-Auth`, `Authorization: Sentry ...`, `?sentry_key=`, or the
  `dsn` in the envelope header (tunnels).
- `Content-Encoding`: gzip, deflate, br, zstd. Any `Content-Type`.
- CORS: any origin (the DSN key is public by design, as in Sentry).
- DSNs may contain a path prefix (`https://key@host/sentry/1`).
- Responses: `200 {"id": "..."}` after the event is committed;
  `400/401/404/413` with `{"detail": ...}` and `X-Sentry-Error`;
  `429` with `Retry-After` and `X-Sentry-Rate-Limits` when the write queue is
  full; `503` while shutting down.
- Duplicate event ids (SDK retries) within an hour are acknowledged once.

## Envelope items

| Item type | Handling |
|---|---|
| `event` | stored (errors and messages) |
| `transaction` | standalone: accepted, dropped; in mallard: stored as OTel spans (ADR 0009) |
| `span` | standalone: accepted, dropped; in mallard: v2 items (span streaming, the SDK 11 default) stored as OTel spans (ADR 0013), older standalone spans dropped |
| `profile`, `profile_chunk` | accepted, dropped |
| `session`, `sessions` | accepted, dropped (no release health) |
| `attachment`, `replay_event`, `replay_recording`, `replay_video` | accepted, dropped |
| `client_report`, `check_in`, `log`, `otel_log`, `statsd`, `feedback`, `user_report` | accepted, dropped |

Dropped items are counted in `sdl_ingest_items_dropped_total{type=...}`. You
can keep `tracesSampleRate` and replays enabled; set them to 0 to save
bandwidth.

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
- `{{auto}}` IP (uses `X-Forwarded-For` only if `SDL_TRUST_FORWARDED_FOR`)
- JS sourcemaps: debug ids and release artifacts; original file, line, column,
  function name and source context; `raw_stacktrace` keeps the original
- regression: a new event reopens a resolved issue

Not implemented: data scrubbing / PII rules, inbound filters, rate limits per
project, quotas, release health, alerts, native symbolication (dSYM, PDB,
ProGuard), event reprocessing.

## Sourcemap upload endpoints (`/api/0/...`, bearer token)

| Endpoint | Notes |
|---|---|
| `GET /api/0/organizations/{org}/chunk-upload/` | options: sha1, gzip, 8 MiB chunks |
| `POST /api/0/organizations/{org}/chunk-upload/` | multipart chunks (`file`, `file_gzip`), checksums verified |
| `POST /api/0/organizations/{org}/artifactbundle/assemble/` | `not_found` + missing chunks, then synchronous `ok` |
| releases create / update / deploys | answered, not stored (stubs: `sentry-cli releases list` shows nothing; releases come from events, see `GET /v1/projects/{p}/releases`) |

Any `org` slug is accepted. Use the server's token (`SDL_API_TOKEN`) as the
auth token. See [sourcemaps.md](sourcemaps.md).

## Not supported

The Sentry web API (`/api/0/projects/...`, `/api/0/issues/...`) and therefore
Sentry's own UI and `sentry-cli issues` / `events`. Use the `/v1` API, the
`sdl` CLI or the TUI.
