# 0028. App scope: no app= filter, several services, All apps

## Status
accepted

## Context
Review of ADR 0027 against the code found three gaps. In the filter language
any unknown `key=value` is an attribute filter, so `app=x` already means something,
and `app` is a common Kubernetes label. Every telemetry endpoint filters on one exact
`service`, but one app can have two service names. And nothing said what the picker
shows before an app is chosen, while Sentry's issues, events and stats are per project.

## Decision
- No `app=` in the filter language, which supersedes that part of ADR 0027. App scope
  comes from `--app` and the picker. No old filter changes, and in the UI a filter
  cannot disagree with the picker.
- Telemetry endpoints with a `service` parameter accept it more than once. An app
  with two service names is scoped to both.
- "All apps" is a choice in the picker and the default. Telemetry shows every
  service as it does today. The errors side asks you to pick an app. There is no
  errors endpoint across projects.

## Consequences
- Plain OTel users keep their view across services.
- Errors across all projects stay out of reach in the UIs, until someone needs them.
- Eight telemetry endpoints change their `service` parameter; single values work as before.
