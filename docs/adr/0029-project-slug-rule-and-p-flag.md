# 0029. Project slugs join by the slug rule; -p is an error where it means nothing

## Status
accepted

## Context
ADR 0027 joins a project and a service when their slugs are equal, but `check_slug`
allows `_` and the slug rule turns it into `-`, so the project `my_app` never joined
the service `my_app`. Separately, `-p` is a global flag: `mallard services -p web` and
`mallard events list --trace <id> -p web` ran and ignored it, and the second looks like
an answer for `web`.

## Decision
- A project's slug goes through the same rule as a service name when apps are joined.
  The app keeps the project's real slug, because clients send it back. If several
  projects give one slug, the exact slug wins, then the oldest. `_` stays allowed.
- `-p` on the command line is a usage error (exit 2) on commands where no project
  applies, like `--app`. This refines ADR 0008. `MALLARD_PROJECT` stays harmless there, and
  an empty `-p ""` counts as not given.

## Consequences
- Scripts that pass `-p` on every call must drop it on those commands. A note on stderr
  would not break them, but exit 0 with data still reads as an answer for that project.
- A server that already has both `my_app` and a bridge-made `my-app` keeps both; nothing
  stored is merged.
