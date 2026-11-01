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
**`duckdb.memory_limit = "1GB"`, `otel.ingest.max_buffer_bytes = "128MB"`, `duckdb.threads = 4`,** with the two
changes of [ADR 0025](../../docs/adr/0025-memory-budget-of-one-container.md): the image's `MALLOC_ARENA_MAX=2` and a
16 MB target file for `events`. On compose (Postgres + RustFS) at 300 events/s + 2,800 spans/s for 26 minutes: 448k
events, 5.4M OTel rows, peak RSS 1.94 GiB, no kill, no DuckDB out-of-memory errors, 13/13 Sentry and 21/21 OTel
maintenance runs ok, nothing rejected. That is little margin. It does not hold 1,000 events/s + 11,200 spans/s;
for that, 4 GiB with 2GB + 256MB did (3.00 GiB).

With both sides at full speed, the same setting was still killed after 6 minutes.

Neither candidate we started with works by itself: 1GB + 512MB and 768MB + 256MB were both killed in 2 GiB at
145-190k events. The new rule is limit + buffer + 0.9 GiB.

All numbers, with durations and thread counts: [capacity](../../docs/otel/capacity.md#both-sides-in-one-container).
Scripts: `bench/mallard/memory`. Applied in `deploy/compose/docker-compose.yml`, `deploy/docker/Dockerfile`,
`config/mallard.example.toml`, and the capacity, operations and configuration docs.

Rough edges, also in that page: each setting ran once; arm64 with other builds running; RustFS on the same
machine, not a real S3 store over the network; no run longer than 26 minutes. See issues 0130-0133.
