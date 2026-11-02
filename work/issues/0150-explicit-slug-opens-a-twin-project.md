---
status: done
type: bug
created: 2026-11-01
---
# A slug that is asked for can open a second project of the same app

## Problem
Issue 0149 made a name without a slug find the project that has that slug under the slug rule. A slug
that is asked for is still taken as is. So `mallard setup z --slug my-app` next to a project `my_app`
creates `my-app`. The exact slug wins in `app_projects`, so the services and new OTel exceptions of
`my_app` move to the new project, while the Sentry SDK's errors stay on the old one. That is the split
of issue 0149, but someone asked for this exact slug. `bridge.default_project` goes the same way.

## How to reproduce
Run against a real `mallard serve`:
```sh
curl -XPOST -H "Authorization: Bearer $T" $URL/sentry/v1/projects -d '{"name":"x","slug":"my_app"}' -H 'content-type: application/json'
mallard setup z --slug my-app
mallard apps      # my-app and my_app
```

## Notes
Options: a 409 for such a slug too, which changes what an explicit slug may do and must not break
`Server::create_project` for the bridge, or a warning. A choice for the user, so not made in 0149.
`a_name_without_a_slug_compares_through_the_slug_rule` pins today's behaviour.

Owner's decision: refuse with a 409 (ADR 0031). `WriteCmd::CreateProject` now compares through the slug
rule inside its transaction, for a slug that was asked for and one derived from a name, so the check in
the handler from issue 0149 went away and two spellings at once make one project. `setup --slug` and
`projects create --slug --if-not-exists` exit 5 and name the project.

The bridge: `Server::create_project` answers with the id of the project that is the same app, so a
`bridge.default_project` whose twin exists uses the twin. Its exceptions go there and no project is
created. Tested with the issue's commands against a real server
(`a_slug_that_is_asked_for_does_not_open_a_twin`) and for the bridge in both spellings
(`the_default_project_uses_its_twin`).
