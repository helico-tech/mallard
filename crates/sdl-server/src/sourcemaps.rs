//! Sourcemaps: the upload protocol of `sentry-cli` / bundler plugins, and
//! the resolver that feeds symbolication at ingest.
//!
//! Upload (see docs/references/sentry-sourcemap-protocol.md):
//! 1. `GET  /api/0/organizations/{org}/chunk-upload/`      -> options
//! 2. `POST /api/0/organizations/{org}/artifactbundle/assemble/` -> missing chunks
//! 3. `POST /api/0/organizations/{org}/chunk-upload/`      -> chunks (multipart)
//! 4. `POST .../assemble/` again -> we assemble synchronously and answer `ok`
//!
//! Storage layout (object store, below `artifacts/`):
//! `chunks/<sha1>` until assembled, then `bundles/<sha1>.zip`.

use crate::db::read::{self, ArtifactLocation};
use crate::db::{Db, NewBundle, NewBundleFile, WriteCmd};
use crate::storage::Storage;
use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use moka::future::Cache;
use sdl_protocol::symbolicate::{self, ResolvedSourceMap, SourceMapRef};
use serde::Deserialize;
use serde_json::Value;
use sha1::{Digest, Sha1};
use sourcemap::SourceView;
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

pub const CHUNK_SIZE: usize = 8 * 1024 * 1024;
pub const CHUNKS_PER_REQUEST: usize = 64;
pub const MAX_REQUEST_SIZE: usize = 32 * 1024 * 1024;
pub const MAX_BUNDLE_SIZE: usize = 512 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct AssembleRequest {
    pub checksum: String,
    #[serde(default)]
    pub chunks: Vec<String>,
    #[serde(default)]
    pub projects: Vec<Value>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub dist: Option<String>,
}

pub fn is_sha1_hex(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn sha1_hex(data: &[u8]) -> String {
    hex::encode(Sha1::digest(data))
}

/// Strip the 8-byte `SYSB` source-bundle header if present.
pub fn strip_bundle_header(data: &[u8]) -> &[u8] {
    if data.len() >= 8 && &data[..4] == b"SYSB" { &data[8..] } else { data }
}

/// The zip part of an uploaded bundle. sentry-cli (symbolic) writes the
/// `SYSB` header and then the zip into the same stream, so the zip offsets
/// count from the start of the file: such a file must be opened whole (zip
/// readers skip leading data). Cutting the header off would shift every
/// offset by 8 bytes. Writers whose offsets count from the zip start need
/// the header cut, so fall back to that.
pub fn bundle_zip(data: &[u8]) -> &[u8] {
    if zip::ZipArchive::new(std::io::Cursor::new(data)).is_ok() { data } else { strip_bundle_header(data) }
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    #[serde(rename = "type", default)]
    ty: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    files: HashMap<String, ManifestFile>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    dist: Option<String>,
}

/// Read `manifest.json` from a bundle zip into index rows.
pub fn index_bundle(zip_bytes: &[u8]) -> Result<(Vec<NewBundleFile>, Option<String>, Option<String>)> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).context("bundle is not a zip file")?;
    let mut manifest = String::new();
    zip.by_name("manifest.json").context("bundle has no manifest.json")?.read_to_string(&mut manifest)?;
    let m: Manifest = serde_json::from_str(&manifest).context("invalid manifest.json")?;
    let mut files = Vec::new();
    for (path, f) in m.files {
        let headers: HashMap<String, String> =
            f.headers.into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
        let debug_id = headers.get("debug-id").and_then(|d| uuid::Uuid::try_parse(d).ok());
        let sourcemap_url = headers.get("sourcemap").or_else(|| headers.get("x-sourcemap")).cloned();
        let file_type = f.ty.unwrap_or_else(|| "source".into());
        if !matches!(file_type.as_str(), "minified_source" | "source_map" | "source" | "indexed_ram_bundle") {
            continue;
        }
        files.push(NewBundleFile { debug_id, url: f.url, file_type, zip_path: path, sourcemap_url });
    }
    Ok((files, m.release.filter(|r| r != "undefined" && !r.is_empty()), m.dist))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AssembleResponse {
    pub state: &'static str,
    #[serde(rename = "missingChunks")]
    pub missing_chunks: Vec<String>,
    pub detail: Option<String>,
}

impl AssembleResponse {
    fn state(state: &'static str) -> Self {
        AssembleResponse { state, missing_chunks: vec![], detail: None }
    }
    fn error(detail: impl Into<String>) -> Self {
        AssembleResponse { state: "error", missing_chunks: vec![], detail: Some(detail.into()) }
    }
}

type ParsedMap = Arc<ResolvedSourceMap>;

pub struct SourceMaps {
    storage: Option<Storage>,
    /// Bundle zips by object key.
    bundles: Cache<String, Bytes>,
    /// Parsed maps by (bundle, zip path).
    maps: Cache<(String, String), Option<ParsedMap>>,
    /// Lookup results by (project, debug id or release/url key).
    lookups: Cache<(u32, String), Option<ArtifactLocation>>,
    /// Checksums currently being assembled.
    assembling: tokio::sync::Mutex<std::collections::HashSet<String>>,
}

impl SourceMaps {
    pub fn new(storage: Option<Storage>, cache_mb: u64) -> Self {
        let budget = cache_mb.max(16) * 1024 * 1024;
        SourceMaps {
            storage,
            bundles: Cache::builder()
                .max_capacity(budget / 2)
                .weigher(|_k: &String, v: &Bytes| v.len().try_into().unwrap_or(u32::MAX))
                .time_to_idle(Duration::from_secs(3600))
                .build(),
            maps: Cache::builder()
                .max_capacity(budget / 2)
                // A parsed map costs roughly 4x its token count in bytes; we
                // only know the source size cheaply, so weigh by tokens.
                .weigher(|_k: &(String, String), v: &Option<ParsedMap>| {
                    v.as_ref()
                        .map_or(64, |m| (m.map.get_token_count() as usize * 24 + 1024).try_into().unwrap_or(u32::MAX))
                })
                .time_to_idle(Duration::from_secs(3600))
                .build(),
            lookups: Cache::builder().max_capacity(50_000).time_to_live(Duration::from_secs(60)).build(),
            assembling: Default::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.storage.is_some()
    }

    fn storage(&self) -> Result<&Storage> {
        self.storage.as_ref().ok_or_else(|| anyhow!("sourcemap storage is not configured"))
    }

    /// Store uploaded chunks (already decompressed) after checking checksums.
    pub async fn store_chunk(&self, claimed: &str, data: Bytes) -> Result<()> {
        if !is_sha1_hex(claimed) {
            bail!("chunk filename must be a sha1 checksum");
        }
        if data.len() > CHUNK_SIZE {
            bail!("Chunk size too large");
        }
        if sha1_hex(&data) != claimed {
            bail!("Checksum mismatch");
        }
        self.storage()?.put(&format!("chunks/{claimed}"), data).await
    }

    pub async fn assemble(&self, db: &Db, req: AssembleRequest) -> Result<AssembleResponse> {
        let storage = self.storage()?;
        if !is_sha1_hex(&req.checksum) || !req.chunks.iter().all(|c| is_sha1_hex(c)) {
            bail!("checksum and chunks must be sha1 hex strings");
        }
        if req.projects.is_empty() {
            bail!("You need to specify at least one project");
        }
        if req.dist.is_some() && req.version.is_none() {
            bail!("You need to specify a release together with a dist");
        }
        let mut project_ids = Vec::new();
        for p in &req.projects {
            let r = match p {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => bail!("One or more projects are invalid"),
            };
            let proj = db.caches.project_by_ref(&r).ok_or_else(|| anyhow!("One or more projects are invalid"))?;
            project_ids.push(proj.id);
        }
        let object_key = format!("bundles/{}.zip", req.checksum);

        // Missing chunks first (as Sentry does), unless already assembled.
        let mut missing = Vec::new();
        for c in &req.chunks {
            if !storage.exists(&format!("chunks/{c}")).await? {
                missing.push(c.clone());
            }
        }
        let already = db.caches.bundle_known(&req.checksum, &project_ids);
        if already {
            return Ok(AssembleResponse::state("ok"));
        }
        if !missing.is_empty() {
            return Ok(AssembleResponse { state: "not_found", missing_chunks: missing, detail: None });
        }
        if req.chunks.is_empty() {
            return Ok(AssembleResponse { state: "not_found", missing_chunks: vec![], detail: None });
        }
        {
            let mut busy = self.assembling.lock().await;
            if !busy.insert(req.checksum.clone()) {
                return Ok(AssembleResponse::state("assembling"));
            }
        }
        let result = self.do_assemble(db, storage, &req, project_ids, &object_key).await;
        self.assembling.lock().await.remove(&req.checksum);
        match result {
            Ok(()) => Ok(AssembleResponse::state("ok")),
            Err(e) => {
                tracing::warn!(checksum = %req.checksum, error = %e, "assembling bundle failed");
                Ok(AssembleResponse::error(e.to_string()))
            }
        }
    }

    async fn do_assemble(
        &self,
        db: &Db,
        storage: &Storage,
        req: &AssembleRequest,
        project_ids: Vec<u32>,
        object_key: &str,
    ) -> Result<()> {
        let mut data = Vec::new();
        let mut hasher = Sha1::new();
        for c in &req.chunks {
            let chunk = storage
                .get(&format!("chunks/{c}"))
                .await?
                .ok_or_else(|| anyhow!("Not all chunks available for assembling"))?;
            hasher.update(&chunk);
            data.extend_from_slice(&chunk);
            if data.len() > MAX_BUNDLE_SIZE {
                bail!("bundle is too large");
            }
        }
        if hex::encode(hasher.finalize()) != req.checksum {
            bail!("Reported checksum mismatch");
        }
        let zip_bytes = Bytes::from(bundle_zip(&data).to_vec());
        let (files, m_release, m_dist) = {
            let z = zip_bytes.clone();
            tokio::task::spawn_blocking(move || index_bundle(&z)).await??
        };
        let release = req.version.clone().filter(|v| v != "undefined" && !v.is_empty()).or(m_release);
        let dist = req.dist.clone().or(m_dist);
        if files.iter().all(|f| f.debug_id.is_none()) && release.is_none() {
            bail!("uploading a bundle without debug ids or release is prohibited");
        }
        storage.put(object_key, zip_bytes.clone()).await?;
        let size_bytes = zip_bytes.len() as u64;
        let bundle = NewBundle {
            bundle_id: req.checksum.clone(),
            project_ids: project_ids.clone(),
            release,
            dist,
            object_key: object_key.to_string(),
            size_bytes,
            files,
        };
        let nfiles = bundle.files.len();
        db.call(|reply| WriteCmd::AddBundle { bundle: Box::new(bundle), reply }).await?;
        for c in &req.chunks {
            storage.delete(&format!("chunks/{c}")).await?;
        }
        // New maps may resolve frames that missed before.
        self.lookups.invalidate_all();
        tracing::info!(checksum = %req.checksum, files = nfiles, bytes = size_bytes, "stored artifact bundle");
        Ok(())
    }

    async fn locate(
        &self,
        db: &Db,
        project_id: u32,
        r: &SourceMapRef,
        release: Option<&str>,
        dist: Option<&str>,
    ) -> Option<(ArtifactLocation, &'static str)> {
        if let Some(id) = r.debug_id {
            let key = (project_id, format!("id:{id}"));
            let loc = self
                .lookups
                .get_with(key, async {
                    db.read(move |c, ns| read::find_by_debug_id(c, ns, project_id, id)).await.unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "debug id lookup failed");
                        None
                    })
                })
                .await;
            if let Some(l) = loc {
                return Some((l, "debug-id"));
            }
        }
        let release = release?.to_string();
        let dist = dist.map(str::to_string);
        let candidates = symbolicate::url_candidates(&r.abs_path);
        let key = (project_id, format!("url:{release}\0{}\0{}", dist.as_deref().unwrap_or(""), r.abs_path));
        let loc = self
            .lookups
            .get_with(key, async move {
                db.read(move |c, ns| read::find_by_url(c, ns, project_id, &release, dist.as_deref(), &candidates))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "release artifact lookup failed");
                        None
                    })
            })
            .await;
        loc.map(|l| (l, "release"))
    }

    async fn load_map(&self, loc: &ArtifactLocation, resolved_with: &'static str, abs_path: &str) -> Option<ParsedMap> {
        let storage = self.storage.as_ref()?;
        let key = (loc.bundle_id.clone(), loc.map_path.clone());
        let loc = loc.clone();
        let abs_path = abs_path.to_string();
        self.maps
            .get_with(key, async move {
                let zip = match self
                    .bundles
                    .try_get_with(loc.object_key.clone(), async {
                        storage.get(&loc.object_key).await?.ok_or_else(|| anyhow!("bundle object missing"))
                    })
                    .await
                {
                    Ok(z) => z,
                    Err(e) => {
                        tracing::warn!(bundle = %loc.bundle_id, error = %e, "loading bundle failed");
                        return None;
                    }
                };
                let parsed =
                    tokio::task::spawn_blocking(move || parse_from_bundle(&zip, &loc, resolved_with, &abs_path)).await;
                match parsed {
                    Ok(Ok(m)) => Some(Arc::new(m)),
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "parsing sourcemap failed");
                        None
                    }
                    Err(_) => None,
                }
            })
            .await
    }

    /// Resolve every minified file referenced by the event's frames.
    pub async fn resolve(&self, db: &Db, project_id: u32, event: &Value) -> HashMap<String, ParsedMap> {
        let mut out = HashMap::new();
        if !self.enabled() || !db.caches.project_has_bundles(project_id) {
            return out;
        }
        let release = event.get("release").and_then(Value::as_str);
        let dist = event.get("dist").and_then(Value::as_str);
        for r in symbolicate::collect_refs(event) {
            if let Some((loc, how)) = self.locate(db, project_id, &r, release, dist).await
                && let Some(m) = self.load_map(&loc, how, &r.abs_path).await
            {
                out.insert(r.abs_path.clone(), m);
            }
        }
        out
    }
}

fn read_entry(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, path: &str) -> Result<Vec<u8>> {
    let mut f = zip.by_name(path).with_context(|| format!("bundle entry {path} missing"))?;
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn parse_from_bundle(
    zip_bytes: &Bytes,
    loc: &ArtifactLocation,
    how: &'static str,
    abs_path: &str,
) -> Result<ResolvedSourceMap> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes[..]))?;
    let map_bytes = read_entry(&mut zip, &loc.map_path)?;
    let map = symbolicate::parse_sourcemap(&map_bytes)?;
    let minified = match &loc.minified_path {
        Some(p) => {
            read_entry(&mut zip, p).ok().and_then(|b| String::from_utf8(b).ok()).map(|s| SourceView::new(Arc::from(s)))
        }
        None => None,
    };
    // Relative `sources` resolve against the map's location. With `~/x.map`
    // urls we use the frame's own origin.
    let map_url = match loc.map_url.as_deref() {
        Some(u) if u.starts_with("~/") => match url::Url::parse(abs_path) {
            Ok(base) => base.join(&u[1..]).map(|u| u.to_string()).unwrap_or_else(|_| u.to_string()),
            Err(_) => u[1..].to_string(),
        },
        Some(u) => u.to_string(),
        None => format!("{abs_path}.map"),
    };
    Ok(ResolvedSourceMap { map, minified, map_url, resolved_with: how })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// (zip path, type, url, headers, content)
    type BundleFile<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)], &'a [u8]);

    pub fn make_bundle(files: &[BundleFile<'_>], release: Option<&str>) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            let mut manifest = serde_json::json!({"files": {}, "org": "default"});
            if let Some(r) = release {
                manifest["release"] = r.into();
            }
            for (path, ty, url, headers, content) in files {
                let h: serde_json::Map<String, Value> =
                    headers.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect();
                manifest["files"][*path] = serde_json::json!({"type": ty, "url": url, "headers": h});
                z.start_file(*path, opts).unwrap();
                z.write_all(content).unwrap();
            }
            z.start_file("manifest.json", opts).unwrap();
            z.write_all(manifest.to_string().as_bytes()).unwrap();
            z.finish().unwrap();
        }
        let mut out = b"SYSB\x02\x00\x00\x00".to_vec();
        out.extend(buf.into_inner());
        out
    }

    #[test]
    fn indexes_manifest_with_header() {
        let b = make_bundle(
            &[
                (
                    "files/_/_/main.js",
                    "minified_source",
                    "~/main.js",
                    &[("Debug-Id", "c941d872-af1f-4f0c-a7ff-ad3d295fe153"), ("Sourcemap", "main.js.map")],
                    b"x",
                ),
                (
                    "files/_/_/main.js.map",
                    "source_map",
                    "~/main.js.map",
                    &[("debug-id", "c941d872-af1f-4f0c-a7ff-ad3d295fe153")],
                    b"{}",
                ),
            ],
            Some("1.0"),
        );
        let (files, release, _) = index_bundle(bundle_zip(&b)).unwrap();
        assert_eq!(release.as_deref(), Some("1.0"));
        assert_eq!(files.len(), 2);
        let js = files.iter().find(|f| f.file_type == "minified_source").unwrap();
        assert_eq!(js.sourcemap_url.as_deref(), Some("main.js.map"));
        assert!(js.debug_id.is_some());
    }

    /// Like sentry-cli: header and zip written to one stream, so the zip's
    /// offsets include the 8 header bytes.
    #[test]
    fn opens_bundle_with_absolute_offsets() {
        let mut buf = std::io::Cursor::new(b"SYSB\x02\x00\x00\x00".to_vec());
        buf.set_position(8);
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file("files/_/_/main.js", opts).unwrap();
            z.write_all(b"x").unwrap();
            z.start_file("manifest.json", opts).unwrap();
            z.write_all(br#"{"files":{"files/_/_/main.js":{"type":"minified_source","url":"~/main.js","headers":{"debug-id":"c941d872-af1f-4f0c-a7ff-ad3d295fe153"}}}}"#).unwrap();
            z.finish().unwrap();
        }
        let data = buf.into_inner();
        assert_eq!(&data[..4], b"SYSB");
        let (files, _, _) = index_bundle(bundle_zip(&data)).unwrap();
        assert_eq!(files.len(), 1);
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bundle_zip(&data))).unwrap();
        assert_eq!(read_entry(&mut zip, "files/_/_/main.js").unwrap(), b"x");
    }

    #[test]
    fn sha1_helpers() {
        assert!(is_sha1_hex(&sha1_hex(b"abc")));
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert!(!is_sha1_hex("ABC"));
    }
}
