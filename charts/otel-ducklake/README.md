# otel-ducklake Helm chart

Deploys the otel-ducklake collector: OTLP/gRPC (4317), OTLP/HTTP (4318) and
the query API (8080, docs at `/docs`).

## Modes

| `mode` | Workload | Catalog | Data | Replicas |
|---|---|---|---|---|
| `single` (default) | StatefulSet + PVC | DuckDB file on the PVC | PVC (or `storage.dataPath` on object storage) | always 1 |
| `shared` | Deployment | PostgreSQL (`storage.catalog`) | object storage (`storage.dataPath`) | `replicaCount` |

A DuckDB or SQLite catalog file allows only one writer, so `single` is pinned
to one replica. Use `shared` to run several replicas.

## Quick start (single)

```sh
helm install otel charts/otel-ducklake -n observability --create-namespace
kubectl -n observability port-forward svc/otel-otel-ducklake 8080:8080
odl --url http://localhost:8080 status
```

## Shared mode (PostgreSQL + S3)

```yaml
mode: shared
replicaCount: 2
storage:
  catalog: "postgres:host=pg.db dbname=ducklake user=odl password=${PGPASSWORD}"
  dataPath: "s3://my-bucket/otel/"
  secrets:
    - type: s3
      provider: credential_chain      # IRSA / EKS Pod Identity
      region: eu-west-1
envFrom:
  - secretRef: {name: odl-storage}   # contains PGPASSWORD (and S3 keys if not using IRSA)
serviceAccount:
  annotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123456789012:role/otel-ducklake
```

`${NAME}` in any config value is replaced from the pod environment, so
credentials stay in Secrets. Other stores: `gs://` (secret `type: gcs`),
`az://`/`abfss://` (secret `type: azure`), `r2://`, any S3-compatible endpoint
(`endpoint`, `url_style: path`, `use_ssl`).

**Maintenance in shared mode.** Every replica runs compaction and retention
for the tables; concurrent runs occasionally conflict ("another transaction
has compacted it"), which is harmless: the loser retries next interval.
Snapshot expiry and file cleanup are catalog-wide (`maintenance.catalogWide`).
The rule: *within one Helm release* running them on every replica is safe
(idempotent, just redundant); *per shared catalog*, exactly one
**application** should own them. If another application (a second release,
an embedding app) shares the catalog and already runs them, set
`maintenance.catalogWide: false` here.

## Auth

```yaml
auth:
  enabled: true
  apiToken: "..."       # or existingSecret: a Secret with key api-token
  ingestAuth: false     # true: OTLP ingest also needs a token (ingestToken / key ingest-token)
  ingestToken: ""
```

- Tokens must not be empty: the server refuses to start with an empty token.
  The chart refuses to render `auth.enabled` without `apiToken` or
  `existingSecret`, and `auth.ingestAuth` without an ingest token.
- `ingress.enabled: true` requires `auth.enabled: true` (rendering fails
  otherwise): the ingress exposes the query API, which can read all telemetry.
- `query.enableSql: true` (ad-hoc SQL endpoint) requires `auth.enabled`.
- Pods get `ODL_TOKEN` from the same Secret, so `kubectl exec ... odl status`
  works. Changing chart-managed tokens restarts the pods (`checksum/secret`).
  After rotating a token inside an `existingSecret`, restart the pods
  yourself (`kubectl rollout restart`).

## Other settings

Any server setting not exposed in `values.yaml` can be set with an
environment override (`ODL_<SECTION>__<KEY>`, see docs/configuration.md):

```yaml
extraEnv:
  - {name: ODL_STORAGE__SCHEMA, value: otel}
  - {name: ODL_MAINTENANCE__SNAPSHOT_RETENTION, value: 2h}
```

`extraConfig` is appended to the generated TOML and is only meant for extra
`[[storage.secrets]]` blocks: every other section is already rendered, and
TOML forbids repeating a section.

## Resources

Defaults (requests 500m/1Gi, limits 2 CPU/2Gi, `duckdb.memoryLimit: 768MB`,
`ingest.maxBufferBytes: 256MB`) fit small and mid-sized setups with large
headroom (a 2-CPU pod ingests ~80k spans/s at saturation). Peak memory is
about `duckdb.memoryLimit + ingest.maxBufferBytes + 1 GB` (Parquet writers,
compaction and allocator overhead; 0.84 GB was measured during compaction
with a 1 GB DuckDB limit). Keep that under the memory limit; for bigger pods
raise `duckdb.memoryLimit` accordingly (e.g. 4Gi → 2.5GB). The root
filesystem is read-only; DuckDB spills to `/data/tmp`.

## Upgrades

- The single-mode StatefulSet's `volumeClaimTemplates` are immutable:
  `persistence.size`, `storageClass` and `accessModes` cannot change with
  `helm upgrade`. Resize the PVC directly (if the storage class allows
  expansion) or reinstall.
- Switching `mode: single` → `shared` does not migrate data: the DuckDB-file
  catalog on the PVC is not copied into PostgreSQL. Start fresh, or migrate
  the catalog yourself first.
- Releases that add table columns roll normally: rows are appended by column
  name, so old pods keep writing (new columns stay NULL for their rows) and
  an older version starts against a newer table. Releases that change or
  remove columns will say so in their notes.
- The image on GHCR must be public, or set `imagePullSecrets`.

See `values.yaml` for every option.
