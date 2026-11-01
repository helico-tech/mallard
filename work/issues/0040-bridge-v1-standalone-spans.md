---
status: todo
type: idea
created: 2026-11-01
---
# Bridge standalone spans of older Sentry SDKs (`span` items without v2)

## Problem
Sentry SDKs before 11 can send single standalone spans as `span` items
without the v2 content type (for example INP spans of `@sentry/browser` 8+).
The payload is one span in the transaction child format (`description`,
`op`, `timestamp`, `data`). The bridge (ADR 0013) only converts v2 items and
counts these as `mallard_bridge_span_items_total{result=unsupported}`, so
they do not show up in the trace views.

## Notes
- Their format is close to `spans[]` in a transaction, so `transaction.rs`
  can do most of the work; release and environment come from the envelope's
  `trace` header.
- Not checked against a real SDK capture yet: e2e installs only SDK 11.
- Worth doing when someone runs an older SDK with standalone spans on and
  misses them.
