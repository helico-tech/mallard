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
Snapshot expiry and file cleanup are catalog-wide (`maintenance.catalogWide`);
running them on every replica is safe but redundant. Set it to `false` when
another application sharing the catalog already does this.

## Auth

```yaml
auth:
  enabled: true
  apiToken: "..."       # or existingSecret with keys api-token, ingest-token
  ingestToken: ""       # empty: OTLP ingest without auth (typical in-cluster)
```

`query.enableSql: true` (ad-hoc SQL endpoint) requires `auth.enabled`.

## Resources

Defaults (requests 500m/1Gi, limits 2 CPU/2Gi, `duckdb.memoryLimit: 1400MB`)
fit small and mid-sized setups. Keep `duckdb.memoryLimit` at about 70% of the
memory limit. The root filesystem is read-only; DuckDB spills to `/data/tmp`.

See `values.yaml` for every option.
