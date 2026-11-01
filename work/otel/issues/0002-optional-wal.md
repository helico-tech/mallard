---
status: wontfix
type: idea
created: 2026-11-01
---
# Optional write-ahead log

## Problem
Acknowledged data in memory is lost on a hard crash (up to flush_interval). Today the answer is a Collector with a persistent queue in front (ADR 0003). An optional local WAL would remove that need for single-node installs.

## Notes
Found during the v0.1 build and review (epic 0001).

Decision: not built, see [ADR 0013](../../../docs/adr/0013-no-write-ahead-log.md).
In short: it protects at most 5 s of telemetry on a hard crash, but costs a
disk format, segment rotation tied to the flusher, replay, duplicate handling,
disk-full handling, and a volume per replica (which would make the stateless
Helm setup stateful). A Collector with a persistent queue needs no code and
also covers the server being down.

The recipe in `docs/otel/deployment.md` was improved: contrib image, no `batch`
processor (it holds data in memory before the durable queue), `fsync` for power
loss, ingest token, Kubernetes note. It was not run against a real Collector
here (no binary available).
