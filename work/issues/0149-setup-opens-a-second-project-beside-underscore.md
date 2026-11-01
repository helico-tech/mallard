---
status: done
type: bug
created: 2026-11-01
---
# `mallard setup my_app` opens a second project next to a project `my_app`

## Problem
The server now joins a project `my_app` and the service `my_app` (issue 0142). But `create_or_get`
looks up `slugify("my_app")`, which is `my-app`, finds nothing and creates that project. The exact
slug wins in `api::app_projects`, so the services and new OTel exceptions move to the new project,
while the Sentry SDK's errors stay on the old one. `POST /v1/projects` without a slug does the same.

## How to reproduce
Seen in `mallard-sentry-cli/src/cli/commands.rs`; run against a real `mallard serve` when it was fixed.
```sh
curl -XPOST -H "Authorization: Bearer $T" $URL/sentry/v1/projects -d '{"name":"x","slug":"my_app"}' -H 'content-type: application/json'
mallard setup my_app
mallard apps
```

## Notes
Only for a project with a slug the rule never gives, which needs an explicit slug or
`bridge.default_project`. `setup` could ask `/api/v1/apps` for the app first, or `check_slug` could
forbid such slugs for new projects, which the issue 0142 notes also weighed.

Reproduced first: `mallard apps` listed `my-app` (project 2) and `my_app` (project 1).

Fixed at the cause, not only in the CLI: `POST /v1/projects` without a slug compares through the slug
rule, and a project with that slug under it is a 409 that names it. `_` stays allowed (ADR 0029) and
`check_slug` did not change. `create_or_get` no longer sends the slug it derived: on that 409 it
lists the projects and takes the one `app_projects` picks, so it stays idempotent. `service_slug` and
`app_projects` moved to `mallard-sentry-api`, so server, bridge and CLI use one copy of the rule.
An exact slug still wins, so a server that has both keeps using `my-app`.

Not done: a slug that was asked for is taken as is, see issue 0150. An old CLI sends the slug it derived,
so it still opens the second project; the CLI and the server ship as one binary.
