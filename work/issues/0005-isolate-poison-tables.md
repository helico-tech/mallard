---
status: todo
type: debt
created: 2026-11-01
---
# One table with a permanent write error blocks all signals

## Problem
All tables are flushed in one transaction. A permanent error on one table (e.g. a hand-edited incompatible column type) blocks traces, logs and metrics until fixed, and the buffer fills. Detect permanent errors and park the affected table's batches.

## Notes
Found during the v0.1 build and review (epic 0001).
