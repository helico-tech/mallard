---
status: todo
type: question
created: 2026-11-02
---
# Can OTel retention keep losing on replicas?

## Problem
[ADR 0033](../../docs/adr/0033-retention-and-flushes-take-turns.md) lets retention's `DELETE` and the flushes take turns inside
one process. Replicas do not share those locks, so a `DELETE` on one still conflicts with the flushes of the others. A
`DELETE` slower than the time between flushes may then never win, as in
[issue 0155](0155-otel-retention-loses-to-the-flusher.md), and the rows and snapshots stay.

## Notes
Not measured. The ADR lists one option that would help here: delete in small slices, each shorter than the time between
flushes. It costs more code and more snapshots, and slices smaller than a day write delete files.
