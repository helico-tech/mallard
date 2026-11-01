---
status: todo
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
