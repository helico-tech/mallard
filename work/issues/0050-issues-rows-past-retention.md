---
status: wontfix
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

Decided 2026-11-01: keep the rows, no delete and no hiding.

- A delete is not worth its cost. The rows are small and the limit on new issues
  bounds them, while the delete reads `events` every run and pauses the writer
  (ADR 0014).
- It would also lose something: a resolved issue that returns would come back as a
  new issue, without its status history, `first_seen` and `first_release`.
- Hiding at read time is not better. Lists never show these issues: `list_issues`
  starts from the events in the window. Only a direct `GET .../issues/{id}` does,
  and it answers with title, status and history, count 0 and no latest event, the
  same as for any issue with no events in the window (`last_seen` then falls back
  to `first_seen`). A 404 would call an issue "not found" that is still known,
  whose id returns with its next event, and whose status can still be changed.

The behaviour is documented in `docs/sentry/data-model.md`.
