---
status: done
created: 2026-11-01
---
# capacity

## Goal
Measure ingest throughput and query latency under CPU/memory limits; document sizing.

## Acceptance criteria
- Built, tested (cargo test), documented.

## Plan
See epic and docs/architecture.md.

## Progress log
- 2026-11-01: created.
- 2026-11-01: Measured: 2-CPU container saturates at ~83k spans/s; steady 3.4k spans/s with flat memory; 7-day 68M-span lake: 5-52 ms for 15m/1h queries with 2 threads. docs/capacity.md.

## Follow-ups
