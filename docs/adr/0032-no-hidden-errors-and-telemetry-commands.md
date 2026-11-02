# 0032. No hidden `errors` and `telemetry` commands

## Status
accepted; removes the hidden aliases of 0008, and what 0011 and 0024 say about them

## Context
ADR 0008 kept `mallard errors …` (also `sentry`) and `mallard telemetry …` (also `otel`) as hidden
aliases when the flat tree came. Nothing is released, so nobody depends on them. They are a second
path to every command, with their own branches in `scope.rs`, and some jobs could only be done
through them.

## Decision
The aliases are gone. `mallard errors` is an ordinary usage error, exit 2. `series errors`, the crates'
names and the server's `/sentry` and `/otel` URL prefixes do not change.

What had no top-level form gets one, so no feature goes with the path:
- `mallard health` (was `telemetry health`): is the server up, without a token. `status` needs one.
- `mallard stats` (was `errors stats`): the totals of a project. `series errors` only has the buckets.
- `mallard flush` (was `errors maintenance`): the running server does its maintenance now. `mallard
  maintenance` is taken: it runs without a server, on the catalog, and needs the config.

What has a top-level form, sometimes in JSON only, goes without a new command:
- `errors info`, `telemetry status`: `mallard status --json`, under `sentry.info` and `otel.info`.
- `telemetry trace -A`: `mallard trace --json` has the attributes and events.
- `telemetry schema`: `mallard schema`.
- `errors tui`, `telemetry tui`: `mallard tui`.

`mallard tui` now does what only `errors tui` did: it says it needs a terminal, instead of panicking.
`mallard trace` now does what only `telemetry trace` did: it shows spans in a parent loop.

## Consequences
- One path per command. Library commands that nothing mounts are removed from `mallard-sentry-cli`
  and `mallard-otel-cli`.
- The text views of `telemetry status` (per table) and `trace -A` are gone. Issue 0184 asks if they
  should come back on `status` and `trace`.
- The TUI components keep their mode without a host for now, see issue 0185.
