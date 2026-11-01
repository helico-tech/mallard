# 0003 — Pin the sibling libraries to commits

## Status
accepted

## Context
Plain path deps (`../otel-ducklake/crates/...`) build whatever is in the
sibling working tree, including half-finished edits. That broke our build
while otel-ducklake was mid-change. The repos have no git remote yet, so
git deps with `rev` are not possible.

## Decision
Each library is checked out as a detached worktree at a known commit in its
own repo: `../<lib>/.claude/worktrees/obs-pin` (that folder is git-ignored
there). Cargo path deps and the Docker build contexts point at it. The
commits are recorded in `libs.lock`; `scripts/pin-libs.sh` restores them,
or moves them with `scripts/pin-libs.sh <sentry-rev> <otel-rev>`.

Upgrading a library = move the pin, run `cargo test --workspace`, commit
`libs.lock` and `Cargo.lock`.

## Consequences
- Builds are reproducible and isolated from work in progress next door.
- The Docker context is small (no `target/` dirs).
- When the repos are pushed, replace this with `git = ..., rev = ...` deps
  and drop the worktrees and the script (`git worktree remove`).
