---
status: todo
type: debt
created: 2026-11-01
---
# `-p` is silently ignored where it means nothing

## Problem
`-p` is a global flag, so `mallard services -p web` and `mallard events list --trace <id> -p web`
run and ignore it. The second one lists events of all projects, which looks like an answer for
`web`. `--app` is a usage error (exit 2) in those places (story 0004/03).

## Notes
A usage error for `-p` would break scripts that set it on every call, and `MALLARD_PROJECT` must
stay harmless. So it needs a choice: a note on stderr, or an error for the flag only.
