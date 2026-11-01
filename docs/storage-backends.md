# Storage backends

A DuckLake lake has two parts, configured independently:

- **Catalog** (`storage.catalog`): a SQL database that holds the metadata.
  That is tables, snapshots, the list of files with per-column statistics, and
  inlined small inserts.
- **Data path** (`storage.data_path`): where the Parquet files go.

## Catalogs

| catalog | `storage.catalog` | processes | status |
|---|---|---|---|
| DuckDB file | `data/catalog.ducklake` (any path) | **one** | default; tested |
| SQLite file | `sqlite:data/catalog.sqlite` | **one** | tested |
| PostgreSQL | `postgres:host=pg port=5432 dbname=ducklake user=odl password=${PGPASSWORD}` | many | tested with 2 replicas (compose and Kubernetes) |
| MySQL | `mysql:host=... database=... user=...` | many | supported by DuckLake, not tested here |

- Use a DuckDB or SQLite file for a single node. With PostgreSQL, several
  collector replicas (and other tools) can read and write the same lake at
  the same time.
- The PostgreSQL value after `postgres:` is a libpq connection string. Use
  `${ENV}` for the password. The database must exist; tables are created on
  first start.
- Managed PostgreSQL (RDS, Cloud SQL, Azure Database, Neon, Supabase) works
  like any PostgreSQL. A connection pooler in *transaction* mode is not
  recommended (DuckLake uses multi-statement transactions).

## Data paths

| storage | `storage.data_path` | secret `type` | notes |
|---|---|---|---|
| local disk / volume / NFS | `/data/lake/` | – | tested |
| AWS S3 | `s3://bucket/prefix/` | `s3` | `provider = "credential_chain"` uses env vars, `~/.aws`, IRSA, instance roles |
| S3-compatible (MinIO, SeaweedFS, Ceph, Garage, Wasabi, Hetzner, ...) | `s3://bucket/prefix/` | `s3` | set `endpoint`, `url_style = "path"`, `use_ssl`; tested with SeaweedFS |
| Cloudflare R2 | `r2://bucket/prefix/` | `r2` | `key_id`, `secret`, `account_id` |
| Google Cloud Storage | `gs://bucket/prefix/` | `gcs` | HMAC `key_id`/`secret` |
| Azure Blob / ADLS Gen2 | `az://container/prefix/` or `abfss://container@account.dfs.core.windows.net/prefix/` | `azure` | `connection_string`, or `provider = "credential_chain"` / `service_principal` |

The needed DuckDB extensions (`httpfs`, `aws`, `azure`, `postgres`,
`sqlite`) are loaded automatically from the catalog and data path schemes and
the secret types. Offline checks (no network) confirmed that GCS, Azure and S3
`credential_chain` configurations load the right extensions. Writes to the
real cloud services were not part of the automated tests.

### Examples

```toml
# AWS S3 with IAM (IRSA / instance profile / env vars)
[storage]
catalog = "postgres:host=pg dbname=ducklake user=odl password=${PGPASSWORD}"
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
  telemetry sits in PostgreSQL. `data_inlining_row_limit = 0` turns it off.
- **Latency matters for flushes**, not for ingest acknowledgements:
  receivers answer once data is in memory. A slow store only fills the buffer
  (see [operations.md](operations.md)).
- **Cost on object storage**: one PUT per table and day partition per flush
  (5 s), so ~50k PUTs/day before compaction, plus compaction rewrites. That's
  cents per month on S3 pricing.
