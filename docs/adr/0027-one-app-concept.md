# 0027. One word for project and service: app

## Status
accepted; partly superseded by 0028 (`app=` filter)

## Context
Sentry data belongs to a project, named by the DSN. OTel data belongs to a
service, named by `service.name`. The UIs show two lists and two filters for
what is usually one program. Three things already link them by name:
`mallard setup` sets `OTEL_SERVICE_NAME` to the project slug, the bridge names
Sentry spans after the project slug, and it sends OTel exceptions to the project
whose slug matches the service.

Options were: match by name; a server-side mapping table, which handles any
names but adds schema, config and a screen; or a project on all OTLP data, which
stops plain OTel apps from working without extra setup.

## Decision
An app is a project and a service whose slugs are equal. The slug rule is the
bridge's (`Checkout API` → `checkout-api`). Nothing new is stored. The server
works out the list, so clients do not each carry the rule.

The UIs say "app". The Sentry-facing words stay where others depend on them: the
DSN, the API paths, `-p/--project`, `service=`. `--app` and `app=` are added next
to them. The ADR 0008 CLI contract does not change for existing flags.

An app with one side still is an app, and shows which side it has.

## Consequences
- Names that differ show as two apps. The UI says so and offers the fix. A
  mapping table can come later if this turns out to be common; nothing here
  blocks it.
- One project with several services through the `service.name` tag: errors stay
  with the project's app, and spans go to the app of each service.
- Two service names with one slug (`Checkout API`, `checkout-api`) are one app.
