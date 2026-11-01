---
status: todo
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
Seen in `mallard-sentry-cli/src/cli/commands.rs`, not run.
```sh
curl -XPOST -H "Authorization: Bearer $T" $URL/sentry/v1/projects -d '{"name":"x","slug":"my_app"}' -H 'content-type: application/json'
mallard setup my_app
mallard apps
```

## Notes
Only for a project with a slug the rule never gives, which needs an explicit slug or
`bridge.default_project`. `setup` could ask `/api/v1/apps` for the app first, or `check_slug` could
forbid such slugs for new projects, which the issue 0142 notes also weighed.
