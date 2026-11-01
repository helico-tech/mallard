---
status: todo
type: debt
created: 2026-11-01
---
# 0050. `issues` and `issue_activity` rows stay after their events are gone

## Problem
Retention deletes `events` only. With the limit on new issues (issue
`work/sentry/issues/0002`) the `issues` table grows by at most 1000 rows per
project and hour, and nothing scans all of it at startup any more, but it never
shrinks. An issue past retention still answers `GET .../issues/{id}`, with no
events.

## Notes
- A delete needs "no event left for this group", which reads `events` once per
  run, and it conflicts with concurrent inserts into `issues` (ADR 0014), so it
  would run on the writer thread.
- Keeping the rows has a use: a resolved issue that comes back reopens.
