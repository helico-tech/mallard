# Storage backends

Both sides share one lake: schemas `sentry` and `otel` in one catalog. It has two
parts, configured independently (`[storage]`):

- **Catalog** (`storage.catalog`): a SQL database that holds the metadata.
  That is tables, snapshots, the list of files with per-column statistics, and
  inlined small inserts.
- **Data path** (`storage.data_path`): where the Parquet files go.

## Catalogs

| catalog | `storage.catalog` | processes | status |
|---|---|---|---|
| DuckDB file | `data/catalog.ducklake` (any path) | **one** | default; tested |
| SQLite file | `sqlite:data/catalog.sqlite` | **one** | tested |
| PostgreSQL | `postgres:host=pg port=5432 dbname=ducklake user=mallard` (password from `PGPASSWORD`) | many | tested with two processes (lease and writer tests) and one pod on kind |
| MySQL | `mysql:host=... database=... user=...` | many | supported by DuckLake, not tested here |

- Use a DuckDB or SQLite file for a single node. With PostgreSQL, several
  replicas (and other tools) can read and write the same lake at the same time.
- The PostgreSQL value after `postgres:` is a libpq connection string.
  Leave the password out of it and set the standard libpq environment
  variable `PGPASSWORD` (or `PGPASSFILE`) instead: libpq reads it, and it
  never appears in a connection string. Error messages are scrubbed of
  `password=` anyway. The database must exist; tables are created on first
  start.
- Managed PostgreSQL (RDS, Cloud SQL, Azure Database, Neon, Supabase) works
  like any PostgreSQL. A connection pooler in *transaction* mode is not
  recommended (DuckLake uses multi-statement transactions).

## Data paths

| storage | `storage.data_path` | secret `type` | notes |
|---|---|---|---|
| local disk / volume / NFS | `/data/lake/` | – | tested |
| AWS S3 | `s3://bucket/prefix/` | `s3` | `provider = "credential_chain"` uses env vars, `~/.aws`, IRSA, instance roles |
| S3-compatible (RustFS, MinIO, SeaweedFS, Ceph, Garage, Wasabi, Hetzner, ...) | `s3://bucket/prefix/` | `s3` | set `endpoint`, `url_style = "path"`, `use_ssl`; tested with RustFS (compose, kind) |
| Cloudflare R2 | `r2://bucket/prefix/` | `r2` | `key_id`, `secret`, `account_id` |
| Google Cloud Storage | `gs://bucket/prefix/` | `gcs` | HMAC `key_id`/`secret` |
| Azure Blob / ADLS Gen2 | `az://container/prefix/` or `abfss://container@account.dfs.core.windows.net/prefix/` | `azure` | `connection_string`, or `provider = "credential_chain"` / `service_principal` |

The needed DuckDB extensions (`httpfs`, `aws`, `azure`, `postgres`, `sqlite`,
`mysql`) are loaded automatically from the catalog and data path schemes and the
secret types. Offline checks (no network) confirmed that GCS, Azure and S3
`credential_chain` configurations load the right extensions. Writes to the
real cloud services were not part of the automated tests.

Sourcemap bundles have their own setting, `storage.artifacts_path`, which takes fewer
schemes (no `r2://`, no `abfss://`): see
[sentry/deployment.md](../sentry/deployment.md#storage).

### Examples

```toml
# AWS S3 with IAM (IRSA / instance profile / env vars)
[storage]
catalog = "postgres:host=pg dbname=ducklake user=mallard"   # password: PGPASSWORD env var
data_path = "s3://acme-telemetry/otel/"
[[storage.secrets]]
type = "s3"
provider = "credential_chain"
region = "eu-west-1"
```

```toml
# Google Cloud Storage (HMAC keys)
[storage]
data_path = "gs://acme-telemetry/otel/"
[[storage.secrets]]
type = "gcs"
key_id = "${GCS_HMAC_KEY}"
secret = "${GCS_HMAC_SECRET}"
```

```toml
# Azure Blob Storage
[storage]
data_path = "az://telemetry/otel/"
[[storage.secrets]]
type = "azure"
connection_string = "${AZURE_STORAGE_CONNECTION_STRING}"
```

```toml
# Cloudflare R2
[storage]
data_path = "r2://telemetry/otel/"
[[storage.secrets]]
type = "r2"
key_id = "${R2_KEY_ID}"
secret = "${R2_SECRET}"
account_id = "${R2_ACCOUNT_ID}"
```

## Choosing

| situation | catalog | data |
|---|---|---|
| one VM, simplest | DuckDB file | local disk |
| one VM, cheap long retention | DuckDB file | object storage |
| Kubernetes, one pod | DuckDB file on a PVC | PVC or object storage |
| HA / several replicas / other tools writing | PostgreSQL | object storage |

## Things to know

- **The data path is stored in the catalog** when it is created. Starting
  with a different `data_path` fails (`DATA_PATH ... does not match`). After
  moving the files, attach once with `OVERRIDE_DATA_PATH true` (DuckDB CLI).
- **One process per file catalog.** A second process gets a lock error. Use
  PostgreSQL for several.
- **Inlined rows live in the catalog** until compaction (default up to 10
  minutes and below 1000 rows per insert). With PostgreSQL that means a little
  telemetry sits in PostgreSQL. `storage.data_inlining_row_limit = 0` turns it off.
- **Latency matters for flushes**, not for ingest acknowledgements:
  receivers answer once data is in memory. A slow store only fills the buffer
  (see [operations.md](operations.md)).
- **Cost on object storage**: at most one PUT per table and day partition per
  flush (5 s). Busy tables: ~17k PUTs/day each; quiet tables are inlined into
  the catalog instead. With spans, logs and metrics busy that is ~1.5 M PUTs
  per month, about $7.50/month at S3 standard prices, plus compaction
  rewrites. Raise `otel.ingest.flush_interval` (e.g. `15s`) to cut it.
