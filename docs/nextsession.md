# Next Session Handoff

Last updated: 2026-06-10

This handoff document records the current project state for the next AI session.

## 1. Current Progress

Current workflow stage:

- Step 1: Architecture Design completed.
- Step 2: Documentation completed.
- Step 3: Context Handoff completed by this file.
- Step 4: Implementation not started.

Important rule:

- Do not write implementation code until the user explicitly requests Step 4.

Existing git commits before this handoff file:

```text
7af09b2 feat: add architecture design
222bb43 feat: add project documentation
```

## 2. Current Repository Contents

```text
Agent.md
docs/
  ARCHITECTURE.md
  SPEC.md
  BUILD.md
  EXTERNAL_DOCS.md
  nextsession.md
```

There is currently no Rust workspace, no `Cargo.toml`, and no implementation code.

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
- Step 3 is represented by this file and should be the latest commit after the current work is completed.

## 5. Pending Tasks

### 5.1 Before Implementation

If the user asks for changes before Step 4:

1. Update documentation only.
2. Keep all project docs in `docs/`, except root `Agent.md`.
3. Update `docs/EXTERNAL_DOCS.md` for any new external integration.
4. Commit documentation changes.

### 5.2 Step 4 Implementation Plan

Only after explicit user approval:

1. Create Rust workspace and root `Cargo.toml`.
2. Create `mediaforge-types` with request, response, manifest, task, and processing parameter types.
3. Create `mediaforge-core` with Resource ID, Result ID, Task ID, canonical parameter, and storage key logic.
4. Create `mediaforge-config` with centralized configuration loading and validation.
5. Create `mediaforge-storage` with S3-compatible storage access.
6. Create `mediaforge-manifest` for manifest read, write, and merge behavior.
7. Create `mediaforge-ingest` for streaming upload, temporary files, hashing, MIME detection, and cleanup.
8. Create `mediaforge-links` for public, temporary, HMAC signed, and optional S3 presigned links.
9. Create `mediaforge-tasks` for object-storage task descriptors, task status, leases, and idempotency.
10. Create `mediaforge-image` with parameter validation first, then libvips-backed processing.
11. Create `mediaforge-video` with parameter validation first, then FFmpeg execution management.
12. Create `mediaforge-api` with routes that delegate all business behavior to modules.
13. Create `mediaforge-worker` with task polling, claiming, execution, and result upload.
14. Add observability through `mediaforge-observability`.
15. Add integration tests using S3-compatible local storage.
16. Add Docker and Kubernetes deployment assets.

Each implementation task should be independently buildable and committed separately.

## 6. Recommended First Implementation Slice

When Step 4 is approved, the lowest-risk first slice is:

1. Workspace setup.
2. Shared types.
3. Core identity and storage key logic.
4. Unit tests for Resource ID, Result ID, Task ID, and prefix generation.

This creates the foundation for deduplication before adding HTTP, S3, image, or video processing.

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

- Add Rust implementation code without explicit Step 4 approval.
- Add a database dependency as a source of truth.
- Store media permanently on local disk.
- Scatter configuration across modules.
- Skip manifest design.
- Hide storage keys or link behavior inside unrelated modules.
- Treat SQLite as required for correctness.
