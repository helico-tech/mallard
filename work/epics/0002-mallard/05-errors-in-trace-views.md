---
status: done
created: 2026-11-01
---
# 05 — Errors visible in trace views

## Goal
Trace lists and waterfalls (CLI and TUI) show which spans have Sentry errors, without a second lookup per trace.

## Acceptance criteria
- `mallard telemetry traces` / TUI trace list show an error-event count per trace.
- Waterfall marks spans that have Sentry events.

## Plan
- Sentry API (additive): `GET /v1/events/by-trace?trace_id=a,b,…` (max 200 ids,
  window `since`/`from`/`to`, default 30d like `/v1/events`). Answer
  `{"traces": {"<trace id>": {"count": n, "events": [{event_id, issue_id,
  project_id, span_id, title}]}}}`; traces without events are left out, at
  most 50 events per trace (newest). One query on `trace_id IN (?::UUID, …)`
  inside the window. Types in `sdl-types`, `Client::errors_by_trace` in
  `sdl-client` (splits more than 200 ids into several calls), OpenAPI
  snapshot regenerated.
- `odl-cli` gets a small hook, `ErrorLookup` (trait, boxed future), plus the
  types it returns (`TraceErrors`, `ErrorEvent`); no `sdl-*` dependency.
  `Ctx::with_error_lookup` and `Embedded::set_error_lookup`; the standalone
  `odl` has none and prints what it printed before.
- `mallard` implements the hook with `mallard-client` (one batch call per
  list) and sets it on the telemetry `Ctx` and the TUI pane.
- CLI: `traces list` items get `sentry_errors` (count; `null` when the
  lookup failed, with a warning on stderr); text gets a `sentry` column.
  The name is not `errors`: `error_count` and the text column `errors`
  already mean spans with error status. `trace`/`traces show`: each span in
  JSON gets `sentry_errors`; in text the span line gets `⚠ n`.
- TUI: trace list column `sentry` (`⚠ n`), waterfall marks spans with `⚠`,
  the span panel lists its Sentry events. `x` on such a span returns the new
  `Outcome::JumpEvent { trace_id, event_id }`; the host opens the errors tab
  on the trace's events and the errors side opens that event when the list
  arrives. `x` elsewhere stays `Jump(trace_id)`.
- Tests: endpoint (sdl-server integration), client (fake server), CLI
  (agent_cli end to end), TUI TestBackend (markers, `x`), errors side opens
  the event.

## Progress log
- 2026-11-01: `GET /v1/events/by-trace` (sdl-types, sdl-server, sdl-client,
  OpenAPI snapshot); endpoint and client tests.
- 2026-11-01: `Outcome::JumpEvent` in mallard-term; the errors tab opens the
  trace's events, then that event (`Embedded::open_trace_event`); host routes it.
- 2026-11-01: `odl_cli::ErrorLookup`; CLI `sentry_errors`/`sentry` column, TUI
  list column, waterfall `⚠`, span panel section, `x` → `JumpEvent`. TUI
  TestBackend tests; standalone `odl` unchanged (test).
- 2026-11-01: `mallard` implements the lookup with mallard-client; `trace`
  marks spans (`sentry_errors` per span in JSON, `⚠ n` in text). agent_cli
  end-to-end checks. Docs (cli, agents, architecture, otel/embedding,
  sdl-cli README). README screenshots regenerated (the waterfall now shows
  `⚠`).

## Follow-ups
- The hidden `mallard telemetry trace <id>` (odl waterfall text) does not
  mark spans; `mallard trace` does. Add it only if someone uses that path.
- The lookup window is the list's window (TUI: the trace ± 1 h). An error
  received after the window's end (late SDK flush) is not counted.
