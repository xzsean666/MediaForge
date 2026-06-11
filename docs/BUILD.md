# Build and Usage Guide

Version: 1.0

This document describes how MediaForge should be built, configured, and run.

Current repository state: a Rust workspace MVP exists with API, worker, storage, manifest, task, link, ingest, image, video, observability, and CLI crates.

## 1. Prerequisites

Required for production-style runtime:

- Rust stable toolchain.
- Cargo.
- FFmpeg executable.
- libvips runtime and development headers.
- S3-compatible object storage.

Recommended for local development:

- Docker.
- Docker Compose or an equivalent local orchestration tool.
- MinIO or another local S3-compatible service.
- SQLite command-line tools for optional cache inspection.

Optional:

- Kubernetes cluster for deployment validation.
- CDN account for public delivery tests.

## 2. Rust Workspace

The workspace structure is defined in `docs/ARCHITECTURE.md`.

Current top-level workspace:

```text
Cargo.toml
crates/
  mediaforge-cli/
  mediaforge-api/
  mediaforge-worker/
  mediaforge-core/
  mediaforge-config/
  mediaforge-types/
  mediaforge-storage/
  mediaforge-ingest/
  mediaforge-manifest/
  mediaforge-image/
  mediaforge-video/
  mediaforge-tasks/
  mediaforge-links/
  mediaforge-observability/
```

`mediaforge-cache` remains optional and is not wired into the current MVP.

## 3. Build Commands

Standard verification commands:

```text
cargo fmt --all
cargo clippy -j1 --workspace --all-targets --all-features
cargo test -j1 --workspace --all-features -- --test-threads=1
cargo build -j1 --workspace --release
```

The project should keep these commands working as the baseline verification flow.

This repository includes `.cargo/config.toml` with:

```text
[build]
jobs = 1
```

Current local default is `jobs = 1`. This keeps Rust compilation from consuming all CPU cores during local development.

## 4. Runtime Modes

MediaForge should support:

```text
mediaforge api
mediaforge worker
mediaforge combined
```

Mode behavior:

- `api`: runs only the HTTP API server.
- `worker`: runs only background workers.
- `combined`: runs API and worker in one process.

The exact CLI may change during implementation, but the three runtime modes must remain explicit.
The current CLI binary is `mediaforge`.

## 5. Planned Configuration

All configuration is centralized in the `mediaforge-config` crate.

Expected environment variables:

```text
MEDIAFORGE_MODE=api|worker|combined
MEDIAFORGE_HTTP_BIND=0.0.0.0:8080

MEDIAFORGE_S3_ENDPOINT=https://example-s3-endpoint
MEDIAFORGE_S3_REGION=auto
MEDIAFORGE_S3_BUCKET=mediaforge
MEDIAFORGE_S3_ACCESS_KEY_ID=replace-me
MEDIAFORGE_S3_SECRET_ACCESS_KEY=replace-me
MEDIAFORGE_S3_FORCE_PATH_STYLE=true|false

MEDIAFORGE_STORAGE_BACKEND=s3|filesystem
MEDIAFORGE_FILESYSTEM_STORAGE_ROOT=/tmp/mediaforge-object-store

MEDIAFORGE_CDN_BASE_URL=https://cdn.example.com
MEDIAFORGE_LINK_SIGNING_SECRET=replace-me
MEDIAFORGE_JWT_SECRET=replace-me

MEDIAFORGE_TEMP_DIR=/tmp/mediaforge
MEDIAFORGE_FFMPEG_PATH=ffmpeg
MEDIAFORGE_FFPROBE_PATH=ffprobe
MEDIAFORGE_FFMPEG_THREADS=1
MEDIAFORGE_FFMPEG_VIDEO_ACCELERATION=none|nvidia

MEDIAFORGE_WORKER_CONCURRENCY=2
MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS=5

MEDIAFORGE_SQLITE_CACHE_PATH=/tmp/mediaforge/cache.sqlite
MEDIAFORGE_LOG_LEVEL=info
```

Secrets must not be committed to the repository.

The `filesystem` storage backend is for local development and tests only. Production deployments must use S3-compatible object storage.

## 6. Local Development Flow

For a local filesystem-backed smoke test:

```text
export MEDIAFORGE_STORAGE_BACKEND=filesystem
export MEDIAFORGE_FILESYSTEM_STORAGE_ROOT=/tmp/mediaforge-object-store
export MEDIAFORGE_CDN_BASE_URL=http://localhost:8080/local-cdn
export MEDIAFORGE_LINK_SIGNING_SECRET=development-secret
cargo run -p mediaforge-cli -- combined
```

Then in another shell:

```text
curl http://127.0.0.1:8080/health
```

For S3-compatible local development:

1. Start local S3-compatible storage.
2. Create the configured bucket.
3. Export MediaForge environment variables.
4. Run formatting and tests.
5. Start MediaForge in `combined` mode for local testing.
6. Upload an image and verify manifest creation.
7. Upload a video and verify task creation.
8. Start or verify worker processing.
9. Query the Resource ID through the API.
10. Generate public, temporary, and signed links.

Current API routes:

```text
GET    /health
POST   /v1/uploads                       # open a presigned direct-to-S3 upload
POST   /v1/uploads/{upload_id}/complete  # confirm upload, queue finalization
GET    /v1/uploads/{upload_id}           # poll for finalized resource_id
DELETE /v1/uploads/{upload_id}           # cancel an upload
GET    /v1/resources/{resource_id}
POST   /v1/resources/{resource_id}/image
POST   /v1/resources/{resource_id}/video
POST   /v1/resources/{resource_id}/links
GET    /v1/tasks/{task_id}
```

Uploads go directly to object storage via a presigned PUT URL; the API never
streams upload bytes. See SPEC §18.1. When `MEDIAFORGE_AUTH_ENABLED=true`, every
route except `/health` requires `Authorization: Bearer <HS256 JWT>`.

## 7. S3-Compatible E2E Test (R2 / B2 / generic S3)

The repository includes a real-environment E2E script that drives the presigned
direct-to-storage upload flow against any S3-compatible provider:

```text
./scripts/e2e-b2.sh
```

The script reads `.env.test`. Select a provider and supply S3 credentials:

```text
MEDIAFORGE_E2E_PROVIDER=r2            # r2 | b2 | s3
MEDIAFORGE_S3_BUCKET=...
MEDIAFORGE_S3_ACCESS_KEY_ID=...
MEDIAFORGE_S3_SECRET_ACCESS_KEY=...
MEDIAFORGE_S3_ENDPOINT=https://<accountid>.r2.cloudflarestorage.com  # R2/S3; blank for B2
MEDIAFORGE_S3_REGION=auto
MEDIAFORGE_S3_FORCE_PATH_STYLE=true
```

- `r2` / `s3`: set `MEDIAFORGE_S3_ENDPOINT` explicitly (R2 uses
  `https://<accountid>.r2.cloudflarestorage.com`, `REGION=auto`).
- `b2`: leave the endpoint blank — the script calls the Backblaze B2 authorize
  API once to discover the S3 endpoint and region, then runs entirely through
  the S3-compatible backend.

The script runs `mediaforge combined`, performs a presigned image and video
upload, waits for worker finalization and processing, and verifies the
resulting manifests. Set `MEDIAFORGE_E2E_AUTH=1` to also verify JWT auth gating
(a separate auth-enabled server, minting a token with `mediaforge mint-token`).
See `.env.test` for a Cloudflare R2 example block.

CPU and compile limits are enabled by default:

```text
MEDIAFORGE_E2E_CARGO_JOBS=1
MEDIAFORGE_E2E_FFMPEG_THREADS=1
MEDIAFORGE_E2E_NICE_LEVEL=15
RAYON_NUM_THREADS=1
```

The script covers:

- API health check.
- Synthetic image generation.
- Synthetic video generation.
- Image upload to B2.
- Duplicate image upload returning the same Resource ID.
- Image manifest query.
- Public link generation.
- B2 presigned link generation.
- Image processing task creation and execution.
- Video upload to B2.
- Duplicate video upload returning the same Resource ID and Task ID.
- Default video transcode task execution.
- HLS video task execution.
- Screenshot, cover, HLS playlist, and HLS segment manifest verification.
- Task status verification.

On failure, the script keeps its temporary work directory and prints the API log tail.

## 8. Object Storage Setup Expectations

The storage provider must support:

- Object upload.
- Object download.
- Object existence checks.
- Prefix listing.
- Metadata reads.
- Presigned URLs if direct object storage links are enabled.

Preferred for task claiming:

- Conditional object creation using S3 preconditions.

Provider compatibility must be verified because S3-compatible services differ in edge behavior.

## 9. FFmpeg Requirements

FFmpeg must be available in the runtime image or host environment.

Required tools:

- `ffmpeg`
- `ffprobe`

Codec support depends on the FFmpeg build. H265 and AV1 support must be verified in the target runtime image before production use.

NVIDIA acceleration requires Docker GPU access plus an FFmpeg build with NVENC
encoders. Set `MEDIAFORGE_FFMPEG_VIDEO_ACCELERATION=nvidia` to use
`h264_nvenc` and `hevc_nvenc` for H264/H265 outputs. In Docker Compose, use the
`docker-compose.gpu.yml` overlay and set `MEDIAFORGE_NVIDIA_DEVICE_ID` to the
GPU index or UUID that should be visible to the container.

## 10. libvips Requirements

libvips must be available in the runtime image or host environment for the preferred image processing path.

The implementation should detect unavailable libvips behavior early and return clear startup or processing errors.

## 11. Docker Deployment

Docker deployment files are now provided:

```text
Dockerfile
Dockerfile.prebuilt
Dockerfile.prebuilt.cn
docker-compose.yml
docker-compose.prebuilt.yml
docker-compose.prebuilt.cn.yml
scripts/build-prebuilt-binary.sh
docs/RUST_DOCKER_DEPLOYMENT.md
```

The Docker images include:

- MediaForge binary.
- FFmpeg and ffprobe.
- libvips runtime packages.
- Non-root `mediaforge` user.
- Writable temporary directory.
- Environment-variable configuration.
- Healthcheck against `/health`.

The Docker setup supports:

```text
linux/amd64
linux/arm64
```

The image should not include long-term media storage.

See `docs/RUST_DOCKER_DEPLOYMENT.md` for buildx, prebuilt binary, China mirror, and compose usage.

## 12. Kubernetes Deployment Direction

Kubernetes support should eventually provide:

- Deployment for API replicas.
- Deployment for worker replicas.
- Optional combined deployment for small setups.
- ConfigMap for non-secret configuration.
- Secret for credentials.
- Readiness and liveness probes.
- Resource limits for CPU, memory, and temporary storage.
- Horizontal scaling for API and workers.

No PersistentVolume should be required for durable media state.

## 13. Verification Requirements

Every major module should keep focused tests:

- Resource ID generation.
- Result ID generation.
- Canonical parameter normalization.
- Storage key generation.
- Manifest serialization and merging.
- Link signing and expiration validation.
- Task ID generation and idempotency behavior.
- Image parameter validation.
- Video profile validation.

Integration tests should cover:

- Upload to S3-compatible storage.
- Manifest round trip.
- Duplicate upload behavior.
- Duplicate processing request behavior.
- Temporary link generation.
- Signed link validation.

End-to-end tests should cover:

- Image upload and derivative generation.
- Video upload and async task completion.
- HLS output availability.
- Resource query from manifest.

## 14. Current Limitations

Current MVP limitations:

- Image resize, crop, rotate, and format conversion are implemented through the Rust `image` crate.
- Image and text watermark operations currently return explicit unsupported errors.
- libvips integration is not implemented yet.
- FFmpeg execution is implemented, but runtime codec support depends on the installed FFmpeg build.
- Uploads are streamed to temporary disk during ingest, then uploaded to storage.
- SQLite cache remains optional and is not implemented in the current MVP.

Production use still requires S3-compatible object storage, FFmpeg, and image backend validation in the target runtime image.
