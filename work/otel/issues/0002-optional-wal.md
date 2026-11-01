---
status: todo
type: idea
created: 2026-11-01
---
# Optional write-ahead log

## Problem
Acknowledged data in memory is lost on a hard crash (up to flush_interval). Today the answer is a Collector with a persistent queue in front (ADR 0003). An optional local WAL would remove that need for single-node installs.

## Notes
Found during the v0.1 build and review (epic 0001).
