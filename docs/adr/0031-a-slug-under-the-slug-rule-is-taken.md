# 0031. A slug that is the same under the slug rule is taken

## Status
accepted; refines 0029

## Context
ADR 0029 joins projects and services through the slug rule, and when several projects give one
slug, the exact slug wins. Issue 0149 made a name without a slug find the project it joins. A slug
that was asked for still went through as is, so `--slug my-app` next to `my_app` created a second
project. It took the services and new OTel exceptions of `my_app` while the Sentry SDK's errors
stayed there: one app split in two (issue 0150). `bridge.default_project` could do the same.

## Decision
- A new project whose `service_slug` equals that of another project is a conflict: `POST
  /v1/projects` answers 409 and names that project, the CLI exits 5. This holds for a slug that was
  asked for and for one derived from a name. `_` stays allowed in a slug, so `my_app` alone is fine.
- The writer checks this inside the transaction that creates the project, so two spellings at once
  cannot both pass, also from two processes.
- `mallard setup --slug` and `projects create --slug --if-not-exists` return a project only if it has
  exactly that slug. Otherwise they exit 5: the caller asked for this slug, and getting another one
  back would silently change what the app sends.
- `Server::create_project`, the bridge's way in, returns the id of the project that is the same app
  instead of an error. So with `bridge.default_project = "catch-all"` next to a project `catch_all`,
  exceptions go to `catch_all` and no project is created. The bridge already uses the existing
  project for a taken slug. Dropping the exceptions would lose data, and failing at startup would
  miss a project made later.

## Consequences
- "The exact slug wins" from ADR 0029 now only matters for a catalog that already has both. Such a
  catalog keeps both, and nothing is merged.
- Asked for `catch-all`, the bridge may store exceptions under `catch_all`. `mallard apps` shows it.
