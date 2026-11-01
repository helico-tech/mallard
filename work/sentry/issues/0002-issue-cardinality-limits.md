---
status: done
type: debt
created: 2026-11-01
---
# 0002. No limit on the number of issues per project

## Problem
`issues` rows and the in-memory issue-status cache never shrink. A noisy
`captureMessage("order 8f3a... failed")` whose variable part is not masked
by grouping, or a custom fingerprint with ids in it, can create one issue per
event: up to ~1M issues a day, a multi-GB cache and a slow startup.

## Notes
- Fix sketch: cap new issues per project per hour (overflow goes into one
  "too many issues" group, counted in a metric), and expire issues whose last
  event is older than the retention period (drop from cache; append-only
  `issues` rows can be rewritten during compaction or filtered by last seen).
- Found by the data-model review (2026-11-01).

### What changed (done)
- Limit: 1000 new issues per project, hour and process (fixed; no config key:
  far above a real release, and only grouping gone wrong reaches it). Further
  new groups go into one issue per project, "Too many new issues"; counted in
  `sdl_issue_overflow_events_total`, logged as a warning. Events keep their own
  title and payload. Known groups are not affected.
- Cache: `moka`, at most 200,000 issues, `time_to_idle` = retention. Not loaded
  at startup any more. A miss asks the lake (one query per batch, for all
  missing groups), so eviction costs no duplicate rows and no lost status.
- `issues` rows are not deleted past retention: they are small once cold, nothing
  scans them at startup any more, and a returning issue keeps its status
  (follow-up: `work/issues/0050-issues-rows-past-retention.md`).
- Docs: `docs/sentry/data-model.md`, `docs/sentry/deployment.md`.
- Tests: `db::tests::new_issues_over_the_limit_share_one_issue`,
  `issue_outside_the_cache_is_found_in_the_lake`, `issue_cache_is_bounded`,
  `writer::tests::new_issue_count_is_per_project_and_hour`.
