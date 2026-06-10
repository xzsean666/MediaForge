# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=rust:1.81-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-slim

FROM --platform=$TARGETPLATFORM ${RUST_IMAGE} AS builder

ARG CARGO_BUILD_JOBS=1
ARG TARGETARCH
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}

WORKDIR /app

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        libssl-dev \
        libvips-dev \
        nasm \
        perl \
        pkg-config; \
    rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,id=mediaforge-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,id=mediaforge-cargo-git-${TARGETARCH},target=/usr/local/cargo/git \
    --mount=type=cache,id=mediaforge-target-${TARGETARCH},target=/app/target \
    set -eux; \
    cargo build --release --locked -j "${CARGO_BUILD_JOBS}" -p mediaforge-cli; \
    cp /app/target/release/mediaforge /usr/local/bin/mediaforge

FROM --platform=$TARGETPLATFORM ${DEBIAN_IMAGE} AS runtime

ARG TARGETARCH
LABEL org.opencontainers.image.title="MediaForge"
LABEL org.opencontainers.image.description="Rust media processing and delivery platform"

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        ffmpeg \
        libvips-tools \
        libvips42 \
        tini \
        tzdata; \
    rm -rf /var/lib/apt/lists/*; \
    groupadd --system --gid 10001 mediaforge; \
    useradd --system --uid 10001 --gid mediaforge --home-dir /var/lib/mediaforge mediaforge; \
    mkdir -p /var/lib/mediaforge /tmp/mediaforge /var/lib/mediaforge/object-store; \
    chown -R mediaforge:mediaforge /var/lib/mediaforge /tmp/mediaforge

COPY --from=builder /usr/local/bin/mediaforge /usr/local/bin/mediaforge

ENV MEDIAFORGE_HTTP_BIND=0.0.0.0:8080 \
    MEDIAFORGE_TEMP_DIR=/tmp/mediaforge \
    MEDIAFORGE_FFMPEG_PATH=ffmpeg \
    MEDIAFORGE_FFPROBE_PATH=ffprobe \
    MEDIAFORGE_FFMPEG_THREADS=1 \
    MEDIAFORGE_WORKER_CONCURRENCY=1 \
    MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS=5 \
    MEDIAFORGE_LOG_LEVEL=info

WORKDIR /var/lib/mediaforge
USER mediaforge:mediaforge

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health >/dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/mediaforge"]
CMD ["combined"]

