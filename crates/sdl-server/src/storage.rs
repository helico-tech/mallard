//! Storage abstraction.
//!
//! One storage URL holds everything:
//!
//! ```text
//! <storage_url>/lake/        DuckLake Parquet data files (written by DuckDB)
//! <storage_url>/artifacts/   sourcemap chunks and bundles (written by us)
//! ```
//!
//! DuckDB reaches the lake through its own filesystem layer (httpfs/azure),
//! configured with a secret built here. Artifacts go through the
//! `object_store` crate. Both read the same credentials, so users configure
//! storage once.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    S3,
    Gcs,
    Azure,
    Local,
}

#[derive(Clone)]
pub struct Storage {
    pub kind: Kind,
    /// Root URL with a trailing slash, as DuckDB sees it.
    pub root: String,
    store: Arc<dyn ObjectStore>,
    /// Prefix inside the object store for artifacts.
    artifacts: ObjPath,
    s3: S3Settings,
}

#[derive(Clone, Debug, Default)]
struct S3Settings {
    endpoint: Option<String>,
    region: String,
    path_style: bool,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Escape a value for a single-quoted SQL string.
pub fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

impl Storage {
    pub fn new(url: &str, cfg: &Config) -> Result<Self> {
        let url = url.trim().trim_end_matches('/').to_string();
        let s3 = S3Settings {
            endpoint: cfg.s3_endpoint.clone(),
            region: cfg.s3_region.clone(),
            path_style: cfg.s3_path_style,
        };
        let (kind, root, store, prefix): (Kind, String, Arc<dyn ObjectStore>, String) =
            if let Some(rest) = url.strip_prefix("s3://").or_else(|| url.strip_prefix("s3a://")) {
                let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
                let store = s3_store(bucket, &s3, None)?;
                (Kind::S3, format!("s3://{bucket}/{}", with_slash(prefix)), store, prefix.to_string())
            } else if let Some(rest) = url.strip_prefix("gs://").or_else(|| url.strip_prefix("gcs://")) {
                // GCS through its S3-compatible XML API with HMAC keys: one
                // credential works for both DuckDB and object_store.
                let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
                let gcs = S3Settings {
                    endpoint: Some("https://storage.googleapis.com".into()),
                    region: "auto".into(),
                    path_style: true,
                };
                let keys = (env("GCS_HMAC_KEY_ID"), env("GCS_HMAC_SECRET"));
                let store = s3_store(bucket, &gcs, Some(keys))?;
                (Kind::Gcs, format!("gs://{bucket}/{}", with_slash(prefix)), store, prefix.to_string())
            } else if let Some(rest) = url.strip_prefix("az://").or_else(|| url.strip_prefix("azure://")) {
                let (container, prefix) = rest.split_once('/').unwrap_or((rest, ""));
                let store = object_store::azure::MicrosoftAzureBuilder::from_env()
                    .with_container_name(container)
                    .build()
                    .context("configuring Azure blob storage")?;
                (Kind::Azure, format!("az://{container}/{}", with_slash(prefix)), Arc::new(store), prefix.to_string())
            } else {
                let path = url.strip_prefix("file://").unwrap_or(&url);
                if path.contains("://") {
                    bail!("unsupported storage url {url:?}; use s3://, gs://, az:// or a local path");
                }
                std::fs::create_dir_all(path).with_context(|| format!("creating {path}"))?;
                let abs = std::fs::canonicalize(path)?;
                let store = object_store::local::LocalFileSystem::new_with_prefix(&abs)?;
                (Kind::Local, format!("{}/", abs.display()), Arc::new(store), String::new())
            };
        let artifacts = ObjPath::from(format!("{}artifacts", with_slash(&prefix)).trim_start_matches('/'));
        Ok(Storage { kind, root, store, artifacts, s3 })
    }

    /// DuckLake `DATA_PATH`.
    pub fn lake_data_path(&self) -> String {
        format!("{}lake/", self.root)
    }

    /// Extensions DuckDB needs for this storage.
    pub fn duckdb_extensions(&self) -> &'static [&'static str] {
        match self.kind {
            Kind::S3 | Kind::Gcs => &["httpfs"],
            Kind::Azure => &["azure"],
            Kind::Local => &[],
        }
    }

    /// `CREATE SECRET` statement so DuckDB can read and write the lake.
    pub fn duckdb_secret_sql(&self) -> Option<String> {
        match self.kind {
            Kind::Local => None,
            Kind::S3 => {
                let scope = self.root.trim_end_matches('/');
                let mut parts = vec!["TYPE s3".to_string(), format!("SCOPE {}", sql_str(scope))];
                match (env("AWS_ACCESS_KEY_ID"), env("AWS_SECRET_ACCESS_KEY")) {
                    (Some(k), Some(s)) => {
                        parts.push(format!("KEY_ID {}", sql_str(&k)));
                        parts.push(format!("SECRET {}", sql_str(&s)));
                        if let Some(t) = env("AWS_SESSION_TOKEN") {
                            parts.push(format!("SESSION_TOKEN {}", sql_str(&t)));
                        }
                    }
                    // IAM roles / IRSA / profiles via the aws extension.
                    _ => parts.push("PROVIDER credential_chain".into()),
                }
                parts.push(format!("REGION {}", sql_str(&self.s3.region)));
                if let Some(ep) = &self.s3.endpoint {
                    let (ssl, host) = match ep.split_once("://") {
                        Some(("http", h)) => (false, h),
                        Some((_, h)) => (true, h),
                        None => (true, ep.as_str()),
                    };
                    parts.push(format!("ENDPOINT {}", sql_str(host.trim_end_matches('/'))));
                    parts.push(format!("USE_SSL {ssl}"));
                }
                if self.s3.path_style {
                    parts.push("URL_STYLE 'path'".into());
                }
                Some(format!("CREATE OR REPLACE SECRET sdl_storage ({})", parts.join(", ")))
            }
            Kind::Gcs => {
                let k = env("GCS_HMAC_KEY_ID").unwrap_or_default();
                let s = env("GCS_HMAC_SECRET").unwrap_or_default();
                Some(format!(
                    "CREATE OR REPLACE SECRET sdl_storage (TYPE gcs, KEY_ID {}, SECRET {}, SCOPE {})",
                    sql_str(&k),
                    sql_str(&s),
                    sql_str(self.root.trim_end_matches('/'))
                ))
            }
            Kind::Azure => {
                let mut parts = vec!["TYPE azure".to_string()];
                if let Some(cs) = env("AZURE_STORAGE_CONNECTION_STRING") {
                    parts.push(format!("CONNECTION_STRING {}", sql_str(&cs)));
                } else {
                    parts.push("PROVIDER credential_chain".into());
                    if let Some(a) = env("AZURE_STORAGE_ACCOUNT_NAME") {
                        parts.push(format!("ACCOUNT_NAME {}", sql_str(&a)));
                    }
                }
                Some(format!("CREATE OR REPLACE SECRET sdl_storage ({})", parts.join(", ")))
            }
        }
    }

    pub fn describe(&self) -> String {
        self.root.clone()
    }

    pub async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        self.store.put(&self.artifact_path_multi(key), PutPayload::from(data)).await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        match self.store.get(&self.artifact_path_multi(key)).await {
            Ok(r) => Ok(Some(r.bytes().await?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn exists(&self, key: &str) -> Result<bool> {
        match self.store.head(&self.artifact_path_multi(key)).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        match self.store.delete(&self.artifact_path_multi(key)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// `a/b/c` keys -> nested object path below `artifacts/`.
    fn artifact_path_multi(&self, key: &str) -> ObjPath {
        key.split('/').filter(|s| !s.is_empty()).fold(self.artifacts.clone(), |p, part| p.join(part))
    }

    /// Write and read back a probe object; used by readiness checks.
    pub async fn probe(&self) -> Result<()> {
        let key = "_probe";
        self.put(key, Bytes::from_static(b"ok")).await?;
        self.get(key).await?.context("probe object missing after write")?;
        Ok(())
    }
}

fn with_slash(p: &str) -> String {
    let p = p.trim_matches('/');
    if p.is_empty() { String::new() } else { format!("{p}/") }
}

fn s3_store(
    bucket: &str,
    s: &S3Settings,
    keys: Option<(Option<String>, Option<String>)>,
) -> Result<Arc<dyn ObjectStore>> {
    let mut b = object_store::aws::AmazonS3Builder::from_env().with_bucket_name(bucket).with_region(&s.region);
    if let Some((Some(k), Some(sec))) = keys {
        b = b.with_access_key_id(k).with_secret_access_key(sec);
    }
    if let Some(ep) = &s.endpoint {
        b = b.with_endpoint(ep).with_allow_http(ep.starts_with("http://"));
    }
    // Same URL style as DuckDB's secret (URL_STYLE 'path' only when asked).
    b = b.with_virtual_hosted_style_request(!s.path_style);
    Ok(Arc::new(b.build().context("configuring S3 storage")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        c: Config,
    }

    fn cfg(args: &[&str]) -> Config {
        let mut v = vec!["t"];
        v.extend_from_slice(args);
        T::parse_from(v).c
    }

    #[test]
    fn s3_urls_and_secret() {
        let s =
            Storage::new("s3://bucket/some/prefix/", &cfg(&["--s3-endpoint", "http://minio:9000", "--s3-path-style"]))
                .unwrap();
        assert_eq!(s.kind, Kind::S3);
        assert_eq!(s.lake_data_path(), "s3://bucket/some/prefix/lake/");
        assert_eq!(s.artifact_path_multi("chunks/ab").as_ref(), "some/prefix/artifacts/chunks/ab");
        let sql = s.duckdb_secret_sql().unwrap();
        assert!(sql.contains("ENDPOINT 'minio:9000'"), "{sql}");
        assert!(sql.contains("USE_SSL false"));
        assert!(sql.contains("URL_STYLE 'path'"));
        assert!(sql.contains("SCOPE 's3://bucket/some/prefix'"));
    }

    #[tokio::test]
    async fn local_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_str().unwrap(), &cfg(&[])).unwrap();
        assert_eq!(s.kind, Kind::Local);
        assert!(s.lake_data_path().ends_with("/lake/"));
        assert!(s.duckdb_secret_sql().is_none());
        s.put("chunks/abc", Bytes::from_static(b"hi")).await.unwrap();
        assert!(s.exists("chunks/abc").await.unwrap());
        assert_eq!(s.get("chunks/abc").await.unwrap().unwrap(), Bytes::from_static(b"hi"));
        s.delete("chunks/abc").await.unwrap();
        assert!(s.get("chunks/abc").await.unwrap().is_none());
        s.probe().await.unwrap();
    }
}
