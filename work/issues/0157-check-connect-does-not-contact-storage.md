---
status: done
type: bug
created: 2026-11-02
---
# `check --connect` says storage is reachable without contacting it

## Problem
`mallard check --connect` prints `config ok, catalog and storage reachable`, but it only
attaches the catalog and builds the sourcemap store. Neither step has to reach the object
store, so a wrong key or a missing container or bucket passes, and shows up at the first
flush or upload. This holds for every backend, not only Azure.

Also, `artifacts_path = "az://account.blob.core.windows.net/container/x"` is read as a
container with that name. The store takes the account from the connection string, so this
fails late with a confusing error, or not at all until the first upload.

## How to reproduce
A config with `data_path` and `artifacts_path` on `az://` and a wrong `AccountKey`:
`mallard check -c <file> --connect` prints ok.

## Plan
- `check --connect` writes a small file to `data_path` and reads it back, and writes and
  deletes a small object in the sourcemap store. A failure is exit 1, as the docs say.
- An `az://` `artifacts_path` whose first part has a dot fails at start-up and in `check`
  (exit 2) and says what to write. Azure container names cannot have dots.
- `data_path` keeps accepting what DuckDB accepts.

## Notes
Found while checking that Azure Blob works against Azurite.

Done, as planned, with two choices:
- The sourcemap store is checked by writing and deleting, not by listing: the server never
  lists there, and listing would pass with a read-only key.
- DuckDB SQL cannot delete a file, so the `data_path` check leaves `mallard-check.parquet`.
  Each check overwrites that one file and orphan cleanup removes it.

The store check stops after 30 seconds, because `object_store` retries for minutes. The
DuckDB check has no such limit of its own; with nothing listening it failed in 6 seconds.

Tested against Azurite with `mallard check -c <file> --connect`:

| case | result |
|---|---|
| good config, twice | exit 0, one blob left |
| wrong key | exit 1, no key in the message |
| missing lake container | exit 1 |
| missing sourcemap container | exit 1 |
| `az://<account>.blob.core.windows.net/...` as `artifacts_path` | exit 2 |
| nothing listening | exit 1 in 6 seconds, stopped at the lake check |

`storage::tests::azure_roundtrip` passes against Azurite too. Not tried: real Azure, S3 and
GCS (the code is the same for them, the requests are not), a wrong key only on the sourcemap
store, the 30-second limit at its full length, and orphan cleanup of this file on object
storage (a unit test covers a local path).
