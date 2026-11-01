---
status: todo
type: bug
created: 2026-11-01
---
# Two other ways an SDK asks for the client address are not handled

## Problem
Found while capturing `@sentry/browser` 10.78 (issue 0094). Read from the code;
neither was run end to end.

- `sdk.settings.infer_ip = "auto"`: with `sendDefaultPii` the captured pageload
  transaction asks for the address this way, with no `user.ip_address`. The SDK's
  `BrowserClient` sets it for all its events, so its error events should do the same.
  Nothing in `crates/` reads `infer_ip`, nor `ingest_settings.infer_ip` of the span
  and metric items in the SDK's source. So those spans have no `client.address`, and
  error events from this SDK probably have no `user.ip_address`.
- `user.ip_address = "{{auto}}"` on a transaction: `bridge/transaction.rs` copies it
  to `client.address` as it is, because transactions reach the bridge raw. SDK 10.78
  browser does not send this; older SDKs may. Their packages were not checked.

## Notes
The rule to follow is the one for events (`event.rs`): fill from the address of the
request, else leave it out. `Spans` has `client_ip` now, `Transaction` does not.
Decide if `infer_ip = "never"` must also stop an address the SDK did not send from
being filled.
