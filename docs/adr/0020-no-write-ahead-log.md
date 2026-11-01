# 0020 — No local write-ahead log; a Collector with a persistent queue gives at-least-once

## Status
accepted (answers the last consequence of otel ADR 0003)

## Context
OTLP ingest acknowledges data when it is in the memory buffer. A hard crash
(SIGKILL, OOM kill, power loss) loses at most `ingest.flush_interval` (5 s)
of it; a clean shutdown loses nothing. Otel ADR 0003 left open adding a local
write-ahead log (WAL) behind a flag, so that single-node installs would not
need a Collector for at-least-once delivery (otel issue 0002).

Options:
- **WAL, fsync before each ack.** Covers process and power loss. Costs: a
  new on-disk format and segment rotation tied to the flusher (a flush must
  retire exactly the segments it committed, also after a retry or a partial
  drop of one table); replay at start-up, before the receivers open; ingest
  hooks (the bridge) must not fire twice on replay; duplicates when a crash
  falls between the catalog commit and the truncate; disk-full as a new
  failure mode; one more config section. Every replica needs its own
  volume: the Helm chart's stateless Postgres + object storage setup would
  turn into a StatefulSet with PVCs. At the target load (tens of requests
  per second) fsync itself is affordable; the code and operations are the
  cost.
- **WAL without fsync.** Cheaper, and page cache survives a process kill, but
  not power or node loss. All of the above except fsync, for half the
  guarantee.
- **Collector with a persistent queue** (`file_storage`). No code here: a
  standard, widely used component. Also covers what a WAL does not: the server
  down for an upgrade or unreachable. Costs one more process and a volume for
  those who want it; everyone else pays nothing.
- **Acknowledge after the flush commits** (as the Sentry side does: it answers
  once its batch has committed). No disk or replay: if the process dies, the
  exporter gets no answer and retries. Costs request latency up to the flush
  interval, which limits SDK exporters that wait for one export at a time.
  Not built: nobody needs it yet.

## Decision
No WAL. Users who cannot lose the last seconds run a Collector with a
persistent queue in front; the recipe is in `docs/otel/deployment.md`.

## Consequences
- The server stays stateless on object storage, and there is one durability
  story to explain and test: exporters retry, the Collector persists.
- On a hard crash without a Collector, up to `flush_interval` of acknowledged
  data is lost. Lowering `flush_interval` narrows that window.
- The Sentry side is unaffected: it acknowledges after commit (sentry ADR
  0001).
- If a real install needs durability without a Collector, try "acknowledge
  after flush" first: it is far smaller than a WAL.
