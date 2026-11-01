---
status: todo
type: idea
created: 2026-11-01
---
# Exemplars are not stored

## Problem
Metric exemplars (links from metric points to traces) are dropped at ingest. Storing them as a nested list per point would allow "jump from a latency spike to a trace".

## Notes
Found during the v0.1 build and review (epic 0001).
