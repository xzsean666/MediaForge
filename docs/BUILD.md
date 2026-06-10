# Build and Usage Guide

Version: 1.0

This document describes how MediaForge should be built, configured, and run.

Current repository state: documentation and architecture are being prepared. Rust implementation code has not been added yet.

## 1. Prerequisites

Required for implementation:

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

## 2. Planned Rust Workspace

The planned workspace structure is defined in `docs/ARCHITECTURE.md`.

Expected top-level workspace:

```text
Cargo.toml
crates/
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
  mediaforge-cache/
  mediaforge-observability/
```

No Rust crates exist yet. They should be created only during Step 4 after explicit user approval.

## 3. Planned Build Commands

After implementation exists, standard commands should be:

```text
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo build --workspace --release
```

The project should keep these commands working as the baseline verification flow.

## 4. Planned Runtime Modes

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

## 5. Planned Configuration

All configuration must be centralized in the future `mediaforge-config` crate.

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

MEDIAFORGE_CDN_BASE_URL=https://cdn.example.com
MEDIAFORGE_LINK_SIGNING_SECRET=replace-me
MEDIAFORGE_JWT_SECRET=replace-me

MEDIAFORGE_TEMP_DIR=/tmp/mediaforge
MEDIAFORGE_FFMPEG_PATH=ffmpeg
MEDIAFORGE_FFPROBE_PATH=ffprobe

MEDIAFORGE_WORKER_CONCURRENCY=2
MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS=5

MEDIAFORGE_SQLITE_CACHE_PATH=/tmp/mediaforge/cache.sqlite
MEDIAFORGE_LOG_LEVEL=info
```

Secrets must not be committed to the repository.

## 6. Planned Local Development Flow

After implementation exists:

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

## 7. Object Storage Setup Expectations

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

## 8. FFmpeg Requirements

FFmpeg must be available in the runtime image or host environment.

Required tools:

- `ffmpeg`
- `ffprobe`

Codec support depends on the FFmpeg build. H265 and AV1 support must be verified in the target runtime image before production use.

## 9. libvips Requirements

libvips must be available in the runtime image or host environment for the preferred image processing path.

The implementation should detect unavailable libvips behavior early and return clear startup or processing errors.

## 10. Docker Deployment Direction

Docker support should eventually provide:

- Runtime image with MediaForge binary.
- FFmpeg installed.
- libvips installed.
- Non-root runtime user.
- Writable temporary directory.
- Environment-variable configuration.

The image should not include long-term media storage.

## 11. Kubernetes Deployment Direction

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

## 12. Verification Requirements

Once implementation begins, every major module should have focused tests:

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

## 13. Current Limitations

At this documentation stage:

- There is no `Cargo.toml`.
- There are no Rust crates.
- There is no runnable binary.
- Build commands are planned commands, not currently executable.

Implementation must wait until the user explicitly requests Step 4.

