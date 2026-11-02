---
status: done
type: bug
created: 2026-11-02
---
# 0190 Web UI: the logs of a trace are hidden in an app without telemetry

## Problem
With project `api` chosen, which has no service of that name, `x` on an issue opens the trace
and `l` or the "2 logs" button says "api has no telemetry". The TUI shows the logs, and
`docs/cli.md` says `trace=` follows the trace across apps. `LogsPage` already asked by id;
the route's `TelemetryScope` stopped it first. An error's project and the services of its
trace often have different names, so this is the common case.

## How to reproduce
`/errors/api` with the demo data, open an issue, `x`, `l`.

## Notes
Fixed: `TelemetryScope` lets a `trace=` filter through (`byId`). Browser test: "the logs of a
trace show in an app without telemetry". Found while taking the README screenshots.
