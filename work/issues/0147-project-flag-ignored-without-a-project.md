---
status: done
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

Chosen: an error for the flag only. `-p` on the command line is exit 2 where no project applies,
checked at parse time like `--app`, and before anything is asked. That is every telemetry command,
`status`, `trace`, `projects`, `sql`, `schema`, and `events list --trace`. `MALLARD_PROJECT` is only
a default and stays ignored there, and an empty `-p ""` counts as not given. A note on stderr would
keep exit 0 and data on stdout, which an agent reads as an answer for that project. Cost: a script
that passes `-p` on every call has to drop it there; the env var is the way to set it once. Same
break as ADR 0008 made before 1.0.
