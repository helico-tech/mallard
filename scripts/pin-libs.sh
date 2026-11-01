#!/usr/bin/env bash
# Pin the sibling libraries to commits (ADR 0003).
#
#   scripts/pin-libs.sh                      # check out the commits in libs.lock
#   scripts/pin-libs.sh <sentry-rev> <otel-rev>   # move the pins, update libs.lock
#
# Each library is a detached worktree in its own repo:
#   ../sentry-ducklake/.claude/worktrees/obs-pin
#   ../otel-ducklake/.claude/worktrees/obs-pin
# Cargo path deps and the Docker build contexts point there, so edits in
# progress in the sibling repos never reach this build.
set -euo pipefail
cd "$(dirname "$0")/.."
lock=libs.lock

pin() { # repo rev
  local repo="../$1" wt=".claude/worktrees/obs-pin"
  if [ ! -d "$repo/$wt" ]; then
    git -C "$repo" worktree add --detach "$wt" "$2" >/dev/null
  else
    git -C "$repo/$wt" checkout --quiet --detach "$2"
  fi
  git -C "$repo/$wt" rev-parse HEAD
}

if [ $# -eq 2 ]; then
  s=$(pin sentry-ducklake "$1"); o=$(pin otel-ducklake "$2")
  printf 'sentry-ducklake %s\notel-ducklake %s\n' "$s" "$o" > "$lock"
elif [ $# -eq 0 ]; then
  while read -r repo rev; do pin "$repo" "$rev" >/dev/null; done < "$lock"
else
  echo "usage: $0 [<sentry-rev> <otel-rev>]" >&2; exit 2
fi
cat "$lock"
