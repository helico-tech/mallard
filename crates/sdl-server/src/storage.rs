//! Object storage for sourcemap bundles (`<url>/artifacts/`), through the
//! `object_store` crate. The standalone binary also keeps the lake under
//! the same URL (`<url>/lake/`, see [`lake`]); DuckDB reaches it through a
//! secret built from the same settings, so users configure storage once.

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use mallard_duck::{ObjectStoreOptions, SecretConfig};
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    S3,
    Gcs,
    Azure,
    Local,
}

/// A storage URL split up: kind, root URL with a trailing slash (as DuckDB
/// sees it), bucket or container, and the prefix inside it.
struct Location {
    kind: Kind,
    root: String,
    bucket: String,
    prefix: String,
}

fn locate(url: &str) -> Result<Location> {
    let url = url.trim().trim_end_matches('/');
    let split = |rest: &str| {
        let (b, p) = rest.split_once('/').unwrap_or((rest, ""));
        (b.to_string(), p.to_string())
    };
    let (kind, scheme, rest) = if let Some(r) = url.strip_prefix("s3://").or_else(|| url.strip_prefix("s3a://")) {
        (Kind::S3, "s3", r)
    } else if let Some(r) = url.strip_prefix("gs://").or_else(|| url.strip_prefix("gcs://")) {
        (Kind::Gcs, "gs", r)
    } else if let Some(r) = url.strip_prefix("az://").or_else(|| url.strip_prefix("azure://")) {
        (Kind::Azure, "az", r)
    } else {
        let path = url.strip_prefix("file://").unwrap_or(url);
        if path.contains("://") {
            bail!("unsupported storage url {url:?}; use s3://, gs://, az:// or a local path");
        }
        std::fs::create_dir_all(path).with_context(|| format!("creating {path}"))?;
        let abs = std::fs::canonicalize(path)?;
        return Ok(Location {
            kind: Kind::Local,
            root: format!("{}/", abs.display()),
            bucket: abs.display().to_string(),
            prefix: String::new(),
        });
    };
    let (bucket, prefix) = split(rest);
    Ok(Location { kind, root: format!("{scheme}://{bucket}/{}", with_slash(&prefix)), bucket, prefix })
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// GCS through its S3-compatible XML API with HMAC keys: one credential
/// works for both DuckDB and object_store.
fn gcs_keys(o: &ObjectStoreOptions) -> Result<(String, String)> {
    match (o.key_id.clone().or_else(|| env("GCS_HMAC_KEY_ID")), o.secret.clone().or_else(|| env("GCS_HMAC_SECRET"))) {
        (Some(k), Some(s)) => Ok((k, s)),
        // Fail now: otherwise object_store silently falls back to the AWS credentials.
        _ => bail!("gs:// storage needs HMAC keys (a gcs secret, or GCS_HMAC_KEY_ID and GCS_HMAC_SECRET)"),
    }
}

/// Where the standalone binary keeps its lake, and the DuckDB secret for it.
pub struct Lake {
    pub data_path: String,
    pub secret: Option<SecretConfig>,
}

/// The lake below `url` (`<url>/lake/`) with a DuckDB secret built from
/// `opts` and the `AWS_*` / `GCS_HMAC_*` / `AZURE_STORAGE_*` environment.
pub fn lake(url: &str, opts: &ObjectStoreOptions) -> Result<Lake> {
    let loc = locate(url)?;
    let scope = toml::Value::String(loc.root.trim_end_matches('/').to_string());
    let text = |s: &str| toml::Value::String(s.to_string());
    let mut o = std::collections::BTreeMap::new();
    match loc.kind {
        Kind::Local => {}
        Kind::S3 => {
            o.insert("type".into(), text("s3"));
            o.insert("scope".into(), scope);
            match (
                opts.key_id.clone().or_else(|| env("AWS_ACCESS_KEY_ID")),
                opts.secret.clone().or_else(|| env("AWS_SECRET_ACCESS_KEY")),
            ) {
                (Some(k), Some(s)) => {
                    o.insert("key_id".into(), text(&k));
                    o.insert("secret".into(), text(&s));
                    if let Some(t) = opts.session_token.clone().or_else(|| env("AWS_SESSION_TOKEN")) {
                        o.insert("session_token".into(), text(&t));
                    }
                }
                // IAM roles / IRSA / profiles via the aws extension.
                _ => {
                    o.insert("provider".into(), text("credential_chain"));
                }
            }
            if let Some(r) = &opts.region {
                o.insert("region".into(), text(r));
            }
            if let Some(ep) = &opts.endpoint {
                let (ssl, host) = match ep.split_once("://") {
                    Some(("http", h)) => (false, h),
                    Some((_, h)) => (true, h),
                    None => (true, ep.as_str()),
                };
                o.insert("endpoint".into(), text(host.trim_end_matches('/')));
                o.insert("use_ssl".into(), toml::Value::Boolean(ssl));
            }
            if opts.path_style {
                o.insert("url_style".into(), text("path"));
            }
        }
        Kind::Gcs => {
            let (k, s) = gcs_keys(opts)?;
            o.insert("type".into(), text("gcs"));
            o.insert("key_id".into(), text(&k));
            o.insert("secret".into(), text(&s));
            o.insert("scope".into(), scope);
        }
        Kind::Azure => {
            o.insert("type".into(), text("azure"));
            if let Some(cs) = env("AZURE_STORAGE_CONNECTION_STRING") {
                o.insert("connection_string".into(), text(&cs));
            } else {
                o.insert("provider".into(), text("credential_chain"));
                if let Some(a) = env("AZURE_STORAGE_ACCOUNT_NAME") {
                    o.insert("account_name".into(), text(&a));
                }
            }
        }
    }
    let secret = (!o.is_empty()).then(|| SecretConfig { name: Some("sdl_storage".into()), options: o });
    Ok(Lake { data_path: format!("{}lake/", loc.root), secret })
}

#[derive(Clone)]
pub struct Storage {
    /// Root URL with a trailing slash.
    root: String,
    store: Arc<dyn ObjectStore>,
    /// Prefix inside the object store for artifacts.
    artifacts: ObjPath,
}

impl Storage {
    /// Artifacts go to `<url>/artifacts/`.
    pub fn new(url: &str, opts: &ObjectStoreOptions) -> Result<Self> {
        let loc = locate(url)?;
        let store: Arc<dyn ObjectStore> = match loc.kind {
            Kind::S3 => {
                let keys = opts.key_id.clone().zip(opts.secret.clone());
                s3_store(&loc.bucket, opts, keys, opts.session_token.clone())?
            }
            Kind::Gcs => {
                let gcs = ObjectStoreOptions {
                    endpoint: Some("https://storage.googleapis.com".into()),
                    region: Some("auto".into()),
                    path_style: true,
                    ..Default::default()
                };
                s3_store(&loc.bucket, &gcs, Some(gcs_keys(opts)?), None)?
            }
            Kind::Azure => Arc::new(
                object_store::azure::MicrosoftAzureBuilder::from_env()
                    .with_container_name(&loc.bucket)
                    .build()
                    .context("configuring Azure blob storage")?,
            ),
            Kind::Local => Arc::new(object_store::local::LocalFileSystem::new_with_prefix(&loc.bucket)?),
        };
        let artifacts = ObjPath::from(format!("{}artifacts", with_slash(&loc.prefix)).trim_start_matches('/'));
        Ok(Storage { root: loc.root, store, artifacts })
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
}

fn with_slash(p: &str) -> String {
    let p = p.trim_matches('/');
    if p.is_empty() { String::new() } else { format!("{p}/") }
}

fn s3_store(
    bucket: &str,
    o: &ObjectStoreOptions,
    keys: Option<(String, String)>,
    token: Option<String>,
) -> Result<Arc<dyn ObjectStore>> {
    let region = o.region.as_deref().unwrap_or("us-east-1");
    let mut b = object_store::aws::AmazonS3Builder::from_env().with_bucket_name(bucket).with_region(region);
    if let Some((k, sec)) = keys {
        b = b.with_access_key_id(k).with_secret_access_key(sec);
        if let Some(t) = token {
            b = b.with_token(t);
        }
    }
    if let Some(ep) = &o.endpoint {
        b = b.with_endpoint(ep).with_allow_http(ep.starts_with("http://"));
    }
    // Same URL style as DuckDB's secret (URL_STYLE 'path' only when asked).
    b = b.with_virtual_hosted_style_request(!o.path_style);
    Ok(Arc::new(b.build().context("configuring S3 storage")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_urls_and_secret() {
        let opts = ObjectStoreOptions {
            endpoint: Some("http://minio:9000".into()),
            region: Some("us-east-1".into()),
            path_style: true,
            key_id: Some("k".into()),
            secret: Some("s".into()),
            ..Default::default()
        };
        let s = Storage::new("s3://bucket/some/prefix/", &opts).unwrap();
        assert_eq!(s.artifact_path_multi("chunks/ab").as_ref(), "some/prefix/artifacts/chunks/ab");
        let l = lake("s3://bucket/some/prefix/", &opts).unwrap();
        assert_eq!(l.data_path, "s3://bucket/some/prefix/lake/");
        let secret = l.secret.unwrap();
        assert_eq!(secret.get("endpoint"), Some("minio:9000"));
        assert_eq!(secret.options["use_ssl"], toml::Value::Boolean(false));
        assert_eq!(secret.get("url_style"), Some("path"));
        assert_eq!(secret.get("scope"), Some("s3://bucket/some/prefix"));
        assert_eq!(secret.get("key_id"), Some("k"));
    }

    #[tokio::test]
    async fn local_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let url = dir.path().to_str().unwrap();
        let s = Storage::new(url, &ObjectStoreOptions::default()).unwrap();
        let l = lake(url, &ObjectStoreOptions::default()).unwrap();
        assert!(l.data_path.ends_with("/lake/"));
        assert!(l.secret.is_none());
        s.put("chunks/abc", Bytes::from_static(b"hi")).await.unwrap();
        assert!(s.exists("chunks/abc").await.unwrap());
        assert_eq!(s.get("chunks/abc").await.unwrap().unwrap(), Bytes::from_static(b"hi"));
        s.delete("chunks/abc").await.unwrap();
        assert!(s.get("chunks/abc").await.unwrap().is_none());
    }
}
