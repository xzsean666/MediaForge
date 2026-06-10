# MediaForge Rust Docker Deployment

Version: 1.0

This document describes the Docker deployment files generated for MediaForge.

MediaForge is a Rust workspace. The deployable binary is:

```text
mediaforge
```

The binary is built by the workspace package:

```text
crates/mediaforge-cli
```

The container supports these runtime commands:

```text
mediaforge api
mediaforge worker
mediaforge combined
mediaforge process-task <task_id>
```

The default Docker command is:

```text
mediaforge combined
```

## 1. Generated Files

Docker build files:

```text
Dockerfile
Dockerfile.prebuilt
Dockerfile.prebuilt.cn
Dockerfile.prebuilt.dockerignore
Dockerfile.prebuilt.cn.dockerignore
.dockerignore
```

Compose files:

```text
docker-compose.yml
docker-compose.prebuilt.yml
docker-compose.prebuilt.cn.yml
```

Support files:

```text
.env.example
scripts/build-prebuilt-binary.sh
deploy/prebuilt/README.md
```

## 2. Runtime Dependencies Installed In Docker

The runtime image installs:

- `ca-certificates`: TLS certificate trust store for S3-compatible storage and HTTPS APIs.
- `curl`: healthcheck command.
- `ffmpeg`: video transcoding, screenshots, covers, and HLS generation.
- `ffprobe`: provided by FFmpeg package and used for media metadata inspection.
- `libvips42`: libvips runtime library for the preferred future image backend.
- `libvips-tools`: libvips command-line tools for runtime diagnostics.
- `tini`: minimal init process for signal handling.
- `tzdata`: timezone data for timestamps and logs.

The source-build Dockerfile builder stage installs:

- `build-essential`
- `clang`
- `cmake`
- `curl`
- `libssl-dev`
- `libvips-dev`
- `nasm`
- `perl`
- `pkg-config`
- `ca-certificates`

These cover Rust native dependency builds, AWS LC build tooling, future libvips bindings, and media-related native packages.

## 3. Architecture Support

The Docker setup is designed for:

```text
linux/amd64
linux/arm64
```

Recommended multi-architecture source build:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f Dockerfile \
  -t your-registry/mediaforge:latest \
  --push \
  .
```

Local single-architecture source build:

```bash
docker build \
  -f Dockerfile \
  -t mediaforge:local \
  .
```

Notes:

- `Dockerfile` uses target-platform builder and runtime stages, so Buildx can build both amd64 and arm64 images.
- Multi-platform source builds may use QEMU emulation depending on the builder environment.
- `CARGO_BUILD_JOBS` defaults to `1` to avoid starving the host machine.

## 4. Standard Source Build Image

Use:

```bash
docker build \
  -f Dockerfile \
  --build-arg CARGO_BUILD_JOBS=1 \
  -t mediaforge:local \
  .
```

Run with local filesystem storage:

```bash
docker run --rm \
  -p 8080:8080 \
  --env-file .env.example \
  mediaforge:local
```

Health check:

```bash
curl -fsS http://127.0.0.1:8080/health
```

Expected response:

```json
{"status":"ok"}
```

## 5. Production S3 Configuration

Production deployments should use S3-compatible object storage:

```text
MEDIAFORGE_STORAGE_BACKEND=s3
MEDIAFORGE_S3_ENDPOINT=https://example-s3-endpoint
MEDIAFORGE_S3_REGION=auto
MEDIAFORGE_S3_BUCKET=mediaforge
MEDIAFORGE_S3_ACCESS_KEY_ID=replace-me
MEDIAFORGE_S3_SECRET_ACCESS_KEY=replace-me
MEDIAFORGE_S3_FORCE_PATH_STYLE=true
```

Do not bake secrets into the image.

Use one of:

- Docker `--env-file`.
- Docker Compose `env_file`.
- Kubernetes Secret.
- Your platform's secret manager.

## 6. Docker Compose

Local source-build compose:

```bash
docker compose up -d --build mediaforge
```

Use a different env file:

```bash
MEDIAFORGE_ENV_FILE=.env.production \
docker compose --env-file .env.production up -d --build mediaforge
```

The service publishes:

```text
${MEDIAFORGE_PORT:-8080}:8080
```

The compose files mount:

```text
/tmp/mediaforge
/var/lib/mediaforge/object-store
```

The object-store volume is only for local filesystem storage. Production S3 deployments can leave it unused.

## 7. Prebuilt Binary Image

Use this path when CI or the host machine builds the Rust binary before Docker packaging.

Build the current host architecture binary:

```bash
scripts/build-prebuilt-binary.sh
```

Default output:

```text
deploy/prebuilt/amd64/mediaforge
```

or:

```text
deploy/prebuilt/arm64/mediaforge
```

Build the prebuilt image:

```bash
docker build \
  -f Dockerfile.prebuilt \
  -t mediaforge:prebuilt \
  .
```

Run:

```bash
docker run --rm \
  -p 8080:8080 \
  --env-file .env.example \
  mediaforge:prebuilt
```

Important:

- The prebuilt binary must be a Linux glibc binary.
- The binary architecture must match the Docker target architecture.
- macOS or Windows host binaries cannot run inside the Debian runtime image.

## 8. Multi-Architecture Prebuilt Images

For multi-architecture prebuilt images, prepare both binaries first:

```text
deploy/prebuilt/amd64/mediaforge
deploy/prebuilt/arm64/mediaforge
```

Then build:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f Dockerfile.prebuilt \
  -t your-registry/mediaforge:prebuilt \
  --push \
  .
```

`Dockerfile.prebuilt` and `Dockerfile.prebuilt.cn` use Docker's `TARGETARCH` build argument to select:

```text
deploy/prebuilt/${TARGETARCH}/mediaforge
```

Supported values:

```text
TARGETARCH=amd64
TARGETARCH=arm64
```

If cross-compiling outside Docker, use:

```bash
MEDIAFORGE_CARGO_TARGET=x86_64-unknown-linux-gnu scripts/build-prebuilt-binary.sh
MEDIAFORGE_CARGO_TARGET=aarch64-unknown-linux-gnu scripts/build-prebuilt-binary.sh
```

Cross-compilation requires the Rust target and native toolchain for the selected architecture.

## 9. China Network Optimized Image

Use this when the deployment server has poor access to Docker Hub or Debian package mirrors.

Build the binary before packaging:

```bash
scripts/build-prebuilt-binary.sh
```

Build the China-optimized runtime image:

```bash
docker build \
  -f Dockerfile.prebuilt.cn \
  --build-arg DEBIAN_IMAGE=m.daocloud.io/docker.io/library/debian:bookworm-slim \
  --build-arg DEBIAN_MIRROR=http://mirrors.aliyun.com/debian \
  --build-arg DEBIAN_SECURITY_MIRROR=http://mirrors.aliyun.com/debian-security \
  -t mediaforge:prebuilt-cn \
  .
```

Compose:

```bash
docker compose -f docker-compose.prebuilt.cn.yml up -d --build mediaforge
```

Override mirrors:

```bash
MEDIAFORGE_DEBIAN_IMAGE=m.daocloud.io/docker.io/library/debian:bookworm-slim \
MEDIAFORGE_DEBIAN_MIRROR=http://mirrors.aliyun.com/debian \
MEDIAFORGE_DEBIAN_SECURITY_MIRROR=http://mirrors.aliyun.com/debian-security \
docker compose -f docker-compose.prebuilt.cn.yml up -d --build mediaforge
```

## 10. CPU Controls

Local development defaults are intentionally conservative:

```text
.cargo/config.toml -> build.jobs = 1
CARGO_BUILD_JOBS=1
MEDIAFORGE_FFMPEG_THREADS=1
MEDIAFORGE_WORKER_CONCURRENCY=1
```

Docker build override:

```bash
docker build \
  -f Dockerfile \
  --build-arg CARGO_BUILD_JOBS=2 \
  -t mediaforge:local \
  .
```

Compose override:

```bash
MEDIAFORGE_DOCKER_CARGO_JOBS=2 \
MEDIAFORGE_FFMPEG_THREADS=2 \
MEDIAFORGE_WORKER_CONCURRENCY=2 \
docker compose up -d --build mediaforge
```

Keep low values on development machines to avoid starving the IDE.

## 11. Runtime Modes In Docker

Default combined mode:

```bash
docker run --rm -p 8080:8080 --env-file .env.example mediaforge:local
```

API only:

```bash
docker run --rm -p 8080:8080 --env-file .env.example mediaforge:local api
```

Worker only:

```bash
docker run --rm --env-file .env.production mediaforge:local worker
```

Process one task:

```bash
docker run --rm --env-file .env.production mediaforge:local process-task <task_id>
```

## 12. Validation Commands

Run syntax and local checks:

```bash
bash -n scripts/build-prebuilt-binary.sh
docker compose config
docker compose -f docker-compose.prebuilt.yml config
docker compose -f docker-compose.prebuilt.cn.yml config
```

Build checks when Docker daemon is available:

```bash
docker build -f Dockerfile -t mediaforge:local .
scripts/build-prebuilt-binary.sh
docker build -f Dockerfile.prebuilt -t mediaforge:prebuilt .
docker build -f Dockerfile.prebuilt.cn -t mediaforge:prebuilt-cn .
```

Multi-architecture check:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f Dockerfile \
  -t your-registry/mediaforge:latest \
  --push \
  .
```

## 13. Troubleshooting

Binary does not run:

- Verify `file deploy/prebuilt/<arch>/mediaforge`.
- Verify architecture matches the image target.
- Verify the binary is Linux glibc, not macOS or Windows.
- Run `ldd /usr/local/bin/mediaforge` inside the container.

Healthcheck fails:

- Confirm the app listens on `0.0.0.0:8080`.
- Confirm `MEDIAFORGE_HTTP_BIND=0.0.0.0:8080`.
- Confirm `/health` returns HTTP 200.
- Check container logs.

FFmpeg fails:

- Run `docker run --rm mediaforge:local ffmpeg -version` by overriding entrypoint if needed.
- Check codec availability in the Debian FFmpeg build.
- Keep `MEDIAFORGE_FFMPEG_THREADS` explicit.

libvips issues:

- Runtime image installs `libvips42` and `libvips-tools`.
- Builder image installs `libvips-dev`.
- The current MVP still uses the Rust `image` crate for implemented image transforms. libvips is installed for the planned preferred backend.

S3 provider compatibility:

- Some S3-compatible providers do not implement conditional writes.
- MediaForge already falls back from unsupported `If-None-Match` writes to deterministic key checks.
- Durable state must still live in S3-compatible object storage for production.
