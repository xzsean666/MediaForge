# MediaForge Specification

Version: 1.0

This document defines the product and system specification for MediaForge.

Architecture details are defined in `docs/ARCHITECTURE.md`. External integration links are defined in `docs/EXTERNAL_DOCS.md`.

## 1. Product Goal

MediaForge is a Rust-based media processing and delivery platform.

It provides:

- Image upload.
- Image compression.
- Image format conversion.
- Image watermarking.
- Image resizing.
- Image cropping.
- Image rotation.
- Video upload.
- Video transcoding.
- Video compression.
- Video screenshots.
- Video cover generation.
- HLS stream generation.
- Media resource delivery.
- Permanent public links.
- Temporary access links.
- Signed access links.
- API-based media resource queries.

MediaForge does not provide long-term local storage.

All durable media assets must be stored in user-configured S3-compatible object storage.

## 2. Non-Goals

MediaForge must not:

- Store media permanently on local disk.
- Require PostgreSQL, MySQL, MariaDB, or another central database.
- Depend on local process memory for correctness.
- Implement image or video codecs from scratch.
- Expose bucket names, object keys, or storage prefixes as the primary client contract.
- Require clients to understand object storage layout.

## 3. System Properties

The system must be:

- Stateless.
- Horizontally scalable.
- Multi-instance deployable.
- Cloud-native.
- Content-addressed.
- Object-storage-driven.
- Idempotent for repeated uploads and processing requests.
- Safe to run as API-only, worker-only, or combined mode.

## 4. Storage Model

Object storage is the only durable source of truth.

Durable objects include:

- Original media files.
- Processed image outputs.
- Processed video outputs.
- HLS playlists and segments.
- Screenshots.
- Cover images.
- Manifest files.
- Task descriptors.
- Task status files.

Local disk may be used only for:

- Upload buffering.
- Temporary media processing.
- FFmpeg work directories.
- Temporary downloads from object storage.

Local temporary files must be deleted after use.

## 5. Resource Identity

Every uploaded asset must receive a Resource ID derived from file content.

Primary Resource ID format:

```text
sha256:<hex_hash>
```

Rules:

- Same bytes produce the same Resource ID.
- Different bytes produce different Resource IDs.
- Resource ID is independent from bucket name and object key.
- API clients use Resource ID as the stable media identifier.

Derived result identity must include:

- Source Resource ID.
- Operation type.
- Canonical processing parameters.

This guarantees that the same source and same parameters reuse the same result.

## 6. Deduplication Requirements

Image deduplication:

- Same image bytes must not be uploaded twice as separate originals.
- Same source image and same processing parameters must reuse existing derivatives.

Video deduplication:

- Same video bytes must not be uploaded twice as separate originals.
- Same source video and same transcoding, screenshot, cover, or HLS parameters must reuse existing outputs.

Task deduplication:

- Same source resource and same normalized task parameters should map to the same deterministic Task ID.
- Repeated task creation should return existing pending, running, or completed task state.

## 7. Manifest Requirements

Each resource group must have a manifest stored in object storage.

Manifest file:

```text
media/sha256/<prefix_1>/<prefix_2>/<resource_id>/manifest.json
```

The manifest must describe:

- Resource ID.
- Media kind: image or video.
- Original file metadata.
- Derived file metadata.
- MIME type.
- File size.
- Width and height when available.
- Duration when available.
- Codec and bitrate when available.
- Created time.
- Processing parameters.
- Task references.

API queries should read manifests first.

The API should not require direct object storage access by the caller.

## 8. Image Processing Requirements

Supported input formats:

- JPG.
- JPEG.
- PNG.
- WEBP.
- GIF.
- AVIF.

Supported output formats:

- JPG.
- PNG.
- WEBP.
- AVIF.

Required operations:

- Upload.
- Compress.
- Convert format.
- Resize.
- Crop.
- Rotate.
- Add image watermark.
- Add text watermark.

Image watermark requirements:

- PNG watermark support.
- SVG watermark support when supported by the selected image backend.
- Position control.
- Opacity control.

Text watermark requirements:

- Text content.
- Font size.
- Color.
- Opacity.
- Position.

Implementation direction:

- Prefer libvips for image processing.
- Use Rust `image` crate only for fallback or focused operations.

## 9. Video Processing Requirements

Supported input formats:

- MP4.
- MOV.
- AVI.
- MKV.
- WEBM.
- MPEG.

Supported output formats:

- H264 MP4.
- H265 MP4.
- AV1 MP4.
- AV1 MKV.

Supported resolutions:

- 360P.
- 480P.
- 720P.
- 1080P.
- 1440P.
- 4K.

Required operations:

- Upload.
- Transcode.
- Compress.
- Generate single screenshot.
- Generate multiple screenshots.
- Generate screenshot at requested timestamp.
- Generate cover image.
- Generate HLS output.

HLS output must include:

- `m3u8` playlists.
- `ts` segments by default.
- VOD playback support.
- Browser playback compatibility.
- Mobile playback compatibility.

Implementation direction:

- Use FFmpeg as the encoding and media analysis engine.
- Rust validates requests, builds safe command arguments, manages process execution, and captures metadata.
- Rust must not implement codecs.

## 10. API Capability Requirements

The API must support:

- Upload image.
- Upload video.
- Create image processing request.
- Create video processing task.
- Query resource by Resource ID.
- Query task by Task ID.
- Generate public media links.
- Generate temporary media links.
- Generate signed media links.

API return rules:

- Return Resource ID for completed resource operations.
- Return Task ID for asynchronous video work.
- Do not require callers to understand buckets, object keys, or prefixes.
- Return manifest-derived metadata.
- Return explicit error responses for unsupported formats, invalid parameters, authorization failures, and missing resources.

## 11. Link Requirements

Permanent public links:

- Used for public resources.
- Compatible with configured CDN base URL.

Temporary links:

- Must support 5 minutes.
- Must support 15 minutes.
- Must support 1 hour.
- Must support 24 hours.
- Must support custom expiration.

Signed links:

- Must include expiration.
- Must protect against tampering.
- Must validate permissions.
- Should use HMAC for platform links.
- May use S3 presigning for direct object storage links.

CDN compatibility:

- Cloudflare CDN.
- Bunny CDN.
- Amazon CloudFront.
- Custom CDN base URL.

## 12. Asynchronous Task Requirements

Video processing must use asynchronous task mode.

Required flow:

```text
Upload
  -> Create task
  -> Worker claims task
  -> Worker processes media
  -> Worker uploads outputs
  -> Worker writes manifest
  -> API returns Resource ID and task status
```

Task state must be durable in object storage.

Task execution must be idempotent.

Exact-once execution should not be assumed across all S3-compatible providers. If a provider does not support reliable conditional object creation, duplicate work may occur, but duplicate final outputs must converge to the same Resource ID, Result ID, and manifest state.

## 13. Configuration Requirements

Configuration must be centralized.

Configuration sources:

- Environment variables.
- Optional configuration file in a future implementation.

Configuration categories:

- Server bind address and port.
- Runtime mode: `api`, `worker`, or `combined`.
- S3 endpoint, region, bucket, credentials, and path-style setting.
- CDN base URL.
- Signing secrets.
- Temporary directory root.
- FFmpeg executable path.
- libvips availability and image backend selection.
- Worker polling and concurrency settings.
- Optional SQLite cache path.
- Logging level.

No module except configuration may read environment variables directly.

## 14. Security Requirements

API security:

- Support JWT authentication when enabled.
- Validate request size limits.
- Validate MIME types and file signatures where practical.
- Reject unsupported formats.
- Normalize processing parameters before use.

Link security:

- HMAC signatures must include expiration.
- Signed links must reject expired timestamps.
- Signed links must reject modified path, resource, or permission fields.

Processing security:

- FFmpeg arguments must be built from typed values.
- Raw request strings must not be interpolated into shell commands.
- Prefer direct process invocation over shell execution.
- Temporary directories must be isolated per request or task.

Storage security:

- Credentials must come from configuration.
- Do not log secrets.
- Do not expose bucket internals in public API responses.

## 15. Reliability Requirements

The system must:

- Treat uploads and task processing as retryable.
- Avoid corrupting manifests during retries.
- Make duplicate processing safe.
- Clean temporary files after success or failure.
- Report structured errors.
- Log Resource ID and Task ID when available.

## 16. Performance Requirements

The system should:

- Stream uploads where possible.
- Compute hashes during streaming when possible.
- Avoid loading large files fully into memory.
- Use libvips for memory-efficient image processing.
- Use FFmpeg for video processing.
- Support worker concurrency limits.
- Support multipart upload for large objects when needed.

## 17. Deployment Requirements

Supported deployment models:

- Single binary local deployment.
- Docker deployment.
- Kubernetes deployment.
- Multi-instance API deployment.
- Multi-instance worker deployment.
- Combined API and worker deployment for small installations.

Any instance must be able to:

- Query resources.
- Create tasks.
- Process tasks.

Correctness must not depend on which instance handled the request.

## 18. Future Extension Targets

Future features may include:

- OCR.
- AI background removal.
- AI image enhancement.
- Video subtitle extraction.
- Audio transcoding.
- Audio extraction.
- Content moderation.
- Multi-tenancy.
- Usage statistics.
- Webhook callbacks.

Future modules must follow the same AI-oriented modularity rules defined in `Agent.md`.

## 19. Acceptance Criteria

The project is acceptable when:

- Resource IDs are content-addressed.
- Object storage contains all durable state.
- API queries work from manifests.
- Duplicate uploads return existing resources.
- Duplicate processing requests reuse existing results.
- Video processing uses asynchronous tasks.
- Temporary local files are cleaned.
- API, worker, and combined modes are supported.
- No traditional database is required.
- External integration docs are tracked in `docs/EXTERNAL_DOCS.md`.

