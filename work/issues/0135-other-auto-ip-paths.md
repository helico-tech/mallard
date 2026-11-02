---
status: done
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

Done. Both gaps were real. Captured from `@sentry/browser` in headless Chromium 1248
(Playwright 1.64.1), then posted to a server in `tests/bridge.rs` before the fix. SDK 8
and the events of SDK 11 were captured but not posted; those cells follow from what the
SDK sends:

| SDK | what it sends | span before | event before |
|---|---|---|---|
| 8.55 `sendDefaultPii` | nothing | no address | no address |
| 9.47 `sendDefaultPii` | `user.ip_address = "{{auto}}"` | the text `{{auto}}` | address |
| 10.78 `sendDefaultPii` | `sdk.settings.infer_ip = "auto"` | no address | no address |
| 10.78 without | `infer_ip = "never"` | no address | no address |
| 11.10 default | `ingest_settings.infer_ip = "auto"` on the span container, `sdk.settings` on events | no address | no address |
| 11.10 `dataCollection.userInfo: false` | `"never"` in the same places | no address | no address |

The rule is one function, `mallard_sentry_protocol::event::client_address`, used for
events, transactions and span containers. `infer_ip = "never"` stores nothing, also an
address the SDK sent or a `{{auto}}`. That answers the question above, and it is stricter
than Relay, which keeps an address the SDK sent. Without `infer_ip` nothing is added, so
SDK 8 gets no address here, where Relay infers one for JavaScript. The address goes on
the root span of a transaction and on segment spans (`is_segment`), so a trace holds as
many copies with SDK 11 as with SDK 10; a span that holds `{{auto}}` itself is also filled.

Node SDKs send no `ingest_settings` and no `infer_ip`, so nothing changes for them. The
`infer_ip` of metric and log items is not read, because those items are not stored.
`docs/sentry/compatibility.md` has the rule. The new fixtures are raw captures; nothing
had to be removed.

Decision by the owner, 2026-11-02: follow the SDK, with no server switch (option A of three: also a server
off switch; off by default). So an SDK that asks for it gets the address stored.
