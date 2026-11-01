---
status: todo
type: bug
created: 2026-11-01
---
# A project slug with `_` never joins its service

## Problem
`check_slug` allows `_` in a project slug, and `POST /v1/projects` with `slug: "my_app"`
or `bridge.default_project` can create one. `service_slug` turns `_` into `-`. So the
service `my_app` has the slug `my-app`:

- `GET /api/v1/apps` shows two apps, `my-app` and `my_app`, though `mallard setup` gave
  them the same name.
- The bridge names Sentry spans after the project slug and sends OTel exceptions to
  the project of `service_slug(service)`, so exceptions of `my_app` open a new project
  `my-app`.

`mallard setup my_app` is not affected: without a given slug the project gets
`slugify`'s `my-app`.

## How to reproduce
```sh
curl -XPOST -H "Authorization: Bearer $T" $URL/sentry/v1/projects -d '{"name":"x","slug":"my_app"}' -H 'content-type: application/json'
# send a span with service.name=my_app, then
mallard apps
```

## Notes
ADR 0027 says "the slug rule is the bridge's", so this stays as it is until decided.
Options: forbid `_` in new slugs (simple; existing ones stay split), or let a service
whose exact name is a project slug join that project (one more rule, in `service_slug`'s
caller and the bridge). I would forbid `_`.
