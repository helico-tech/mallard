# 0009 — Bridge: Sentry transactions as traces, OTel exceptions as issues

## Status
accepted

## Context
mallard runs both sides in one process on one catalog (ADR 0001), but
each side only stored its own data. Apps that only use a Sentry SDK got
no traces (sdl-server dropped `transaction` items), and apps that only use
OpenTelemetry got no issues (exceptions stayed span events and log
attributes). Both libraries must keep building and running alone, and
mallard must not write into their schemas directly (architecture rules).

## Decision
The conversion lives in `mallard-server` (`src/bridge/`), in process, no
HTTP. Each library gets small hooks; without them nothing changes.

- `sdl-server`: `Server::set_transaction_sink` (transaction items go to a
  `TransactionSink` instead of being dropped), `Server::set_event_observer`
  (`EventObserver::event_stored(project, trace_id, span_id)` for every
  stored SDK event), `Server::ingest_event(project, json)` (same
  normalize/group/write path as ingest), `project_by_slug`,
  `create_project`.
- `odl-server`: `Server::set_ingest_hook` (`IngestHook::traces`/`logs`
  see each OTLP request once it is buffered) and
  `Server::ingest_traces(ExportTraceServiceRequest)` (same conversion,
  limits and back-pressure as OTLP, not shown to the hook).

Config, both on by default:

```toml
[bridge]
transactions = true       # Sentry transactions -> OTel spans
exceptions = true         # OTel exceptions -> Sentry events and issues
default_project = ""      # slug for exceptions of unknown services; "" = one project per service
dedupe_window = "10s"     # how long an OTel exception waits for a Sentry event of its span (max 10m)
```

### Sentry transaction → OTel spans

One OTLP `ResourceSpans` per transaction: the root span from the
transaction and `contexts.trace`, one span per `spans[]` item (items
without a valid `span_id` or `timestamp` are skipped; a transaction
without valid trace/span ids is dropped).

| OTel | root span (transaction) | child span (`spans[]`) |
|---|---|---|
| `trace_id` | `contexts.trace.trace_id` | `trace_id`, else the root's |
| `span_id` | `contexts.trace.span_id` | `span_id` |
| `parent_span_id` | `contexts.trace.parent_span_id` | `parent_span_id`, else the root |
| `name` | `transaction`, else `op`, else `transaction` | `description`, else `op`, else `span` |
| `kind` | `data["otel.kind"]` if sent, else from `op` (below) | same |
| start / end | `start_timestamp` / `timestamp` (seconds or RFC 3339; floats rounded to µs) | same |
| status | `ok` → OK; missing → UNSET; any other (`internal_error`, `cancelled`, …) → ERROR, message = the Sentry status | same |
| attributes | `contexts.trace.data`, then event `tags` (tags win), `sentry.op`, `sentry.origin`, `sentry.status`; `user.id`, `user.email`, `user.name` (username), `client.address` (ip) | span `tags`, then `data` (data wins), `sentry.op`, `sentry.origin`, `sentry.status` |

`op` → kind: `*.server` → SERVER; `*.client`, `http`, `db`, `db.*`,
`cache.*`, `rpc`, `grpc` → CLIENT; `queue.publish*`, `queue.submit*` →
PRODUCER; `queue.process*`, `queue.receive*`, `queue.task*` → CONSUMER;
else INTERNAL.

Resource: `service.name` = event tag `service.name`, else the project
slug; `service.version` = `release`; `deployment.environment.name` =
`environment`; `host.name` = `server_name`; `telemetry.sdk.language` =
`platform`. Scope: `sdk.name` / `sdk.version`. `server_name` is the host
in Sentry SDKs (Node sets `os.hostname()`), so it is not used as the
service; `contexts.otel.resource` is ignored too (@sentry/node always says
`service.name = node`).

Errors of the same request keep their `contexts.trace.span_id`, so they
sit under the right span in `mallard trace <id>` and the TUI.

Back-pressure: the sink converts on a blocking thread, at most 1024 at
once; more, a full OTel buffer or an invalid payload drops the
transaction. Sentry ingest never waits. Metrics:
`mallard_bridge_transactions_total{result=stored|invalid|busy|otel_full|too_large|error}`,
`mallard_bridge_spans_total`.

### OTel exception → Sentry event

An exception is a span event named `exception`, or a log record with an
`exception.type` or `exception.message` attribute (one of the two is
required).

| Sentry | from |
|---|---|
| `exception.values[0].type` / `value` | `exception.type` (else `Error`) / `exception.message` |
| `…stacktrace.frames` | `exception.stacktrace` parsed (Python, Java/Kotlin, JS V8 and Firefox, Go `debug.Stack`, .NET, Rust backtrace; the SDK's `telemetry.sdk.language` is tried first); unparsed text goes to `extra["exception.stacktrace"]` |
| `…mechanism` | `{type: "otel", handled: !exception.escaped}` (handled only when `escaped` is sent) |
| title, culprit, group | computed by the Sentry side as for any event (type + message, frames) |
| `contexts.trace` | the span's (or log record's) `trace_id` and `span_id` |
| `timestamp` | event time (span event), `time` or `observed_time` (log) |
| `level` | `error`; logs: severity ≥ 21 fatal, 17–20 or unset error, 13–16 warning, 9–12 info, else debug |
| `platform` | `telemetry.sdk.language`: python, java, nodejs → node, webjs → javascript, go, dotnet → csharp, rust, ruby, php, … else `other` |
| `environment` / `release` / `server_name` | `deployment.environment.name` (or `deployment.environment`) / `service.version` / `host.name` |
| `transaction` / `logger` | span name / log scope name |
| `logentry.formatted` | log body, when it differs from the message |
| `sdk` | `telemetry.sdk.name` / `telemetry.sdk.version` |
| `tags` | `service.name`, `otel.source` = `span_event` or `log` |
| `contexts.otel` | `resource` attributes and the span's or record's `attributes` (without `exception.*`) |
| `event_id` | hash of trace, span, time, type, message, stack trace: an OTLP retry is stored once |

Project: the project whose slug is the slugified `service.name`
(`Checkout API` → `checkout-api`); else `bridge.default_project`; with an
empty default, a new project named after the service. A missing project
is created on first use.

### Dedupe rule

An app may report the same exception twice: through its Sentry SDK and
through its OTel SDK. The rule: **an OTel exception is not stored when a
Sentry SDK event with the same `trace_id` and `span_id` was stored in the
last hour, up to `dedupe_window` after the OTel exception arrived.**

- The `EventObserver` puts (trace_id, span_id) of each stored SDK event
  in an in-memory set (1 h TTL, 200 000 entries).
- Each OTel exception waits in a queue (10 000 max) for `dedupe_window`,
  then is checked against that set and stored or dropped.
- Exceptions without both ids are always stored. Events made by the
  bridge do not enter the set, so two exceptions on one span are both
  stored.
- On shutdown the queue is stored without waiting (no Sentry event can
  arrive any more).

Metric: `mallard_bridge_exceptions_total{result=stored|duplicate|queue_full|no_project|sentry_full|error}`.

## Consequences
- OTel-only apps get issues, Sentry-only apps get traces, with no SDK
  change. Each library still builds and runs alone; the hooks are unused
  there.
- Dedupe is per process and in memory: after a restart, or with several
  replicas, an exception whose Sentry event went to another replica or
  arrived before the restart can be stored twice. A Sentry event that
  arrives later than `dedupe_window` does not remove the OTel one (tables
  are append-only).
- An app that sends the same spans through Sentry tracing and an OTLP
  exporter gets them twice in `otel.spans`; there is no span dedupe.
- OTel exceptions wait `dedupe_window` (10 s) before they show up as
  issues.
- Stack trace parsing is best effort; unknown formats keep the raw text.
