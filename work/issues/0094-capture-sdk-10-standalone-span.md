---
status: done
type: debt
created: 2026-11-01
---
# Check standalone spans against a real @sentry/browser 10

## Problem
The bridge's standalone span conversion (ADR 0022) was written from the source
of `@sentry/browser` 10.78 and tested with a hand-written payload. It has not
seen an envelope from a running browser: e2e installs only SDK 11.

## Notes
Capture an INP envelope from a browser test with `@sentry/browser` 10 and use
it as a fixture, as `tests/fixtures/sentry-node-11-spans.envelope` does for SDK 11.
With user info collection on, 10.78 sets `client.address` to the Relay placeholder
`{{auto}}`. The bridge stores that text as it is; see what a capture shows.

Done: `tests/fixtures/sentry-browser-10-inp.envelope` is the raw body from
`@sentry/browser` 10.78.0 in headless Chromium 1248 (Playwright 1.64.1), with
`browserTracingIntegration`, `sendDefaultPii: true` and `tracesSampleRate: 1`.
A click on a button whose handler blocks for 300 ms, then a click on a link:
leaving the page is what reports INP. The SDK files were served with an import
map, without a bundler. Nothing had to be scrubbed: the capture has no paths,
hostnames, addresses or names. `release` and `environment` are test values.

What the capture showed against the hand-written payload:

- Right: one span per item, `{"type":"span"}`, a `trace` header and no `sdk`
  header, the field names, `measurements.inp`, `release` and `environment` in
  `data`. So no span was dropped and every ADR 0022 mapping holds.
- Wrong: `client.address` with user info collection on is the text `{{auto}}`,
  and the bridge stored that text. Now it is the address of the request, by the
  same rule as `user.ip_address` of an event; without one the attribute is left
  out. This needed `client_ip` on the `Spans` hook of `mallard-sentry-server`.
- Wrong in the payload only, and the bridge does not read these: there are two
  shapes, and it mixed them. With no active span: no `parent_span_id`,
  `is_segment: true`, `segment_id` is its own id, `sentry.source: custom`.
  With the pageload span active (leaving within its idle time): `parent_span_id`
  and `segment_id` are that span, no `is_segment`. It also had
  `sentry.sample_rate`, which the capture does not.
- `data.user` is only set by `getCurrentScope().setUser()`. `Sentry.setUser()`
  writes to another scope, so the usual call leaves it out.
- The span shares its trace with the pageload transaction, usually as a second
  root. `mallard traces show` lists both.

Also checked against a real `mallard serve`: this fixture, and the SDK itself with
its DSN pointing at the server. Found along the way: issue 0135.
