# Prebuilt Binary

This directory is used by `Dockerfile.prebuilt` and `Dockerfile.prebuilt.cn`.

Generate the binary with:

```bash
scripts/build-prebuilt-binary.sh
```

Default output paths:

```text
deploy/prebuilt/amd64/mediaforge
deploy/prebuilt/arm64/mediaforge
```

The generated binaries are intentionally ignored by git.

For multi-architecture prebuilt images, provide a Linux binary for each Docker architecture before running `docker buildx build --platform linux/amd64,linux/arm64`.

