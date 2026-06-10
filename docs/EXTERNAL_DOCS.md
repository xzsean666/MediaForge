# External Documentation Index

Last verified: 2026-06-10

This file stores official documentation links for external projects, SDKs, cloud services, and tools that MediaForge may integrate with.

Future AI sessions must check this file before using external APIs. If a link changes or a new integration is added, update this file and record the verification date.

## Rust Platform

| Integration | Purpose | Official docs |
| --- | --- | --- |
| Rust Book | Rust language reference for project contributors | https://doc.rust-lang.org/book/ |
| Cargo Book | Workspace, build, dependency, and packaging behavior | https://doc.rust-lang.org/cargo/ |
| Rust standard library | Standard library API reference | https://doc.rust-lang.org/std/ |

## Web API Runtime

| Integration | Purpose | Official docs |
| --- | --- | --- |
| Axum | HTTP API framework | https://docs.rs/axum/latest/axum/ |
| Tokio | Async runtime | https://docs.rs/tokio/latest/tokio/ |
| Tower | Middleware and service abstraction | https://docs.rs/tower/latest/tower/ |
| Tower HTTP | HTTP middleware utilities | https://docs.rs/tower-http/latest/tower_http/ |

## Serialization and Uploads

| Integration | Purpose | Official docs |
| --- | --- | --- |
| Serde | Rust serialization framework | https://serde.rs/ |
| serde_json | JSON serialization and deserialization | https://docs.rs/serde_json/latest/serde_json/ |
| Multer | Multipart upload parsing | https://docs.rs/multer/latest/multer/ |

## Object Storage and S3 Compatibility

| Integration | Purpose | Official docs |
| --- | --- | --- |
| AWS SDK for Rust | AWS SDK developer guide | https://docs.aws.amazon.com/sdk-for-rust/latest/dg/welcome.html |
| aws-sdk-s3 crate | Rust S3 client API | https://docs.rs/aws-sdk-s3/latest/aws_sdk_s3/ |
| Amazon S3 API Reference | Baseline S3 API behavior | https://docs.aws.amazon.com/AmazonS3/latest/API/Welcome.html |
| Amazon S3 PutObject | Conditional writes and object upload behavior | https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html |
| Amazon S3 presigned URLs | Temporary direct object access | https://docs.aws.amazon.com/AmazonS3/latest/userguide/PresignedUrlUploadObject.html |
| Cloudflare R2 S3 API | Cloudflare R2 S3-compatible behavior | https://developers.cloudflare.com/r2/api/s3/api/ |
| Cloudflare R2 presigned URLs | R2 temporary object access | https://developers.cloudflare.com/r2/api/s3/presigned-urls/ |
| Backblaze B2 S3-compatible API | Backblaze B2 S3 compatibility | https://www.backblaze.com/docs/cloud-storage-s3-compatible-api |
| MinIO S3 API compatibility | MinIO S3-compatible behavior | https://docs.min.io/aistor/developers/s3-api-compatibility/ |
| Wasabi S3 API Reference | Wasabi S3-compatible behavior | https://docs.wasabi.com/apidocs/wasabi-api |
| DigitalOcean Spaces API Reference | Spaces REST/S3-compatible API | https://docs.digitalocean.com/reference/api/spaces/ |
| DigitalOcean Spaces S3 compatibility | Spaces S3 feature compatibility | https://docs.digitalocean.com/products/spaces/reference/s3-compatibility/ |

## Image Processing

| Integration | Purpose | Official docs |
| --- | --- | --- |
| libvips API | Primary image processing engine | https://www.libvips.org/API/current/ |
| Rust libvips crate | Rust bindings for libvips | https://docs.rs/libvips/latest/libvips/ |
| Rust image crate | Fallback image operations | https://docs.rs/image/latest/image/ |

## Video Processing

| Integration | Purpose | Official docs |
| --- | --- | --- |
| FFmpeg documentation | Video transcoding, screenshots, HLS generation | https://ffmpeg.org/documentation.html |
| FFmpeg formats | Container and format behavior | https://ffmpeg.org/ffmpeg-formats.html |
| FFmpeg codecs | Codec behavior and availability | https://ffmpeg.org/ffmpeg-codecs.html |
| FFprobe documentation | Media metadata inspection | https://ffmpeg.org/ffprobe.html |

## Cache and Local Metadata

| Integration | Purpose | Official docs |
| --- | --- | --- |
| SQLite documentation | Optional local cache only | https://www.sqlite.org/docs.html |

## Hashing, Auth, and Signing

| Integration | Purpose | Official docs |
| --- | --- | --- |
| sha2 crate | SHA256 Resource IDs and result IDs | https://docs.rs/sha2/latest/sha2/ |
| md5 crate | MD5 compatibility only, not security-sensitive signing | https://docs.rs/md5/latest/md5/ |
| jsonwebtoken crate | JWT API authentication | https://docs.rs/jsonwebtoken/latest/jsonwebtoken/ |
| hmac crate | HMAC signed platform links | https://docs.rs/hmac/latest/hmac/ |

Security note:

- SHA256 should be the default for content addressing.
- MD5 must not be used for security-sensitive identity, signing, or authorization. Use MD5 only when required for protocol compatibility.

## Observability

| Integration | Purpose | Official docs |
| --- | --- | --- |
| tracing crate | Structured logs and spans | https://docs.rs/tracing/latest/tracing/ |

## Delivery, CDN, and Signed Links

| Integration | Purpose | Official docs |
| --- | --- | --- |
| Cloudflare Cache/CDN | CDN caching and delivery | https://developers.cloudflare.com/cache/ |
| Cloudflare Cache-Control behavior | Origin cache control and CDN behavior | https://developers.cloudflare.com/cache/concepts/cache-control/ |
| Bunny Developer Hub | Bunny CDN and platform docs | https://docs.bunny.net/ |
| Bunny token authentication | Bunny signed URL/token behavior | https://docs.bunny.net/cdn/security/token-authentication/basic |
| Amazon CloudFront private content | Signed URLs and signed cookies overview | https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/PrivateContent.html |
| Amazon CloudFront signed URLs | CloudFront signed URL details | https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/private-content-signed-urls.html |

## Deployment

| Integration | Purpose | Official docs |
| --- | --- | --- |
| Docker docs | Container build and runtime | https://docs.docker.com/ |
| Kubernetes docs | Cluster deployment and scaling | https://kubernetes.io/docs/home/ |

## Provider Compatibility Notes

S3-compatible providers are not identical. Before relying on a provider for task leases, presigned URLs, metadata behavior, multipart uploads, or conditional writes, verify the current provider-specific documentation.

Task claiming should prefer S3 conditional writes when available. If the provider does not support reliable conditional object creation, MediaForge must remain correct through deterministic object keys and idempotent manifest updates.

