# MediaForge Architecture

Version: 1.0

This document defines the architecture for MediaForge, a Rust-based media processing and delivery platform.

The architecture is optimized for AI-assisted maintenance. Each module has a narrow responsibility, explicit inputs and outputs, and minimal hidden context.

## 1. Overall System Architecture

MediaForge is a stateless service that accepts media assets, processes images and videos, stores all durable outputs in S3-compatible object storage, and returns stable Resource IDs plus generated access links.

The platform is organized around five architectural ideas:

1. Object storage is the only durable source of truth.
2. Resource IDs are derived from content hashes.
3. Processing results are deterministic from source content plus normalized parameters.
4. Video processing runs through asynchronous tasks.
5. API reads resource state from manifest files stored beside the media objects.

High-level runtime components:

```text
Client
  |
  v
API Server (Axum / Tokio / Tower)
  |
  |-- Upload and query requests
  |-- Link generation requests
  |-- Task creation requests
  v
Core Services
  |
  |-- Ingest Service
  |-- Resource Identifier Service
  |-- Manifest Service
  |-- Storage Service
  |-- Image Processing Service
  |-- Video Task Service
  |-- Link Signing Service
  v
S3-Compatible Object Storage
  |
  |-- Original media
  |-- Derived media
  |-- Manifest files
  |-- Task descriptors
  |-- Task status files

Worker Runtime
  |
  |-- Polls or receives task descriptors
  |-- Claims processing work
  |-- Runs FFmpeg / libvips pipelines
  |-- Uploads outputs
  |-- Updates manifests and task status
```

MediaForge may run in one of three modes:

```text
api       Runs only the HTTP API server.
worker    Runs only background processing workers.
combined  Runs API and worker runtime in one process for simple deployments.
```

All modes use the same configuration model and object storage layout.

## 2. Required Directory Structure

The intended Rust project structure is:

```text
.
|-- Agent.md
|-- docs/
|   |-- ARCHITECTURE.md
|   |-- SPEC.md
|   |-- BUILD.md
|   |-- EXTERNAL_DOCS.md
|   `-- nextsession.md
|-- Cargo.toml
|-- crates/
|   |-- mediaforge-cli/
|   |-- mediaforge-api/
|   |-- mediaforge-worker/
|   |-- mediaforge-core/
|   |-- mediaforge-config/
|   |-- mediaforge-types/
|   |-- mediaforge-storage/
|   |-- mediaforge-ingest/
|   |-- mediaforge-manifest/
|   |-- mediaforge-image/
|   |-- mediaforge-video/
|   |-- mediaforge-tasks/
|   |-- mediaforge-links/
|   |-- mediaforge-cache/
|   `-- mediaforge-observability/
|-- deploy/
|   |-- docker/
|   `-- kubernetes/
|-- scripts/
`-- tests/
```

The `crates/` layout is preferred over a large single crate because each crate can be understood and tested in isolation.

`mediaforge-cli` is the command-line entry point. It selects `api`, `worker`, or `combined` mode and delegates behavior to the API and worker crates.

## 3. Module Breakdown

### 3.1 `mediaforge-api`

Purpose:

HTTP API entry point built with Axum.

Input:

- Multipart uploads.
- JSON processing requests.
- Resource query requests.
- Link generation requests.
- Task status requests.

Output:

- JSON API responses.
- Resource IDs.
- Task IDs.
- Generated media links.

Dependencies:

- `mediaforge-config`
- `mediaforge-types`
- `mediaforge-ingest`
- `mediaforge-manifest`
- `mediaforge-tasks`
- `mediaforge-links`
- `mediaforge-observability`

Rules:

- Contains route definitions and request validation only.
- Does not contain image, video, storage, or hashing logic.
- Does not depend on local process state for request correctness.

### 3.1.1 `mediaforge-cli`

Purpose:

Command-line entry point for running MediaForge.

Input:

- CLI subcommand: `api`, `worker`, or `combined`.
- Centralized runtime configuration from `mediaforge-config`.

Output:

- Running API server.
- Running worker loop.
- Combined API and worker runtime.

Dependencies:

- `mediaforge-config`
- `mediaforge-api`
- `mediaforge-worker`
- `mediaforge-observability`

Rules:

- Contains process startup only.
- Does not contain API route logic, task logic, storage logic, or media processing logic.
- Runtime mode must remain explicit.

### 3.2 `mediaforge-worker`

Purpose:

Background task runtime for video processing and long-running derived media generation.

Input:

- Task descriptors from object storage.
- Source resource manifests.
- Processing profiles.

Output:

- Processed media objects.
- Updated manifests.
- Task status objects.

Dependencies:

- `mediaforge-config`
- `mediaforge-types`
- `mediaforge-storage`
- `mediaforge-manifest`
- `mediaforge-video`
- `mediaforge-image`
- `mediaforge-tasks`
- `mediaforge-observability`

Rules:

- Claims work explicitly through the task module.
- Treats duplicate task execution as possible and must remain idempotent.
- Does not require a local queue, local database, or local permanent files.

### 3.3 `mediaforge-core`

Purpose:

Shared domain behavior that is not tied to HTTP, S3, FFmpeg, or libvips.

Input:

- File metadata.
- Hash values.
- Normalized processing parameters.

Output:

- Resource IDs.
- Result IDs.
- Canonical parameter strings.
- Storage key plans.

Dependencies:

- `mediaforge-types`

Rules:

- Pure logic where practical.
- No network calls.
- No file system writes.
- No hidden global state.

### 3.4 `mediaforge-config`

Purpose:

Centralized runtime configuration.

Input:

- Environment variables.
- Optional configuration file.

Output:

- Validated configuration struct.

Dependencies:

- `mediaforge-types`

Rules:

- All configurable behavior must originate here.
- No module should read environment variables directly.
- Configuration field names must be explicit.

### 3.5 `mediaforge-types`

Purpose:

Shared API and domain data structures.

Input:

- None at runtime.

Output:

- Typed request models.
- Typed response models.
- Manifest models.
- Task models.
- Processing parameter models.

Dependencies:

- Serde

Rules:

- Contains data structures only.
- Avoids business logic except simple validation helpers when needed.
- Names must be descriptive and stable because API clients depend on them.

### 3.6 `mediaforge-storage`

Purpose:

S3-compatible object storage access.

Input:

- Storage keys.
- Byte streams.
- Metadata.
- Presign options.

Output:

- Uploaded objects.
- Download streams.
- Object metadata.
- Existence checks.
- Presigned object storage URLs when configured.

Dependencies:

- `mediaforge-config`
- `mediaforge-types`
- AWS SDK for Rust S3 client

Rules:

- This is the only module that talks directly to object storage.
- Bucket names, endpoints, regions, credentials, and path style settings come from configuration.
- Storage keys are accepted from `mediaforge-core`; they are not invented ad hoc.
- A filesystem backend may exist only for local development and tests. Production durable media storage must use S3-compatible object storage.

### 3.7 `mediaforge-ingest`

Purpose:

Accept and normalize incoming media before durable storage.

Input:

- Multipart file streams.
- Optional remote source streams in future extensions.

Output:

- Temporary local processing file.
- Content hash.
- File size.
- Detected MIME type.
- Initial source metadata.

Dependencies:

- `mediaforge-core`
- `mediaforge-types`
- `mediaforge-storage`

Rules:

- Local files are temporary only.
- Hashing must happen during streaming where possible.
- Temporary files must be cleaned after request or task completion.

### 3.8 `mediaforge-manifest`

Purpose:

Read, write, validate, and merge resource manifests.

Input:

- Resource ID.
- Source metadata.
- Derived media metadata.
- Task results.

Output:

- Manifest JSON files in object storage.
- Typed manifest values for API responses.

Dependencies:

- `mediaforge-core`
- `mediaforge-types`
- `mediaforge-storage`

Rules:

- API queries should prefer manifest reads over listing raw storage keys.
- Manifest writes must be idempotent.
- Manifests must not require a database record to be meaningful.

### 3.9 `mediaforge-image`

Purpose:

Image transformation pipeline.

Input:

- Source image file.
- Normalized image operation parameters.
- Watermark assets when needed.

Output:

- Derived image files.
- Derived image metadata.

Dependencies:

- `mediaforge-types`
- libvips bindings when available
- Rust `image` crate as fallback for narrow operations

Rules:

- libvips is preferred for performance and memory efficiency.
- Processing parameters must be normalized before deriving output IDs.
- The same source image and same parameters must produce the same result key.

### 3.10 `mediaforge-video`

Purpose:

Video transformation pipeline through FFmpeg.

Input:

- Source video file.
- Normalized video processing profile.
- Screenshot or HLS generation parameters.

Output:

- Transcoded video files.
- HLS playlists and segments.
- Screenshot images.
- Cover images.
- Derived media metadata.

Dependencies:

- `mediaforge-types`
- FFmpeg executable
- `mediaforge-image` for cover image post-processing when needed

Rules:

- Rust manages FFmpeg execution but does not implement encoders.
- Command arguments must be built from typed profiles, not string concatenation from raw request input.
- Each FFmpeg run must use an isolated temporary working directory.

### 3.11 `mediaforge-tasks`

Purpose:

Asynchronous task descriptors, task claiming, task status, and idempotency.

Input:

- Resource ID.
- Processing request.
- Worker lease request.

Output:

- Task ID.
- Task descriptor object.
- Task status object.
- Worker lease object.

Dependencies:

- `mediaforge-core`
- `mediaforge-types`
- `mediaforge-storage`

Rules:

- Task state is stored in object storage.
- Task IDs should be deterministic when the source resource and normalized parameters are identical.
- Worker claiming should use S3 conditional writes when the configured storage provider supports them.
- If strict conditional writes are unavailable, duplicate processing may occur, but final result objects and manifests must remain deterministic and safe to reuse.

### 3.12 `mediaforge-links`

Purpose:

Generate public, temporary, and signed links.

Input:

- Resource ID.
- Manifest entry.
- Link policy.
- Expiration duration.
- Caller authorization context.

Output:

- Public CDN URL.
- Temporary signed URL.
- HMAC-protected platform URL.
- Optional S3 presigned URL.

Dependencies:

- `mediaforge-config`
- `mediaforge-types`
- `mediaforge-storage`

Rules:

- Link behavior is explicit in the request or route.
- CDN base URL and signing secrets come from centralized configuration.
- Signed links must include expiration and tamper protection.

### 3.13 `mediaforge-cache`

Purpose:

Optional local cache for hot manifests, recent task status, and repeated metadata reads.

Input:

- Manifest reads.
- Task status reads.
- Cache policy.

Output:

- Cached values.

Dependencies:

- SQLite
- `mediaforge-types`

Rules:

- The system must work after deleting the SQLite file.
- Cache misses must fall back to object storage.
- This module must not become a source of truth.

### 3.14 `mediaforge-observability`

Purpose:

Logging, tracing, metrics hooks, and operational diagnostics.

Input:

- Request context.
- Task context.
- Error context.

Output:

- Structured logs.
- Trace spans.
- Metrics events.

Dependencies:

- Tracing ecosystem

Rules:

- No business decisions should depend on observability state.
- Logs must include Resource ID and Task ID when available.

## 4. Data Flow

### 4.1 Image Upload and Processing Flow

```text
Client uploads image
  |
  v
API validates request
  |
  v
Ingest streams upload to temporary file and computes content hash
  |
  v
Core derives Resource ID from hash
  |
  v
Manifest checks if resource already exists
  |
  |-- Exists: return existing Resource ID and links
  |
  |-- Missing:
        |
        v
        Storage uploads original image
        |
        v
        Image module generates requested derivatives
        |
        v
        Storage uploads derived images
        |
        v
        Manifest writes source and derivative metadata
        |
        v
        API returns Resource ID and links
```

### 4.2 Video Upload and Async Processing Flow

```text
Client uploads video
  |
  v
API validates request
  |
  v
Ingest streams upload to temporary file and computes content hash
  |
  v
Core derives Resource ID from hash
  |
  v
Storage uploads original video if missing
  |
  v
Manifest writes or reuses source metadata
  |
  v
Task module creates deterministic task descriptor
  |
  v
API returns Resource ID and Task ID
  |
  v
Worker discovers pending task
  |
  v
Worker claims task lease
  |
  v
Worker downloads source or reuses local temporary copy
  |
  v
Video module runs FFmpeg operations
  |
  v
Storage uploads derived videos, HLS files, screenshots, and cover
  |
  v
Manifest merges derived metadata
  |
  v
Task status becomes completed
```

### 4.3 Resource Query Flow

```text
Client requests resource by Resource ID
  |
  v
API validates Resource ID
  |
  v
Manifest module loads manifest from object storage or cache
  |
  v
Link module generates links according to request policy
  |
  v
API returns resource metadata and links
```

### 4.4 Link Generation Flow

```text
Client requests link
  |
  v
API validates access policy
  |
  v
Manifest resolves target object
  |
  v
Link module chooses public, temporary, HMAC signed, or S3 presigned mode
  |
  v
API returns generated URL with expiration metadata
```

## 5. Object Storage Layout

Storage keys must use prefix partitioning. No resource type should place all objects in one flat directory.

Recommended layout:

```text
media/
  sha256/
    ab/
      cd/
        <resource_id>/
          manifest.json
          original/
            source.<ext>
          image/
            <result_id>/
              output.<ext>
              metadata.json
          video/
            <result_id>/
              output.<ext>
              metadata.json
          hls/
            <result_id>/
              master.m3u8
              variant-1080p.m3u8
              segments/
                segment-000001.ts
          screenshots/
            <result_id>/
              shot-000001.jpg
          cover/
            <result_id>/
              cover.jpg

tasks/
  pending/
    <task_id>.json
  leases/
    <task_id>.json
  status/
    <task_id>.json
  completed/
    <task_id>.json
  failed/
    <task_id>.json
```

`ab` and `cd` are hash prefix partitions derived from the Resource ID.

## 6. Resource and Result Identity

Resource ID:

```text
sha256:<hex_hash>
```

Result ID:

```text
sha256:<hash_of_source_resource_id_and_canonical_parameters>
```

Task ID:

```text
sha256:<hash_of_source_resource_id_operation_type_and_canonical_parameters>
```

Rules:

- Identical input bytes must produce the same Resource ID.
- Identical source plus identical normalized parameters must produce the same Result ID.
- API clients should not depend on bucket names, prefixes, or object keys.
- Resource ID is the stable public identifier.

## 7. Manifest Model

Each resource group must have a manifest.

The manifest should describe:

- Resource ID.
- Media kind.
- Original object.
- Derived objects.
- MIME types.
- File sizes.
- Width, height, duration, codec, and bitrate when available.
- Creation time.
- Processing parameters.
- Task history references.

The manifest is the API read model. API queries should not reconstruct resource state by scanning storage unless the manifest is missing or being repaired.

## 8. Key Design Decisions

### 8.1 Rust as the implementation language

Rust is selected for predictable performance, low memory usage, safe concurrency, and simple deployment as a compiled binary.

### 8.2 Axum, Tokio, and Tower for the API runtime

Axum provides the HTTP framework, Tokio provides async runtime, and Tower provides middleware structure.

### 8.3 Object storage as source of truth

MediaForge does not require PostgreSQL, MySQL, MariaDB, or another traditional database. S3-compatible object storage stores originals, derivatives, manifests, and task descriptors.

### 8.4 SQLite is optional cache only

SQLite may cache hot data, but deleting the SQLite file must not break correctness.

### 8.5 FFmpeg is the only video encoding engine

Rust manages FFmpeg processes and validates parameters. It does not implement codecs.

### 8.6 libvips is preferred for image processing

libvips is preferred for large images and lower memory usage. The Rust `image` crate may be used for smaller operations or fallback behavior.

### 8.7 Idempotency is required

All write operations must be safe to retry. Duplicate uploads, duplicate tasks, or duplicate worker execution should converge to the same manifest and same object keys.

### 8.8 Exact-once processing is not assumed

S3-compatible providers differ in conditional write behavior. MediaForge should use conditional writes for task leases when available, but correctness must rely on deterministic outputs and idempotent manifest updates.

### 8.9 Configuration is centralized

Runtime configuration belongs in `mediaforge-config`. No other module reads environment variables directly.

### 8.10 Composition over inheritance

Rust traits may define small interfaces at module boundaries, but behavior should be composed explicitly rather than hidden behind deep abstraction.

## 9. External Integration Boundaries

MediaForge must keep all third-party integrations behind explicit modules:

- S3-compatible storage is isolated in `mediaforge-storage`.
- FFmpeg process handling is isolated in `mediaforge-video`.
- libvips and image processing are isolated in `mediaforge-image`.
- CDN URL and signing behavior are isolated in `mediaforge-links`.
- Optional SQLite cache is isolated in `mediaforge-cache`.

Current external documentation links are stored separately in `docs/EXTERNAL_DOCS.md`.

## 10. Risks and Unknowns

1. S3-compatible providers may differ in support for conditional writes, object metadata behavior, multipart upload details, and presigned URL compatibility.
2. libvips installation differs across Linux distributions and container images.
3. FFmpeg codec availability depends on the installed FFmpeg build.
4. Strict prevention of duplicate video processing may require a provider that supports reliable conditional object creation or a future optional queue backend.
5. HLS segment naming and cache headers need careful CDN compatibility testing.
6. AVIF and H265 support may depend on system libraries and FFmpeg licensing/build options.

## Implementation Update — Upload, Auth, Reliability (2026-06-10)

The realized system differs from the original MVP sketch in the areas below.
New crates: `mediaforge-auth` (HS256 JWT verification + Axum middleware) and
`mediaforge-uploads` (upload-session lifecycle and finalization queue).

### Upload data flow (presigned, server-side hashing)

```text
client ──POST /v1/uploads──▶ API ──(presigned PUT URL, upload_id)──▶ client
client ──PUT bytes────────▶ object storage (staging key)            [API not in path]
client ──POST .../complete▶ API ──HEAD staging, enqueue finalize──▶ uploads/pending/
worker ──finalize────────▶ download staging (stream+hash) → resource_id
                            → server-side copy to content-addressed key
                            → write manifest → delete staging
                            → schedule processing task → mark session completed
client ──GET /v1/uploads/{id}──▶ resource_id + processing_task_id
```

Rationale: bytes never traverse the API, so large videos are unaffected by API
memory/limits; the worker computes the SHA-256 during the download it must do
anyway, so an untrusted client cannot poison the content-addressed namespace and
nothing is downloaded twice. Object keys: `uploads/sessions/{id}.json`,
`uploads/pending/{id}.json`, `uploads/staging/{id}/source`.

### Source expiry

Per-object TTL is app-level (S3 lifecycle is not portable across providers).
Markers live at `sources/expiring/{unix:020}/{resource}.json`; the zero-padded
timestamp makes the listing time-ordered so the worker reaper stops at the first
not-yet-due marker.

### Concurrency and durability

- Manifest mutations use optimistic concurrency: `get_object_with_version` +
  `put_object_if_match` (S3 ETag `If-Match`/`If-None-Match`; filesystem uses a
  content hash plus an in-process lock, dev-only) with bounded retry.
- Task lifecycle: leases expire and are reclaimed; failures retry up to a cap;
  completed/failed tasks are dequeued so listing stays cheap.
- Storage gained streaming `put_object_streaming` / `get_object_to_file` and a
  server-side `copy_object`, so neither the API nor the worker buffers whole
  media files in memory.

### Authentication

Optional HS256 JWT, off by default, enforced by middleware on all routes except
`/health`. See SPEC §18.2.
