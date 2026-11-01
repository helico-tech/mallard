---
status: todo
type: bug
created: 2026-11-01
---
# Sourcemap storage on Azure ignores the connection string, and `check` misses it

## Problem
The config example says `artifacts_path` credentials come from the
`[[storage.secrets]]` entry whose scope covers it. For `az://` that is not so:
`mallard_duck::object_store_options` returns nothing for that scheme, and
`mallard_sentry_server`'s storage uses `MicrosoftAzureBuilder::from_env()`, which does
not read `AZURE_STORAGE_CONNECTION_STRING`. So the secret that works for the lake does
not work for sourcemaps, and the server does not start:

    error: starting sentry: configuring Azure blob storage: Generic MicrosoftAzure error: Account must be specified

Also, `mallard check --connect` says `config ok, catalog and storage reachable` for this
config, and for `artifacts_path = "r2://..."`, which fails at start-up the same way
(`unsupported storage url`).

## How to reproduce
`MALLARD_SERVER__API_TOKENS=dev MALLARD_STORAGE__ARTIFACTS_PATH=az://c/artifacts
AZURE_STORAGE_CONNECTION_STRING='AccountName=a;AccountKey=aGVsbG8=' mallard serve`.
The same variables with `check --connect` print ok.

## Notes
Options: pass the `azure` secret's connection string to the store, or document only the
`AZURE_STORAGE_ACCOUNT_*` variables and fix the example comment. Either way `check
--connect` should build the sourcemap store. Real Azure was not tried, only the failure
at start-up. Found during issue 0016; `docs/sentry/deployment.md` says what works today.
