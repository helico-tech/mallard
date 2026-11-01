# Sentry JavaScript source map protocol (upload + symbolication)

A reference for a self-hosted server that must work, unchanged, with:

- `sentry-cli sourcemaps upload` (Rust CLI, 2.x and 3.x), and
- the bundler plugins (`@sentry/vite-plugin`, `@sentry/webpack-plugin`, ...).

Everything below was read from source code on 2026-10-31. Each claim cites a file.
Items marked **UNVERIFIED** could not be confirmed from source.

## Sources and citation keys

| Key | Repository @ revision | What it is |
|---|---|---|
| `cli3` | `getsentry/sentry-cli` @ `master` `33a69573b0` (release 3.8.2) | Rust CLI 3.x |
| `cli2` | `getsentry/sentry-cli` @ tag `2.58.6` (last 2.x) | Rust CLI 2.x |
| `sentry` | `getsentry/sentry` @ `master` `063a6cb279` | Sentry server (Python) |
| `symbolicator` | `getsentry/symbolicator` @ `master` `dfc396f813` | JS symbolication (Rust) |
| `symbolic` | `getsentry/symbolic` @ `master` `7a0d1a7f07` | Bundle format (Rust) |
| `newcli` | `getsentry/cli` @ `main` `3567c3c937` (npm package `sentry` 0.45.x) | New TypeScript CLI |
| `sjs` | `getsentry/sentry-javascript` @ `develop` `f74a45dfa4` | JS SDK + `@sentry/bundler-plugins` 11.10.0 |
| `bp4` | `getsentry/sentry-javascript-bundler-plugins` @ tag `4.6.0` | Older bundler plugin core |

Paths are written as `key:path:line` (line numbers are approximate).

## 0. Summary for implementers

The minimal server surface for debug-ID based source map upload:

| # | Method + path | Used by | Required? |
|---|---|---|---|
| 1 | `GET /api/0/organizations/{org}/chunk-upload/` | all clients | **required** |
| 2 | `POST {url from #1}` (multipart chunks) | all clients | **required** |
| 3 | `POST /api/0/organizations/{org}/artifactbundle/assemble/` | all clients | **required** |
| 4 | `GET /api/0/organizations/{org}/` | `newcli` only (region lookup) | optional: a 404 makes it fall back to the base URL (see 1.5) |
| 5 | `POST /api/0/organizations/{org}/releases/`, `PUT /api/0/organizations/{org}/releases/{version}/`, `POST .../deploys/` | bundler plugin "release management" (create/finalize/deploy) | optional: failures are logged, the build goes on (see 1.6) |

Recommended `GET chunk-upload` response (works for every client checked):

```json
{
  "url": "https://sentry.example.com/api/0/organizations/acme/chunk-upload/",
  "chunkSize": 8388608,
  "chunksPerRequest": 64,
  "maxFileSize": 4294967296,
  "maxRequestSize": 33554432,
  "concurrency": 8,
  "hashAlgorithm": "sha1",
  "compression": ["gzip"],
  "accept": ["release_files", "artifact_bundles", "artifact_bundles_v2"]
}
```

Key rules:

- All JSON responses must have `Content-Type: application/json`. The Rust CLI refuses to parse a body without it (`cli3:src/api/mod.rs:1657` `is_json`).
- Return `url` as an **absolute** URL on the **same origin** as the API base URL (see 2.1).
- Advertise only `gzip` compression. Do not advertise `zstd` or `brotli` (see 2.2).
- The assemble response must **always** contain `missingChunks` (an array, may be empty). The Rust CLI fails to parse it otherwise (`cli3:src/api/data_types/chunking/artifact.rs:20` `missing_chunks: Vec<Digest>` has no default).
- When chunks are missing, assemble must answer `state: "not_found"` with the list. `newcli` stops without uploading if the first answer is `created` (see 3.4).
- A chunk must be visible to assemble as soon as its `POST` returned 2xx. After uploading, the Rust CLI treats `not_found` as a hard error (`cli3:src/api/data_types/chunking/file_state.rs:26` `is_err` includes `NotFound`).

---

## 1. HTTP requests made by `sentry-cli sourcemaps upload ./dist`

### 1.1 Auth header and common headers

Every request to the Sentry API carries (`cli3:src/api/mod.rs:1282-1352`):

```
Authorization: Bearer <token>
User-Agent: sentry-cli/<version>[ <SENTRY_PIPELINE value>]
Expect:
Content-Type: application/json        (only on JSON bodies)
```

- The token comes from `--auth-token`, `SENTRY_AUTH_TOKEN`, or `.sentryclirc` `[auth] token` (`cli3:src/config.rs:121-127`).
- The chunk upload goes to an absolute URL. `Api::request` adds no auth to absolute URLs, so `upload_chunks` forces the `Authorization` header on that request (`cli3:src/api/mod.rs:415-424`).
- Extra headers from `.sentryclirc` `[defaults] headers` are added to every request (`cli3:src/api/mod.rs:1292-1296`, `cli3:src/config.rs` `get_headers`).
- 2.x also supports a deprecated API key sent as HTTP Basic username (`cli2:src/api/mod.rs:1774-1781`). You can ignore it.
- `newcli` sends `Authorization: Bearer <token>` too, after trimming whitespace (`newcli:packages/cli/src/lib/auth-header.ts` `formatAuthHeader`; `newcli:packages/cli/src/lib/sentry-client.ts:159`).

**Base URL.** `--url`, `SENTRY_URL`, or `[defaults] url`, default `https://sentry.io/` (`cli3:src/config.rs:150-158`). Paths become `{base}/api/0/{path}` (`cli3:src/config.rs:330-337`). The base URL must be a bare origin with no path (`cli3:src/config.rs:308-318`: "bad sentry url: not on URL root").

**Gotcha: org auth tokens override the URL.** A token that starts with `sntrys_` is parsed as `sntrys_<base64 JSON payload>_<base64-no-pad 32-byte secret>` (exactly three `_`-separated parts). If the payload has a non-empty `url`, that URL **replaces** `--url`/`SENTRY_URL` (a warning is printed) (`cli3:src/config.rs:146-158`, `cli3:src/utils/auth_token/org_auth_token.rs`). The `org` in the payload also overrides `--org` (`cli3:src/config.rs:413-442`).
Payload fields: `org` (required), `url` (optional/null). In 2.x `region_url` is also required, or the token is treated as "unknown format" (`cli2:src/utils/auth_token/org_auth_token.rs:16-23`).
So if your server issues `sntrys_` tokens, embed **your** URL in them. A token in an unknown format is sent as-is in the Bearer header and does not change the URL (`cli3:src/utils/auth_token/auth_token_impl.rs:63-75`).
`newcli` also refuses to send a `sntrys_` token to a host other than its embedded `url` (`newcli:packages/cli/src/lib/sentry-client.ts:139-149`).

**Retries.** HTTP 502, 503 and 504 are retried with backoff, up to `max_retries` (default 5) (`cli3:src/api/mod.rs:68-72, 1448-1497`; `cli3:src/constants.rs:42`).

**Error bodies.** For 4xx/5xx the CLI shows `{"detail": "..."}` or `{"error": "..."}` (`cli3:src/api/mod.rs:1690-1695`, `1550-1590`).

### 1.2 Local checks before any request (3.x)

Order in `execute` (`cli3:src/commands/sourcemaps/upload.rs:413-447`):

1. Release: `--release`, `SENTRY_RELEASE`, or the hidden `--version` flag. Optional (`cli3:src/config.rs:445-475`).
2. Org: `--org`, `SENTRY_ORG`, the org in a `sntrys_` token, or `[defaults] org`. Required.
3. Projects: `--project` (can repeat), `SENTRY_PROJECT`, or `[defaults] project`. **Required** in 3.x and 2.58.x (`cli3:src/config.rs:482-512`). Older 2.x allowed none.
4. A token must exist, or it fails with `AuthMissing` (`cli3:src/api/mod.rs:128-137`).

There is **no** `GET /api/0/` auth check, no project lookup and no release creation in `sourcemaps upload` (3.x). The only API call before the files are processed is the chunk-upload options request.

If `--release` is not given and none of the files with a sourcemap reference has a debug ID, the CLI stops locally: "Cannot upload: You must either specify a release or have debug ids injected into your sources" (`cli3:src/utils/sourcemaps.rs:666-692`).

### 1.3 Request sequence: 3.x (and 2.x when all capabilities are advertised)

`sentry-cli sourcemaps upload ./dist --org acme --project web [--release 1.2.3 [--dist 42]]`

| Step | Request | Required response | Source |
|---|---|---|---|
| 1 | `GET /api/0/organizations/acme/chunk-upload/` | 200 JSON options (section 2). A 404 gives "organization not found" | `cli3:src/api/mod.rs:661-666` |
| 2 | (local) build artifact bundle zip, split into `chunkSize` pieces, SHA-1 each piece and the whole file | — | `cli3:src/utils/file_upload.rs:261-275`, `cli3:src/utils/fs.rs:148-160` |
| 3 | `POST /api/0/organizations/acme/artifactbundle/assemble/` with `{checksum, chunks, projects, version?, dist?}` | 200 JSON with `state` + `missingChunks`. Only `missingChunks` is used here | `cli3:src/utils/file_upload.rs:286-296` |
| 4 | 0..N × `POST <url>` multipart, only the chunks listed in `missingChunks`, batched, in parallel | any status below 400 except 301/302 | `cli3:src/utils/chunks/mod.rs:155-196`, `cli3:src/api/mod.rs:379-433` |
| 5 | `POST .../artifactbundle/assemble/` again (same body). Without `--wait`: exactly once. With `--wait`/`--wait-for`: every 1 s until `ok`/`error` or timeout | `created`/`assembling`/`ok` = success; `error`/`not_found` = failure | `cli3:src/utils/file_upload.rs:193-257` |

With `--release`/`--dist`, only the assemble body changes (`version`, `dist`). The CLI does **not** create the release. Sentry links the bundle to the release *name* even if no release object exists (`sentry:src/sentry/api/endpoints/organization_artifactbundle_assemble.py:229-233` comment; `sentry:src/sentry/tasks/assemble.py` `_create_artifact_bundle`).

**Outcome of step 5** (`cli3:src/utils/file_upload.rs:236-257`):

- `error` or `not_found` → exit with "Failed to process uploaded files: {detail or 'unknown error'}".
- `created` or `assembling` without `--wait` → success, prints "File upload complete (processing pending on server)".
- still pending with `--wait` after the timeout → failure "Failed to process files in Ns".
- `ok` → "File processing complete".

**Wait timeout** = min(`--wait-for` or 300 s, server `maxWait` or 300 s if 0/absent) (`cli3:src/utils/file_upload.rs:200-206`, `cli3:src/constants.rs:49`).

If all chunks are already on the server (`missingChunks: []`), step 4 is skipped and it prints "Nothing to upload, all files are on the server" (`cli3:src/utils/file_upload.rs:298-306`).

### 1.4 Capabilities (`accept`) and the 2.x fallbacks

**3.x.** `ChunkServerOptions` in 3.x has no `accept` field. It always uses the artifact-bundle path (`cli3:src/api/data_types/chunking/upload/options.rs`). Sentry notes that the CLI ignores `accept` "starting with 3.1.0" (`sentry:src/sentry/api/endpoints/chunk.py:42-46`). **3.0.x still reads `accept`** and behaves like 2.x (`sentry-cli@3.0.0:src/utils/file_upload.rs:43,172,297,386`).

**2.x** (`cli2:src/utils/file_upload.rs`, `cli2:src/api/data_types/chunking/upload/options.rs`):

- If `accept` is missing it defaults to `["debug_files"]` (`options.rs` `default_chunk_upload_accept`).
- `FileUpload::upload` (`cli2:src/utils/file_upload.rs:370-420`):
  1. `initialize_legacy_release_upload`: if `accept` has `artifact_bundles` **or** `artifact_bundles_v2` and a project is set → do nothing. Otherwise: with a release → create it (`POST /api/0/projects/{org}/{project}/releases/` for a single project, else `POST /api/0/organizations/{org}/releases/`) (`cli2:src/api/mod.rs:684-700`). Without a release → error.
  2. If `accept` has `release_files` → chunked upload (the flow in 1.3).
  3. Otherwise → **legacy one-request-per-file upload** (see section 6) with a deprecation warning.
- Inside the chunked upload (`cli2:src/utils/file_upload.rs:622-680`, `528-581`):
  - The step-3 pre-check (first assemble call to get `missingChunks`) runs **only** if `accept` has `artifact_bundles_v2`. Without it, **all** chunks are uploaded every time.
  - Assemble goes to `/artifactbundle/assemble/` if `accept` has `artifact_bundles` or `artifact_bundles_v2`. Otherwise it goes to the legacy `POST /api/0/organizations/{org}/releases/{version}/assemble/` (needs a release).
- If `GET chunk-upload` returns 404, 2.x treats chunked upload as unsupported (`cli2:src/api/mod.rs:961-976`). With a release it falls back to the legacy per-file path. Without one: "The provided organization ... does not exist ...".

**Minimal `accept` for the debug-ID artifact-bundle path in every 2.x version:** `["release_files", "artifact_bundles_v2"]`. Also add `"artifact_bundles"`: it costs nothing and covers any client that checks only that value. Missing `release_files` → legacy per-file upload. Missing both bundle values → legacy release-bundle assemble.

Unknown `accept` strings are ignored (`cli2:src/api/data_types/chunking/upload/capability.rs:45-68`). Sentry itself advertises 12 values (`sentry:src/sentry/api/endpoints/chunk.py:47-60`). Only the three above matter for source maps.

`--use-artifact-bundle` / `SENTRY_FORCE_ARTIFACT_BUNDLES=1` (2.x, deprecated) adds `artifact_bundles` on the client side (`cli2:src/commands/sourcemaps/upload.rs:434-446`).

### 1.5 Bundler plugins

There are two generations:

**A. `@sentry/bundler-plugin-core` ≤ 4.x** (old repo; checked at 4.6.0). It drives the Rust CLI `@sentry/cli ^2.57.0` through its JS wrapper (`bp4:packages/bundler-plugin-core/package.json`; `bp4:packages/bundler-plugin-core/src/build-plugin-manager.ts`):

- `sentry-cli sourcemaps inject <paths>` (local only) (`build-plugin-manager.ts:555`).
- `sentry-cli sourcemaps upload --release <name or "undefined"> -p <project> ...` (`build-plugin-manager.ts:630`; `cli2:lib/releases/index.js:190-192`). The HTTP traffic is exactly 1.3/1.4.
- Release management when a release name is known: `releases new` (`POST .../releases/`), `releases set-commits --auto`, `releases finalize` (`PUT /api/0/organizations/{org}/releases/{version}/` with `{"dateReleased": ...}`), `releases deploys new` (`POST /api/0/organizations/{org}/releases/{version}/deploys/`) (`build-plugin-manager.ts:474-531`; `cli3:src/api/mod.rs:470-499, 607-622`).

**B. `@sentry/bundler-plugins` 11.x** (now in the `sentry-javascript` monorepo; `@sentry/vite-plugin` on the old repo's `main` just re-exports `@sentry/bundler-plugins/vite`). It no longer uses the Rust CLI. It uses the **new TypeScript CLI**, npm package `sentry` `^0.45.0`, through `createSentrySDK()` (`sjs:packages/bundler-plugins/package.json:123`; `sjs:packages/bundler-plugins/src/core/cli.ts`). Its upload flow (`newcli:packages/cli/src/lib/api/sourcemaps.ts`, `.../chunk-upload.ts`, `.../region.ts`):

| Step | Request | Notes |
|---|---|---|
| 1 | `GET /api/0/organizations/{org}/` | Region lookup: reads `links.regionUrl`. Non-auth errors (e.g. 404) → falls back to the base URL. Auth errors are rethrown (`region.ts` `resolveOrgRegionUncached`, `errors.ts` `withAuthGuard`). The result is cached locally. |
| 2 | `GET {region}/api/0/organizations/{org}/chunk-upload/` | Validated with valibot: `url`, `chunkSize`, `chunksPerRequest`, `maxRequestSize`, `hashAlgorithm`, `concurrency`, `compression` **required**; `maxFileSize`, `maxWait` optional (`chunk-upload.ts` `ChunkServerOptionsSchema`). |
| 3 | `POST .../artifactbundle/assemble/` `{checksum, chunks, projects:[project], version?, dist?}` | `error` → fail. `ok` → done. `created` and not waiting → **done, no chunk upload**. Otherwise go on. |
| 4 | `POST {url}` one chunk per request, `concurrency` in parallel | Ignores `chunksPerRequest`. Field `file_gzip` if gzip was chosen; `file` + header `Content-Encoding: zstd` if zstd was chosen (`chunk-upload.ts` `uploadChunk`). |
| 5 | `POST .../artifactbundle/assemble/` every 1 s | Returns on `created` (or `ok` when waiting). Throws on `error`. 300 s deadline (`chunk-upload.ts` `pollAssembly`). |

- The plugin always passes `release: options.release.name ?? 'undefined'` (`sjs:packages/bundler-plugins/src/core/build-plugin-manager.ts:690-697, 792-798`), so bundles can arrive with `version: "undefined"`. Sentry may drop such placeholder releases (behind an option flag) (`sentry:src/sentry/tasks/assemble.py:412-428, 642-695`).
- `newcli` has a host-scoping guard: a request whose origin differs from the token's host throws `HostScopeError` (`newcli:packages/cli/src/lib/sentry-client.ts:131-137`, `.../token-host.ts:126-134`). So the chunk-upload `url` must be on the same origin as the configured `SENTRY_URL`/`SENTRY_HOST`.
- `newcli` calls `fetch(serverOptions.url)` directly. **UNVERIFIED** whether a relative `url` works there; an absolute URL is safe.
- **UNVERIFIED:** which `@sentry/vite-plugin`/`@sentry/webpack-plugin` version first switched from generation A to B. Support both: the wire protocol is the same, apart from the details marked above.
- **UNVERIFIED:** the exact response schema `@sentry/api`'s `getOrganization` expects for step 1. Returning 404 is the documented fallback path. Returning `{"slug": "acme", "links": {"organizationUrl": "<base>", "regionUrl": "<base>"}}` should also work.

### 1.6 Release management errors do not fail the build

In both plugin generations, an error in release create/set-commits/finalize/deploy goes to `handleRecoverableError(e, false)`. It is logged, not thrown, unless the user set `errorHandler` (`sjs:packages/bundler-plugins/src/core/build-plugin-manager.ts:307-332, 590-594`). The debug-ID upload is a separate hook and runs anyway. You can skip the release endpoints for a first version. Users then see "An error occurred. Couldn't finish all operations" in build logs.

Request/response shapes if you implement them (Rust CLI 3.x):

- `POST /api/0/organizations/{org}/releases/` body `{"version": "1.2.3", "projects": ["web"], "url"?, "dateStarted"?, "dateReleased"?}`. Numeric project strings are sent as JSON numbers (`cli3:src/api/mod.rs:1720-1730`, `cli3:src/api/serialization.rs`).
- Response `ReleaseInfo`: `version` and `dateCreated` (RFC 3339) are required. `url`, `dateReleased`, `lastEvent`, `newGroups`, `projects`, `lastCommit` are optional (`cli3:src/api/mod.rs:1771-1791`).
- `PUT /api/0/organizations/{org}/releases/{version}/` with `UpdatedRelease` fields (`dateReleased`, `refs`, `commits`, `status`, ...) (`cli3:src/api/mod.rs:1752-1767`).
- **UNVERIFIED:** the exact endpoints the new `sentry` CLI uses for `release create/finalize/deploy`. Not read.

---

## 2. Chunk upload endpoint

### 2.1 `GET /api/0/organizations/{org}/chunk-upload/`

Sentry's response (`sentry:src/sentry/api/endpoints/chunk.py:186-268`):

| Field | Sentry value | Rust 3.x | Rust 2.x | newcli | Meaning |
|---|---|---|---|---|---|
| `url` | absolute upload URL (see below) | required | required | required | Where chunks are POSTed |
| `chunkSize` | `8388608` (8 MiB, `SENTRY_CHUNK_UPLOAD_BLOB_SIZE`) | required, non-zero | required, non-zero | required | Bytes per chunk. Keep it a power of two (CLI ≤ 2.39.1 requires that, `chunk.py:252-256`) |
| `chunksPerRequest` | `64` | required | required | required (unused) | Max parts per POST |
| `maxRequestSize` | `33554432` (32 MiB) | required | required | required (unused) | Max uncompressed bytes per POST |
| `maxFileSize` | `4294967296` (2^32) | optional | optional | optional | Not used for source maps |
| `maxWait` | (not sent) | optional, seconds | optional | optional | Caps `--wait` |
| `concurrency` | `8` (`1` in DEBUG) | required, `u8` | required, `u8` | required | Parallel POSTs |
| `hashAlgorithm` | `"sha1"` | required, must be `"sha1"` | same | required (any string) | Chunk checksum algorithm |
| `compression` | `["gzip","zstd"]` | optional (default `[]`) | optional | **required** array | Accepted chunk codecs |
| `accept` | 12 capability strings | ignored | optional, default `["debug_files"]` | ignored | See 1.4 |

Sources: `cli3:src/api/data_types/chunking/upload/options.rs`, `cli2:src/api/data_types/chunking/upload/options.rs`, `cli3:src/api/data_types/chunking/hash_algorithm.rs` (an enum with only `Sha1`, so any other value fails to deserialize), `newcli:packages/cli/src/lib/api/chunk-upload.ts` `ChunkServerOptionsSchema`.

**`url` rules.** Sentry returns a relative, version-less path (`organizations/{org}/chunk-upload/`) only to `sentry-cli` 1.70.1 ≤ version < 2.30.0, and an absolute URL to everyone else (`chunk.py:206-240`).
The Rust CLI accepts both: a relative path gets `/api/0/` added (`cli3:src/config.rs:330-337`). `newcli` passes the value straight to `fetch`.
→ **Always return an absolute URL** on the API's own origin.

### 2.2 `POST {url}`: multipart chunk upload

**Body.** `multipart/form-data`. One part per chunk (`cli3:src/api/mod.rs:379-405`):

```
--BOUNDARY
Content-Disposition: form-data; name="file_gzip"; filename="3b5d5c3712955042212316173ccf37be80000000"
Content-Type: application/octet-stream

<gzip(chunk bytes)>
--BOUNDARY
Content-Disposition: form-data; name="file_gzip"; filename="ad0234829205b9033196ba818f7a872b00000000"
...
--BOUNDARY--
```

- **Part name** = the codec: `file` (uncompressed), `file_gzip` (gzip with header) (`cli3:src/api/data_types/chunking/compression.rs`). 2.x can also send `file_brotli` if the server lists `brotli` (`cli2:src/api/data_types/chunking/compression.rs`). Sentry does not accept that.
- **Part filename** = the chunk's checksum: SHA-1 of the **uncompressed** chunk, as 40 lowercase hex characters. The checksums are computed on the raw zip bytes before compression (`cli3:src/utils/fs.rs:148-160`, `cli3:src/utils/file_upload.rs:269-275`, `cli3:src/api/mod.rs:391-404`).
- **Part content type**: curl's default for buffer parts (`application/octet-stream`). `newcli` sets `application/octet-stream` explicitly.
- **Codec choice**:
  - Rust CLI: the highest of the advertised codecs it knows. 3.x knows `gzip`(10) > `uncompressed`(0); unknown strings such as `zstd` map to `uncompressed` (`cli3:src/utils/chunks/mod.rs:166-172`, `compression.rs` `#[serde(other)]`). 2.x adds `brotli`(20).
  - `newcli`: prefers `zstd`, then `gzip` (`chunk-upload.ts` `pickUploadEncoding`). With `zstd` it sends field `file` plus a **request** header `Content-Encoding: zstd`. That header means "each part is zstd", not "the HTTP body is zstd" (`sentry:src/sentry/api/endpoints/chunk.py:296-305, 375-385`). Many HTTP stacks would try to decode the whole body.
  - → **Advertise `["gzip"]` only.** Every client then uses the `file_gzip` field with no `Content-Encoding` header.
- **Batching (Rust)**: chunks are grouped so each request has ≤ `chunksPerRequest` parts and ≤ `maxRequestSize` *uncompressed* bytes (the first chunk always fits). Batches run on a thread pool with `concurrency` threads (`cli3:src/utils/chunks/mod.rs:73-96, 174-193`). `newcli` sends one chunk per request (`chunk-upload.ts` `uploadMissingChunks`).

**Server-side validation in Sentry** (`chunk.py:294-366`, `sentry:src/sentry/models/files/abstractfileblob.py:136-150`):

- More than 64 parts → 400 `{"error": "Too many chunks"}`.
- Zero parts → 200.
- A decompressed chunk larger than `chunkSize` → 400 `{"error": "Chunk size too large"}`.
- Total over 32 MiB → 400 `{"error": "Request too large"}`.
- Bad gzip → 400 `{"error": "Invalid gzip payload"}`.
- SHA-1 of the (decompressed) content ≠ filename → 400 `{"error": "Checksum mismatch"}`.
- Duplicates are deduplicated. Chunks are stored content-addressed and linked to the org ("blob owner").

**Response.** Sentry returns `200` with an empty body. The Rust CLI ignores the body. It fails on ≥ 400 and on 301/302 (reported as "project not found") (`cli3:src/api/mod.rs:427-433`). `newcli` needs `response.ok`.

Blob lifetime in Sentry: `find_missing_chunks` refreshes the timestamp of blobs older than 12 h. Old blobs are deleted after about a day (`sentry:src/sentry/debug_files/upload.py`; `sentry:src/sentry/models/files/utils.py:20-22, 126`). Clients rely only on `missingChunks`, so the retention time is up to you.

---

## 3. Assemble endpoint: `POST /api/0/organizations/{org}/artifactbundle/assemble/`

### 3.1 Request

Sent by the Rust CLI as `ChunkedArtifactRequest` (`cli3:src/api/data_types/chunking/artifact.rs`):

```json
{
  "checksum": "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12",
  "chunks": [
    "3b5d5c3712955042212316173ccf37be80000000",
    "ad0234829205b9033196ba818f7a872b00000000"
  ],
  "projects": ["web"],
  "version": "1.2.3",
  "dist": "42"
}
```

- `checksum`: SHA-1 of the **whole bundle file** = SHA-1 of all chunks joined in order (`cli3:src/utils/fs.rs:148-160`: one hasher fed with every chunk).
- `chunks`: ordered SHA-1 list of the chunks. The order defines how the file is rebuilt (`sentry:src/sentry/tasks/assemble.py:140-143`).
- `projects`: slugs or numeric IDs. The Rust CLI sends plain strings; it omits the field only when empty (2.x).
- `version`: omitted if missing **or empty** (`version_is_empty`). `dist`: omitted if missing.

Sentry's validation (`sentry:src/sentry/api/endpoints/organization_artifactbundle_assemble.py:36-67, 136-207`):

- JSON schema: required `checksum` (`^[0-9a-f]{40}$`), `chunks` (array of the same pattern), `projects` (array of strings). `version` must match `^[^/]+$`. `dist` is a string. No other properties.
- Errors → `400 {"error": "<message>"}`, e.g. `"You need to specify at least one project"`, `"One or more projects are invalid"` (every listed project must exist and be active), `"You need to specify a release together with a dist"`.
- A missing release permission → 404.

### 3.2 Response states

`ChunkFileState` (`sentry:src/sentry/tasks/assemble.py:58-63`; `cli3:src/api/data_types/chunking/file_state.rs`; `newcli` `AssembleResponseSchema`):

| `state` | Meaning | Example |
|---|---|---|
| `not_found` | Some chunks are missing, or unknown checksum polled with `chunks: []` | `{"state":"not_found","missingChunks":["3b5d...0000"]}` |
| `created` | Job accepted and queued | `{"state":"created","missingChunks":[]}` |
| `assembling` | Worker is running | `{"state":"assembling","detail":null,"missingChunks":[]}` |
| `ok` | Bundle stored and indexed | `{"state":"ok","detail":null,"missingChunks":[]}` |
| `error` | Failed; `detail` has the reason | `{"state":"error","detail":"uploading a bundle without debug ids or release is prohibited","missingChunks":[]}` |

The response object has `state` (string), `missingChunks` (array of SHA-1 strings) and `detail` (string or null, optional).

### 3.3 Sentry's server logic (the reference behaviour)

From `organization_artifactbundle_assemble.py:189-253`:

1. Find which of `chunks` this org does not have (`find_missing_chunks`). If any are missing → `{"state":"not_found","missingChunks":[...]}`. This happens **before** the state cache is read.
2. Look up the cached state for key `(org, checksum)` (Redis, 600 s TTL, `assemble.py:252-279`). `ok` → `{"state":"ok","detail":null,"missingChunks":[]}`. Any other cached state → return it with its `detail`. This makes repeated POSTs idempotent "polls".
3. No cached state and `chunks == []` → `{"state":"not_found","missingChunks":[]}`.
4. `dist` without `version` → 400.
5. Set state `created`, queue `assemble_artifacts(org_id, project_ids, version, dist, checksum, chunks)`, return `{"state":"created","missingChunks":[]}`.

The worker (`assemble.py:849-920`):

1. Sets `assembling`.
2. Joins the blobs in `chunks` order and checks the SHA-1. On mismatch → `error` "Reported checksum mismatch". If chunks are missing → `error` "Not all chunks available for assembling". If the total is over 4 GiB → `error` (`assemble.py:90-177`).
3. No projects → `error`.
4. Opens the zip and validates it (section 4.4). Then `ok`. Any other exception → `error` "internal server error".

### 3.4 How each client polls

| Client | First call | After uploading chunks | Poll interval / limit |
|---|---|---|---|
| Rust 3.x / 2.x with `artifact_bundles_v2` | uses only `missingChunks` | one more call (no `--wait`), or a loop (`--wait`) | 1 s, `min(--wait-for or 300 s, maxWait or 300 s)` (`cli3:src/utils/chunks/mod.rs:23`, `cli3:src/utils/file_upload.rs:193-234`) |
| Rust 2.x with only `artifact_bundles` | none (uploads all chunks) | same as above | same |
| newcli | `error` → fail, `ok` → done, `created` (no wait) → **done without uploading** | sleep 1 s, POST, repeat until `created`/`ok` | 1 s, 300 s (`newcli:.../chunk-upload.ts` `pollAssembly`, `.../sourcemaps.ts` `uploadArtifactBundle`) |

Consequences for your server:

- Never answer `created` while chunks are still missing. Use `not_found` + `missingChunks`.
- After chunks are uploaded, the next assemble POST must not return `not_found` (Rust fails on it).
- Make assemble idempotent per `(org, checksum)`. Both clients repeat the identical body to poll.
- Answering `ok` synchronously (assembling inside the request) is valid for all clients.

---

## 4. Artifact bundle zip format

### 4.1 Container

- The file starts with an 8-byte header **before** the zip data: ASCII `SYSB` plus a u32 version `2` (`symbolic:symbolic-debuginfo/src/sourcebundle/mod.rs:73-76, 525-552, 1210-1222`).
  - The Rust writer writes a packed struct in native byte order (little-endian on x86/ARM). `newcli` writes it explicitly little-endian (`newcli:packages/cli/src/lib/sourcemap/zip.ts:131-138`).
- Zip readers must accept data before the zip.
  - Python `zipfile` (used by Sentry, `sentry:src/sentry/models/artifactbundle.py:215-217`) and the Rust `zip` crate (used by symbolic `SourceBundle::parse` on the full buffer including `SYSB`, `symbolic:.../sourcebundle/mod.rs` `parse_with_opts`) both do.
  - **UNVERIFIED** for other zip libraries. Test with a real bundle.
- Entry compression: Stored or Deflated (`cli3:src/utils/source_bundle.rs:309-330` canary test; `newcli` uses `stored` when a wire codec is used, else `deflate`). Timestamps are fixed to 1980-01-01 (`symbolic:.../sourcebundle/mod.rs:1195-1203`).

### 4.2 `manifest.json` (zip entry at the root)

Written by symbolic `SourceBundleWriter` for the Rust CLI (`symbolic:.../sourcebundle/mod.rs:206-224, 556-570, 1571-1580`; `cli3:src/utils/source_bundle.rs:56-145`):

```json
{
  "files": {
    "files/_/_/assets/index-B2x9.js": {
      "type": "minified_source",
      "url": "~/assets/index-B2x9.js",
      "headers": {
        "debug-id": "c3a7e7a0-4b9b-4c3f-9a1e-0d9f0c7f3e21",
        "sourcemap": "index-B2x9.js.map"
      }
    },
    "files/_/_/assets/index-B2x9.js.map": {
      "type": "source_map",
      "url": "~/assets/index-B2x9.js.map",
      "headers": {
        "debug-id": "c3a7e7a0-4b9b-4c3f-9a1e-0d9f0c7f3e21"
      }
    }
  },
  "debug_id": "5f1b0c2e-8d7a-5b3c-9e4f-a1b2c3d4e5f6",
  "org": "acme",
  "project": "web",
  "release": "1.2.3",
  "dist": "42",
  "note": "optional --note text"
}
```

- `files`: a map from **zip entry path** to file info. The key is the real path inside the zip. Always read entries by key, never derive paths from URLs.
- File info fields, all optional:
  - `type`: one of `source`, `minified_source`, `source_map`, `indexed_ram_bundle` (`symbolic:.../sourcebundle/mod.rs:172-198`; `sentry:src/sentry/models/artifactbundle.py:34-53`).
  - `url`: the lookup URL, usually `~/<path relative to the upload dir>`.
  - `path`: native source path; not set for JS.
  - `headers`: string → string.
- Known headers: `debug-id` and `sourcemap` (legacy `x-sourcemap`). The writer lowercases keys (`add_header`). Readers must lowercase them too: Sentry and symbolic do (`sentry:.../artifactbundle.py:252-253`; `symbolic:.../sourcebundle/mod.rs:226-245`). `newcli` writes `Sourcemap` with a capital S (`newcli:packages/cli/src/lib/api/sourcemaps.ts` `buildArtifactBundle`).
- `sourcemap` header value: the minified file's `//# sourceMappingURL=` value as written (usually relative, e.g. `index.js.map`), or a guessed basename (`cli3:src/utils/sourcemaps.rs:286-327, 1067-1100`). It is set only on `minified_source` files (`cli3:src/utils/sourcemaps.rs:591-605`). It is resolved relative to the file's URL at symbolication time.
- `debug-id` header: set on a JS file when it has a `//# debugId=<uuid>` comment. Set on a map when it has a top-level `"debug_id"` or `"debugId"` field (`debug_id` wins) (`cli3:src/utils/file_upload.rs:72-130`; `symbolic:symbolic-debuginfo/src/js.rs` `discover_debug_id`, `discover_sourcemap_embedded_debug_id`). The JS file and its map share the same debug ID.
- Top-level attributes are flattened string fields (`#[serde(flatten)] attributes`):
  - `debug_id`: bundle ID. The Rust CLI uses a UUID derived from SHA-1 of all files (`cli3:src/utils/source_bundle.rs:147-170`).
  - `org`.
  - `project`: only if exactly one project.
  - `release`, `dist`, `note`: if given.
  - `source_links` (native only) may also appear.
  - `newcli` writes no `debug_id`. Sentry then derives the bundle ID from the checksum (`sentry:src/sentry/tasks/assemble.py:526-537`).

### 4.3 Entry paths inside the zip

Rust CLI: `files/` + `url_to_bundle_path(url)` (`cli3:src/utils/source_bundle.rs:172-196`, `symbolic:.../sourcebundle/mod.rs:1543-1546`):

| `url` | zip entry |
|---|---|
| `~/assets/app.js` | `files/_/_/assets/app.js` |
| `~/#/bar` | `files/_/_/#/bar` |
| `https://cdn.example.com/a.js` | `files/https/cdn.example.com/a.js` |
| `app:///main.js` | `files/app/_/main.js` |

Duplicate paths get `.1`, `.2`, ... added (`symbolic:.../sourcebundle/mod.rs:1548-1568`).

`newcli` uses no `files/` prefix: `~/x` → `_/_/x`, anything else → `_/<url>` (`newcli:.../sourcemaps.ts` `urlToBundlePath`).

Default URL prefix is `~` (`--url-prefix`, `--url-suffix` change it) (`cli3:src/commands/sourcemaps/upload.rs:340-372`). Files that are not valid UTF-8 are skipped (`cli3:src/utils/source_bundle.rs:115-125`).

### 4.4 What Sentry checks and stores after assembly

From `sentry:src/sentry/tasks/assemble.py:430-846` and `sentry:src/sentry/models/artifactbundle.py`:

- The zip must open and contain `manifest.json`. Otherwise `error` "the bundle is invalid".
- Each entry must be ≤ 1 GiB uncompressed (`MAX_SOURCE_FILE_SIZE`).
- A bundle with 0 files in `files` is dropped silently (state still `ok`).
- `release`/`dist` from the request win over the manifest's `release`/`dist`.
- Placeholder releases (`undefined`, `null`, empty, unexpanded `$ENV_VAR` names) may be dropped behind option flags, but only if every file has a debug ID.
- No debug IDs **and** no release → `error` "uploading a bundle without debug ids or release is prohibited".
- Bundle ID = manifest `debug_id` (normalized, cut to 36 chars) or derived from the checksum. Uploading the same bundle ID again replaces the stored file.
- Index rows created:
  - bundle ↔ each project.
  - bundle ↔ `(release_name, dist_name or "")` if there is a release.
  - `(debug_id, source_file_type)` → bundle, for every file with a valid `debug-id` header and a known `type`.
  - A URL index per release is built only when a release has ≥ 3 bundles (`INDEXING_THRESHOLD`, `sentry:src/sentry/debug_files/artifact_bundles.py:40`).

---

## 5. How an event uses the uploaded files

In Sentry this runs in Symbolicator. Sentry sends the frames and modules, and Symbolicator fetches bundles back through Sentry's `artifact-lookup` API. A self-hosted server re-implements this logic directly.

### 5.1 Event input

The JS SDK finds debug IDs at runtime. The injected snippet stores `globalThis._sentryDebugIds[<Error().stack>] = "<debug-id>"` (`newcli:packages/cli/src/lib/sourcemap/debug-id.ts:118-120`). The SDK parses each stack key to a filename, sets `frame.debug_id`, then moves those IDs into `debug_meta.images` keyed by `abs_path` (or `filename`) (`sjs:packages/core/src/utils/debug-ids.ts` `getFilenameToDebugIdMap`; `sjs:packages/core/src/utils/prepareEvent.ts:196-244` `applyDebugIds`, `applyDebugMeta`).

```json
{
  "platform": "javascript",
  "release": "1.2.3",
  "dist": "42",
  "exception": {
    "values": [{
      "type": "TypeError",
      "value": "Cannot read properties of undefined (reading 'x')",
      "stacktrace": {
        "frames": [
          {
            "filename": "https://app.example.com/assets/index-B2x9.js",
            "abs_path": "https://app.example.com/assets/index-B2x9.js",
            "function": "Kt",
            "lineno": 1,
            "colno": 48213,
            "in_app": true
          }
        ]
      }
    }]
  },
  "debug_meta": {
    "images": [
      {
        "type": "sourcemap",
        "code_file": "https://app.example.com/assets/index-B2x9.js",
        "debug_id": "c3a7e7a0-4b9b-4c3f-9a1e-0d9f0c7f3e21"
      }
    ]
  }
}
```

- Frames are in caller-first order (oldest first). Sentry sends them to Symbolicator as `caller_first` (`sentry:src/sentry/lang/javascript/processing.py` `process_js_stacktraces`).
- An image counts only if `type == "sourcemap"` and both `debug_id` and `code_file` are set (`processing.py` `is_sourcemap_image`).
- **Line/column convention:** event `lineno`/`colno` are **1-based**. Source map mappings and symbolic's `SourcePosition` are **0-based**. Symbolicator converts both ways (5.4).

### 5.2 Which frames are processed

`processing.py` `_handles_frame`. A frame is processed only if:

- the frame's `platform` (or the event's) is `javascript` or `node` (`sentry:src/sentry/lang/javascript/utils.py:17`);
- `abs_path` and `lineno` are present;
- `abs_path` is not `native` or `[native code]`;
- it is not a node builtin: platform `node` with `abs_path` not matching `^((app|webpack|[a-zA-Z]|https?):|/)`.

Only `platform, abs_path, lineno, colno, function` are sent on (`_normalize_frame`). A frame with `lineno < 1` gets an `invalid_location` error (`symbolicator:crates/symbolicator-js/src/symbolication.rs` `symbolicate_js_frame`). A leading `async ` is removed from `abs_path` (`symbolicator:crates/symbolicator-js/src/lookup.rs:196-214`).

### 5.3 Matching a frame to files

From `symbolicator:crates/symbolicator-js/src/lookup.rs` and `utils.rs`:

1. **Module / debug ID.** Find the image whose `code_file` is **exactly equal** to the frame's `abs_path` (`lookup.rs:150-159, 216-251`). Its `debug_id` (if any) is the frame's debug ID.
2. **Minified file** (key: `abs_path` + optional `debug_id`, type `minified_source`) (`lookup.rs:559-626, 726-765, 1013-1068`).
   - With a debug ID:
     1. Look in bundles already downloaded for `(debug_id, minified_source)`.
     2. Ask Sentry's lookup API with that debug ID (and the file stem + release/dist), download the bundles, retry by debug ID.
     3. Only then look in bundles by URL.
     4. Then legacy individual release files.
     5. Then web scraping, if enabled.
   - Without a debug ID:
     1. Look in bundles by URL.
     2. Then individual release files.
     3. Then the lookup API (needs a release).
     4. Retry.
     5. Then scraping.
3. **Source map reference.** It comes from the bundle header `sourcemap`/`x-sourcemap`, else from the last `//# sourceMappingURL=` (or `//@`) comment in the minified file (`symbolic:.../sourcebundle/mod.rs:463-481`, `symbolic-debuginfo/src/js.rs`). It is resolved against the minified file's `abs_path` with `join_paths` (`lookup.rs:461-488, 300-317`; `utils.rs:187-247`). A `data:` URL means an embedded map.
4. **Debug ID fallback.** The debug ID is the image's, else a `//# debugId=` comment found in the minified file (`lookup.rs:628-637`).
5. **No map needed.** If there is neither a map reference nor a debug ID, the frame is returned unchanged (not an error) (`lookup.rs:639-643`; `symbolication.rs` `None => return Ok(raw_frame.clone())`).
6. **Source map file** (type `source_map`). The key is (resolved map URL, debug_id) or (debug_id only). It uses the same "debug ID first, URL second" order (`lookup.rs:645-676`).

**URL candidates** for any "by URL" lookup. From `get_release_file_candidate_urls`: drop the `#fragment`, then try in order (`utils.rs:303-322`):

```
https://app.example.com/assets/index.js?v=3   (absolute)
https://app.example.com/assets/index.js       (absolute, no query)
~/assets/index.js?v=3                         (host removed, "~" prefix)
~/assets/index.js                             (host removed, no query)
```

These are compared with each manifest file's `url` (exact match, `symbolic:.../sourcebundle/mod.rs:588-606, 1024-1029`). Bundles are tried newest first (`lookup.rs:968-1003`).

**Which bundles are candidates** (server side, `sentry:src/sentry/api/endpoints/artifact_lookup.py` `get`, `sentry:src/sentry/debug_files/artifact_bundles.py:296-381, 505-535, 670-730`). The lookup request carries `debug_id` (repeated), `release`, `dist`, and `url` = file stem, e.g. `/assets/index` with host, query and suffixes `.min .js .map .cjs .mjs .ts .d .jsx .tsx` removed (`symbolicator:.../utils.rs:260-287`; `symbolicator:.../api_lookup.rs:103-130`).

1. If `debug_id` is given → the **single newest** bundle of this project that contains that debug ID (`resolved_with: "debug-id"`).
2. Otherwise, with a release:
   - the bundles of `(release, dist or "")` whose URL index contains the stem (case-insensitive substring match), up to 5 (`"index"`);
   - plus, while the release's URL index is incomplete (< 3 bundles or not all indexed), the 5 most recent bundles of that release/dist (`"release"`).
3. If no bundle was found → legacy `ReleaseFile` artifacts (`"release-old"`).

All queries are limited to bundles linked to the event's project.

### 5.4 Rewriting the frame

From `symbolicator:crates/symbolicator-js/src/symbolication.rs` `symbolicate_js_frame`:

- **Lookup position** = `SourcePosition::new(lineno - 1, colno.saturating_sub(1))` (0-based). If no token is found → `invalid_location` error and the frame stays raw.
- **`function`**:
  1. Take the token's scope name from the SourceMapCache: a named scope, `<anonymous>`, or, if unknown, the minified function name.
  2. Then `get_function_for_token`. Use that name unless it is in `["<anonymous>", "__webpack_require__", "__webpack_modules__"]`. Else use the *caller* frame's token name ("caller naming"; it works because frames are walked caller-first). Else use the minified name.
  3. Then `fold_function_name` collapses repeated dotted parts (`utils.rs:16-97`).
- **`abs_path`** = `join_paths(base, token.file_name)`. `base` is the remote map URL, else the minified `abs_path` (`lookup.rs:236-246`).
- **`filename`** = the token's file name. `webpack:`/`turbopack:` names are cleaned up, e.g. `webpack://app/./src/a.js` → `./src/a.js` and `.../~/x` → `~/x` (`utils.rs:99-130`).
- **`module`** is generated for `webpack:`, `turbopack:`, `http(s):` and `app:` paths (`utils.rs:155-185`).
- **`lineno`** = `token.line + 1`, **`colno`** = `token.column + 1` (back to 1-based).
- **Context.** `pre_context`, `context_line` and `post_context` come from the map's `sourcesContent`. If that is missing, the original source file is fetched with key `join_paths(base, file)`. Otherwise the error `missing_source_content` is set.
  - 5 lines before and after; lines over 150 characters are cut to about 140 around the column (`symbolicator:crates/symbolicator-service/src/source_context.rs:1-50`).
  - Context that came with the minified frame is cleared on the processed frame.
- **`data`**:
  - `sourcemap`: the map URL label (remote map URL, else the frame's `abs_path`).
  - `sourcemap_origin`: where the map came from.
  - `resolved_with`: one of `debug-id`, `url`, `index`, `release`, `release-old`, `scraping`, `unknown`.
  - `symbolicated: true` (`symbolicator:crates/symbolicator-js/src/interface.rs:257-292`).
- **Sentry merge** (`sentry:src/sentry/lang/javascript/processing.py` `_merge_frame`):
  - copies the non-empty fields `function, abs_path, filename, lineno, colno, pre_context, context_line, post_context, module`, plus `data.sourcemap`, `data.sourcemap_origin`, `data.resolved_with`, `data.symbolicated`, onto the original frame;
  - sets `in_app` (`webpack:` paths: not `~/` and not `/node_modules/`; `app:` paths: not `node_modules/`; any `/node_modules/` → false);
  - sets `event.symbolicated_in_app`.
- **Raw stack trace.** Next to the processed `stacktrace`, Sentry stores `raw_stacktrace: {"frames": [...]}` on the same exception. It holds the original minified frames, plus context lines taken from the minified file. Frames that were not processed appear unchanged in both (`processing.py` `process_js_stacktraces`, `_merge_frame_context`).
- **Errors** go to `event.errors` as `{"type": ..., "url"/"source"/...}`, with types `js_no_source`, `js_invalid_source`, `js_missing_sources_content`, `js_invalid_sourcemap_location` (with `row`/`column`), `js_scraping_disabled`. Missing files under `node_modules` or `chrome-extension:` are not reported (`processing.py` `map_symbolicator_process_js_errors`; `sentry:src/sentry/models/eventerror.py:32-44`).

Example processed frame:

```json
{
  "filename": "./src/components/Cart.tsx",
  "abs_path": "webpack://app/./src/components/Cart.tsx",
  "module": "src/components/Cart",
  "function": "CartTotal",
  "lineno": 42,
  "colno": 17,
  "pre_context": ["..."],
  "context_line": "  const total = items.x.reduce(sum, 0);",
  "post_context": ["..."],
  "in_app": true,
  "data": {
    "sourcemap": "https://app.example.com/assets/index-B2x9.js.map",
    "resolved_with": "debug-id",
    "symbolicated": true
  }
}
```

---

## 6. Legacy release-file upload endpoints

Who calls them:

- **sentry-cli 2.x**, only when the server's `accept` lacks `release_files`, or the chunk-upload GET returns 404 (1.4):
  - `GET /api/0/projects/{org}/{project}/releases/{version}/files/?cursor=` (or the `/organizations/{org}/releases/{version}/files/` form when there is no project). Paginated with the `Link` header; 404 ends the listing (`cli2:src/api/mod.rs:488-556`).
  - `DELETE /api/0/projects/{org}/{project}/releases/{version}/files/{id}/` for files that will be replaced (`cli2:src/api/mod.rs:625-657`).
  - `POST /api/0/projects/{org}/{project}/releases/{version}/files/`: multipart with fields `file` (contents, filename = basename), `name` (the `~/...` URL), `dist` (optional), and `header` repeated as `Key:Value` (e.g. `Sourcemap:app.js.map`). 409 = already exists, treated as OK (`cli2:src/api/mod.rs:1530-1583`; server side `sentry:src/sentry/releases/endpoints/project_release_files.py:159-224`).
  - Release creation first: `POST /api/0/projects/{org}/{project}/releases/` (single project) or `POST /api/0/organizations/{org}/releases/` (`cli2:src/api/mod.rs:684-700`).
- **sentry-cli 2.x with `release_files` but without `artifact_bundles(_v2)`** (or older 2.x without a project): `POST /api/0/organizations/{org}/releases/{version}/assemble/` with body `{"checksum": "...", "chunks": [...]}` and the same response schema as section 3 (`cli2:src/api/mod.rs:998-1021`; `sentry:src/sentry/api/urls.py:2305-2307`).
- **sentry-cli 1.x** and the 2.x `files upload` / `releases files` commands: also the per-file endpoints above. 3.x removed `files` (`cli3` has no `src/commands/files/`).

**Can you skip them?** Yes, if you advertise `release_files` + `artifact_bundles` + `artifact_bundles_v2`. Then every 2.x and 3.x CLI uses the artifact-bundle path, and `newcli` has no legacy path at all.

Clients that would still need them:

- sentry-cli 1.x;
- users who run `sentry-cli files upload` or `releases files ... upload-sourcemaps` by hand;
- older 2.x versions run without `--project`.

**UNVERIFIED:** the exact 2.x version where `--project` became mandatory. 2.58.6 requires it (`cli2:src/commands/sourcemaps/upload.rs` uses `get_projects` → `NonEmptyVec`); 2.20.0/2.30.0 have code paths for "no project".

For these, leave the legacy routes unrouted (404), or answer 410 with `{"detail": "legacy release file upload is not supported; upgrade sentry-cli"}`. The CLI shows `detail` to the user.

---

## 7. Things not verified

- How non-Python/non-Rust zip libraries handle the `SYSB` prefix (4.1).
- `newcli` with a relative chunk `url`, and the response schema expected from `GET /api/0/organizations/{org}/` (1.5).
- The plugin version where `@sentry/vite-plugin` / `@sentry/webpack-plugin` moved from `@sentry/cli` (Rust) to the `sentry` npm CLI (1.5).
- The `sentry` npm CLI's release create/finalize/deploy endpoints (1.6).
- The 2.x version where `--project` became required (6).
- How JS SDKs older than the current `develop` branch fill `debug_meta` (5.1 describes the current code only).
