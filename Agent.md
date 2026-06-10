# Agent Guide

This file defines how AI agents must work on MediaForge.

MediaForge is a Rust-based media processing and delivery platform. The project prioritizes AI understandability, modular architecture, stateless operation, object-storage durability, and content-addressed media resources.

## 1. Mandatory Execution Protocol

Before starting any major step, the agent must clearly state:

1. The current step.
2. What files or outputs it will produce.
3. That it will not skip earlier required steps.

Required step order:

1. Step 1: Architecture Design.
2. Step 2: Documentation.
3. Step 3: Context Handoff.
4. Step 4: Implementation, only after explicit user approval.

Implementation code must not be written before Step 4.

## 2. Current Project Step Rules

Step 1 output:

- `docs/ARCHITECTURE.md`
- Overall system architecture.
- Module breakdown with responsibilities.
- Data flow.
- Key design decisions.

Step 2 output:

- `docs/SPEC.md`
- `docs/BUILD.md`
- `docs/EXTERNAL_DOCS.md`
- Root-level `Agent.md`

Step 3 output:

- `docs/nextsession.md`
- Current progress.
- Architecture summary.
- Completed parts.
- Pending tasks.
- Next actions.
- Risks and unknowns.

Step 4 output:

- Rust implementation code only after the user explicitly asks to implement.

## 3. Documentation Placement

All project documentation must live in `docs/`, except this root `Agent.md`.

Required documentation files:

- `docs/ARCHITECTURE.md`: architecture and module boundaries.
- `docs/SPEC.md`: product and system specification.
- `docs/BUILD.md`: build, usage, and deployment instructions.
- `docs/EXTERNAL_DOCS.md`: official documentation links for external integrations.
- `docs/nextsession.md`: handoff document for the next AI session.

If the project integrates another product, SDK, cloud service, framework, runtime, or external tool, add its official documentation link to `docs/EXTERNAL_DOCS.md`.

## 4. Git Workflow

After each major step:

```text
git add .
git commit -m "feat: <describe current step>"
```

Do not push unless the user explicitly asks.

Do not rewrite or discard user changes.

## 5. Architecture Principles

Optimize for AI comprehension over elegance.

Modules must be split by cognitive responsibility:

- A module should be understandable in isolation.
- A module should have one clear purpose.
- Inputs, outputs, and dependencies must be explicit.
- Avoid hidden side effects.
- Avoid hidden global state.
- Avoid large utility modules.
- Avoid deep inheritance or layered abstractions that hide behavior.
- Prefer composition and explicit data flow.
- Keep configuration centralized.

Naming is documentation:

- Use descriptive names.
- Avoid abbreviations such as `cfg`, `tmp`, and `svc`.
- Names must reflect purpose, not implementation convenience.

## 6. MediaForge System Constraints

MediaForge must remain stateless.

Durable data must live in S3-compatible object storage:

- Original images.
- Original videos.
- Derived media.
- HLS playlists and segments.
- Screenshots.
- Cover images.
- Manifest files.
- Task descriptors and task status.

The platform must not require:

- PostgreSQL.
- MySQL.
- MariaDB.
- Any traditional central database.

SQLite is allowed only as a disposable local cache. Deleting the SQLite file must not break correctness.

Local disk is temporary only:

- Upload buffering.
- FFmpeg work directories.
- libvips/image processing work files.
- Temporary downloads.

All temporary files must be cleaned after processing.

## 7. Core Technical Direction

Language:

- Rust.

Web runtime:

- Axum.
- Tokio.
- Tower.

Serialization:

- Serde.
- Serde JSON.

Object storage:

- AWS S3-compatible API.
- AWS SDK for Rust S3 client.

Image processing:

- libvips first.
- Rust `image` crate only for fallback or narrow operations.

Video processing:

- FFmpeg only.
- Rust must manage FFmpeg execution and parameters.
- Rust must not implement video encoders.

Hashing:

- SHA256 for Resource IDs and content addressing.
- MD5 only for compatibility checks where required by storage protocols.

Authentication and signing:

- JWT for API authentication when needed.
- HMAC for signed platform links.
- S3 presigned URLs where storage-provider presigning is required.

## 8. Required AI Behavior

When modifying the project:

1. Read `Agent.md`.
2. Read `docs/ARCHITECTURE.md`.
3. Read `docs/SPEC.md`.
4. Read `docs/nextsession.md` if continuing previous work.
5. Check `docs/EXTERNAL_DOCS.md` before using external APIs or libraries.
6. State the current step before acting.
7. Keep edits scoped to the requested step.

When complexity grows:

- Stop.
- Re-check module boundaries.
- Refactor the design before adding more behavior.

When unsure about external APIs:

- Use the official documentation link in `docs/EXTERNAL_DOCS.md`.
- Re-verify the current docs if behavior may have changed.
- Update `docs/EXTERNAL_DOCS.md` with the verified link and date.

## 9. Implementation Guardrails

No implementation code may be added until the user explicitly requests Step 4.

When Step 4 starts:

- Create the Rust workspace incrementally.
- Implement one module at a time.
- Add tests for each module boundary.
- Keep public types explicit.
- Keep configuration centralized in `mediaforge-config`.
- Keep storage access isolated in `mediaforge-storage`.
- Keep FFmpeg handling isolated in `mediaforge-video`.
- Keep image processing isolated in `mediaforge-image`.
- Keep task state isolated in `mediaforge-tasks`.

Each implementation step should be independently buildable and reviewable.

