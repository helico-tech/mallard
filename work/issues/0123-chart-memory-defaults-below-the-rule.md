---
status: done
type: question
created: 2026-11-01
---
# What should a 2 GiB container run, with both sides in one process?

## Problem
The documented rule was container ≥ `duckdb.memory_limit` + `otel.ingest.max_buffer_bytes` + 1 GB, it counted
only telemetry, and its numbers came from the old binaries. History: the Helm chart, which has since been
removed, had 1GB, the default 512 MB buffer and a 2 GiB limit, which broke the rule; hence this issue's
filename. Found during issue 0016.

## Notes
### Answer, measured 2026-11-01 on `mallard`
**`duckdb.memory_limit = "1GB"`, `otel.ingest.max_buffer_bytes = "128MB"`, `duckdb.threads = 4`,** with the changes
of [ADR 0025](../../docs/adr/0025-memory-budget-of-one-container.md): the image's `MALLOC_ARENA_MAX=2`, a 16 MB target
file for `events`, and Sentry's `flush_rows` 10000 and `compact_interval` 2m. On the committed compose file (Postgres +
RustFS) with nothing else overridden, at 300 events/s + 2,800 spans/s for 31 minutes: 563k events and 6.5M OTel rows stored, peak
RSS 1.84 GiB, no kill, no DuckDB out-of-memory errors, 15/15 Sentry compactions and 4/4 OTel maintenance runs ok, nothing
rejected. Close settings (1200MB + 128MB, 768MB + 256MB) were killed at 1,000 events/s + 11,200 spans/s before the two Sentry
defaults changed; that load was not run again in 2 GiB. For it, 4 GiB with 2GB + 256MB and 8 threads held for 31 minutes (3.02 GiB, 15/15).

The two settings we started with do not work: 1GB + 512MB and 768MB + 256MB were both killed in 2 GiB at 145-190k events. The
new rule is limit + buffer + 0.9 GiB.

An early answer here was wrong. It passed on shortened intervals (flush every 30 s), but on the shipped intervals it was
killed in 10 minutes. Those runs are in the same page.

All numbers, with durations and thread counts: [capacity](../../docs/otel/capacity.md#both-sides-in-one-container).
Scripts: `bench/mallard/memory`. Applied in `deploy/compose/docker-compose.yml`, `deploy/docker/Dockerfile`,
`crates/mallard-sentry-server` (defaults), `config/mallard.example.toml`, and the capacity, operations and configuration docs.

Rough edges, also in that page: each setting ran once; arm64 with other builds running; RustFS on the same
machine, not a real S3 store over the network; the final settings ran 31 minutes, the longest related run 76. See
issues 0130-0133.
