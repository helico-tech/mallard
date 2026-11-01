---
status: done
type: debt
created: 2026-11-01
---
# Standalone paths left in the side crates

## Problem
Story 0002/11 removed `Standalone` from the Sentry server. Some code from the old
binaries is still there because something still reaches it:

- `mallard_otel_server::Server` still binds its own listeners from
  `config.server` (`api_addr` and so on). Only tests use that; mallard uses the
  routers. `mallard_otel_server::config::{ServerConfig, AuthConfig}` exist for it.
- `tui::run_standalone` (telemetry) and `tui::run` (Sentry) run one side's TUI in
  its own terminal. They are reached only through the hidden aliases
  `mallard telemetry tui` and `mallard errors tui`; `mallard tui` does not use them.
- `ERRORS_HINTS` in `crates/mallard/src/main.rs` now equals `mallard_sentry_cli::HINTS`.

## Notes
None of it is wrong, it is only more code than the product needs. Decide when the
hidden aliases go.

Done for the code that only tests reached. `Server::serve` and `config::ServerConfig` are
gone, along with the `tower-http` dependency; `tokio-util` is now a dev-dependency. Tests
moved to the routers: `client_against_server` binds its own listeners, and the check that
a failed start stops the receivers is now `mallard-server`'s
`serve_stops_when_background_fails_to_start` (the code that does this in the product),
plus `run_background_returns_when_it_cannot_start` here. `log_format` parsing is checked in
`mallard-server`, which owns it. `ERRORS_HINTS` and `TELEMETRY_HINTS` are gone: both libraries'
`HINTS` say the same. `TELEMETRY_HINTS` had a `project_env` that `mallard_otel_cli::HINTS` lacks,
but only the Sentry side reads it, so messages do not change.

Kept, because something other than tests reaches it:

- `config::AuthConfig`: this issue said it existed for the listeners, but
  `mallard_server::Config::otel_config` passes the tokens through it.
- `Server::new`: only tests use it, but it binds nothing and removing it was not in scope.

**Decision for the owner: when do the hidden aliases go?** `mallard errors …` and
`mallard telemetry …` stay, with the TUI entry points they reach (`tui::run_standalone`,
Sentry `tui::run`). Some commands exist only there, for example `mallard errors info` and
`mallard errors maintenance`, so removing them is a product decision. This issue is closed
for the rest. No config keys or env vars that users set changed.
