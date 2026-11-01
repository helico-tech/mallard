---
status: wontfix
type: debt
created: 2026-11-01
---
# Switch the sibling libraries to git dependencies

## Problem
The libraries are pinned through detached worktrees in the sibling repos
(ADR 0003). That only works on a machine with both repos checked out next to
this one, and the Docker build needs named build contexts.

## Notes
When sentry-ducklake and otel-ducklake are pushed (helico-tech/...), use
`git = "...", rev = "..."` deps, drop `scripts/pin-libs.sh` and `libs.lock`,
simplify the Dockerfile, and remove the `obs-pin` worktrees with
`git worktree remove` + `git worktree prune`.

Superseded by ADR 0004: the repos were merged into mallard.
