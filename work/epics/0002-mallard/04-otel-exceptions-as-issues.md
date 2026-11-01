---
status: done
created: 2026-11-01
---
# 04 — OTel exceptions become Sentry issues

## Goal
Exceptions recorded by OpenTelemetry SDKs (span events named `exception`, and logs with `exception.*` attributes) are grouped into issues on the errors side, so OTel-only apps get issue tracking.

## Acceptance criteria
- An OTel span with an exception event creates/updates an issue with type, message and stack trace.
- The issue links back to the trace.
- Opt-out switch in config; no duplicates when an app sends both Sentry and OTel.

## Plan
- odl-server: optional `IngestHook` called with each OTLP traces/logs
  request after it was buffered (OTLP/HTTP and gRPC).
- sdl-server: `Server::ingest_event(project, json)` (same normalize,
  group and write path as SDK events), project lookup/create, and an
  `EventObserver` called for each stored SDK event (trace + span id).
- mallard-server `bridge`: find span events named `exception` and log
  records with `exception.*` attributes, build a Sentry event (title from
  type + message, frames parsed from `exception.stacktrace` for Python,
  Java, JS/Node, Go, .NET, Rust, else the raw text in `extra`), trace and
  span ids in `contexts.trace`, environment/release/server from resource.
- Project: slug = slugified `service.name`; else `bridge.default_project`
  (created when missing); empty default = create a project per service.
- Dedupe: hold each OTel exception for `bridge.dedupe_window` (10s), then
  drop it if a Sentry SDK event with the same trace_id + span_id was
  stored in the last hour (in-memory set fed by the observer).
- Config `[bridge] exceptions`, `default_project`, `dedupe_window`; ADR 0009.
- Tests: stack trace parsers per language, mapping unit tests,
  integration test incl. the dedupe case.

## Progress log
- 2026-11-01: odl-server `IngestHook` (after buffering, OTLP only);
  sdl-server `ingest_event`, `EventObserver`, `project_by_slug`,
  `create_project`; `bridge/exception.rs` + `bridge/stacktrace.rs` with
  unit tests (Python, Java, Node, Go, .NET, Rust traces; Python span event
  and Java log record mappings run through the Sentry side's normalizer).
- Integration test `tests/bridge.rs::otel_exceptions_become_issues_once`:
  span event and log exceptions → issues in the auto-created project
  `checkout`, linked to the trace; Sentry event before and after the OTLP
  export → stored once.
- Project rule chosen: slugified `service.name`; else
  `bridge.default_project`; empty default = create one project per
  service (the default project is created too when missing).
- Dedupe rule: hold each OTel exception `dedupe_window` (10 s), drop it if
  a Sentry SDK event with the same trace_id + span_id was stored in the
  last hour (in-memory set). Event ids are a hash of the exception, so an
  OTLP retry is stored once. Shutdown stores the queue without waiting.
- Bug found and fixed while testing: with `exceptions = false` the Sentry
  side shut down at start-up (its shutdown waited on the bridge worker).

## Follow-ups
- Dedupe is per process and in memory (restart, several replicas): see
  ADR 0009 Consequences; same limit as sentry issue 0001.
