# 0003. In-memory buffering with back-pressure; no queue in front

Date: 2026-11-01

## Status

accepted

## Context

Writing a Parquet file per OTLP request would create millions of tiny files
and S3 PUTs. So ingest must batch, and batching means data sits somewhere
before it is written. Options:

1. **Memory buffer** + periodic flush, bounded, with back-pressure.
2. **Local write-ahead log** (append raw requests to disk, replay after a
   crash). Needs a persistent volume per replica and replay logic.
3. **Kafka / Redpanda / an S3-backed queue** in front, consumed by a writer.
   Durable and decoupled, but an extra stateful system to run. S3-backed
   queues add ~50-100 ms latency and per-PUT cost per append.

Target load is small/mid: tens to a few thousand spans per second. One
process ingests >250k spans/s (see docs/capacity.md), so a queue is not
needed for throughput.

## Decision

Option 1. Requests are converted to Arrow and kept in a bounded buffer
(`ingest.max_buffer_bytes`). They are flushed every `flush_interval` (5 s) in
one transaction. When the buffer is full, receivers return retryable errors
(HTTP 429 / gRPC `RESOURCE_EXHAUSTED`). Failed flushes are retried, never
dropped. On SIGTERM the buffer is flushed.

## Consequences

- At most `flush_interval` of acknowledged data is lost on a hard crash
  (SIGKILL, OOM kill, power loss). A clean shutdown loses nothing.
- Users who need at-least-once delivery put a standard OpenTelemetry
  Collector in front, with a persistent sending queue
  (`exporters.otlphttp.sending_queue.storage: file_storage`). It keeps data on
  disk until otel-ducklake accepts it. No code needed on our side
  (documented in docs/deployment.md).
- A WAL can be added later behind a config flag, without changing the
  storage format.
