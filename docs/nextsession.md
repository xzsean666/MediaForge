# Next Session Handoff

Last updated: 2026-06-10

This handoff document records the current project state for the next AI session.

## 1. Current Progress

Current workflow stage:

- Step 1: Architecture Design completed.
- Step 2: Documentation completed.
- Step 3: Context Handoff completed by this file.
- Step 4: Implementation MVP completed.

Important rule:

- Further implementation can continue because the user explicitly requested Step 4 in the current session.

Existing git commits before this handoff file:

```text
7af09b2 feat: add architecture design
222bb43 feat: add project documentation
```

## 2. Current Repository Contents

```text
Agent.md
Cargo.toml
Cargo.lock
docs/
  ARCHITECTURE.md
  SPEC.md
  BUILD.md
  EXTERNAL_DOCS.md
  nextsession.md
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

The repository contains a Rust workspace and runnable `mediaforge` CLI.

## 3. Architecture Summary

MediaForge is a Rust-based media processing and delivery platform.

Core architecture:

- Stateless API and worker runtimes.
- Durable state stored only in S3-compatible object storage.
- Resource IDs derived from content hashes.
- Processing outputs derived from source Resource ID plus canonical parameters.
- Manifests stored in object storage act as the API read model.
- Video processing uses asynchronous object-storage-backed tasks.
- SQLite is allowed only as disposable local cache.
- Local disk is temporary only.

Primary runtime modes:

- `api`: HTTP API only.
- `worker`: background task processing only.
- `combined`: API and worker in one process.

Planned Rust workspace:

```text
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
  mediaforge-cache/
  mediaforge-observability/
```

Implemented MVP crates:

```text
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

## 4. Completed Parts

Completed documentation:

- `Agent.md`: AI working rules, step workflow, architecture principles, git workflow, and implementation guardrails.
- `docs/ARCHITECTURE.md`: system architecture, module responsibilities, data flows, storage layout, identity model, manifest model, and risks.
- `docs/SPEC.md`: product requirements, non-goals, storage model, resource identity, image/video capabilities, API requirements, security, reliability, deployment, and acceptance criteria.
- `docs/BUILD.md`: planned prerequisites, build commands, runtime modes, configuration, local development flow, Docker/Kubernetes direction, verification requirements, and current limitations.
- `docs/EXTERNAL_DOCS.md`: official documentation links for Rust, Axum, Tokio, Tower, Serde, AWS S3, S3-compatible providers, libvips, FFmpeg, SQLite, hashing/auth crates, CDN providers, Docker, and Kubernetes.

Completed git workflow:

- Step 1 committed.
- Step 2 committed.
- Step 3 committed.
- Step 4 MVP commits added.

## 5. Pending Tasks

### 5.1 Ongoing Implementation Rules

If the user asks for more implementation work:

1. Preserve the existing crate boundaries.
2. Keep durable state in S3-compatible object storage.
3. Keep local filesystem storage limited to development and tests.
4. Update `docs/EXTERNAL_DOCS.md` for any new external integration.
5. Commit after each major step.

### 5.2 Completed Step 4 Implementation

Completed:

1. Rust workspace and root `Cargo.toml`.
2. Shared type crate.
3. Core identity and storage key logic.
4. Centralized environment configuration.
5. S3-compatible storage module plus filesystem backend for local development/tests.
6. Manifest repository.
7. Object-storage-backed task repository.
8. Link generator for public, temporary, signed, and storage presigned links.
9. Ingest module with temporary-file streaming session and content hashing.
10. Image module with resize, crop, rotate, and format conversion through the Rust `image` crate.
11. Video module with typed FFmpeg command management.
12. API routes for upload, query, task creation, task status, and link generation.
13. Worker runtime for pending task polling, claiming, processing, upload, and manifest updates.
14. Observability initialization.
15. CLI entry point named `mediaforge`.
16. CPU-limited Backblaze B2 E2E script.
17. Backblaze B2 fallback for providers that do not implement conditional `If-None-Match` writes.
18. Docker deployment files for source build, prebuilt binary, and China mirror prebuilt runtime.
19. Docker compose files and multi-architecture amd64/arm64 build documentation.

Not completed:

1. libvips integration.
2. Image and text watermark execution.
3. SQLite cache crate.
4. Docker and Kubernetes deployment assets.
5. MinIO-specific integration tests.
6. CI automation for real-object-storage E2E tests.
7. Larger FFmpeg sample media coverage across codecs and resolutions.
8. Actual Docker image build verification on a machine with Docker daemon access.

Update: a real Backblaze B2 E2E script now exists at `scripts/e2e-b2.sh` and has been run successfully against B2. MinIO-specific integration tests are still pending.

Each implementation task should be independently buildable and committed separately.

## 6. Recommended First Implementation Slice

Recommended next implementation slice:

1. Add Dockerfile with FFmpeg and libvips runtime dependencies.
2. Add MinIO-based integration tests.
3. Add libvips-backed image processing path.
4. Add watermark support.
5. Add end-to-end API upload tests.

The current foundation already supports deterministic Resource IDs, Result IDs, Task IDs, manifests, tasks, links, API, and worker runtime.

## 7. Risks and Unknowns

S3 compatibility:

- Providers differ in conditional writes, metadata handling, multipart behavior, path-style support, and presigned URL details.
- Task claiming depends on conditional object creation when strict duplicate prevention is required.

Task execution:

- Exact-once processing cannot be assumed.
- The implementation must rely on deterministic keys and idempotent manifest writes.

FFmpeg:

- H265 and AV1 availability depends on the FFmpeg build.
- Runtime images must be validated for required codecs.

libvips:

- Installation and format support vary across operating systems and containers.
- SVG and AVIF behavior may depend on optional system libraries.

Security:

- FFmpeg must be invoked without shell interpolation.
- Link signing must include expiration and tamper protection.
- Secrets must never be logged or committed.

Performance:

- Large media uploads must stream where possible.
- Avoid loading full videos or large images into memory.
- Local development should keep Cargo build jobs at 1 and FFmpeg thread count explicit to avoid starving the IDE.

## 8. Next Actions for the Next AI Session

1. Read `Agent.md`.
2. Read `docs/ARCHITECTURE.md`.
3. Read `docs/SPEC.md`.
4. Read `docs/BUILD.md`.
5. Read `docs/EXTERNAL_DOCS.md` before selecting external APIs.
6. Confirm whether the user is requesting documentation changes or Step 4 implementation.
7. If Step 4 is requested, start with the first implementation slice listed above.
8. Commit after each major step.

## 9. Do Not Do Yet

Do not:

- Add a database dependency as a source of truth.
- Store media permanently on local disk.
- Scatter configuration across modules.
- Skip manifest design.
- Hide storage keys or link behavior inside unrelated modules.
- Treat SQLite as required for correctness.
