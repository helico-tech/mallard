---
status: done
type: bug
created: 2026-11-01
---
# `mallard setup 1` prints the setup of project id 1

## Problem
`create_or_get` first asks `GET /v1/projects/{slug}`, and that route takes an id or a slug. So a name
that is a number, which `check_slug` forbids as a slug, finds the project with that id: `mallard setup 1`
says `project exists: my_app (id 1)` and prints that project's DSN and `OTEL_SERVICE_NAME`. An app
named `1` then sends its data to another app's project, with exit 0. `mallard setup 99` with no such
id is the expected usage error (exit 2). `projects create 1 --if-not-exists` shares the code.

## How to reproduce
Run against a real `mallard serve`, with a project with id 1:
```sh
mallard setup 1
```

## Notes
Found while fixing issue 0149, which did not change it. A likely fix: `create_or_get` compares the
answer's slug with what it asked for. Note that `--slug 1` has the same lookup.

## What changed
`create_or_get` keeps an answer only when its slug is the one it asked for. A number that finds a project by
id goes on to the create, and the server refuses it: `slug must not be numeric`, a 400, so exit 2 (ADR 0008),
as `mallard setup 99` already did. It holds for `setup 1`, `setup x --slug 1`, `projects create 1 --if-not-exists`
and `projects create x --slug 1 --if-not-exists`. Other routes still take an id.
Test: `a_number_does_not_get_the_project_with_that_id`, against a real `mallard serve`; before the fix `setup 1`
exited 0.
