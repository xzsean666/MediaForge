#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${ENV_FILE:-$ROOT_DIR/.env.test}"

if [[ ! -f "$ENV_FILE" ]]; then
  echo "missing env file: $ENV_FILE" >&2
  exit 1
fi

set -a
source "$ENV_FILE"
set +a

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "missing required environment variable: $name" >&2
    exit 1
  fi
}

require_command cargo
require_command curl
require_command ffmpeg
require_command jq

require_env B2_APPLICATION_KEY_ID
require_env B2_APPLICATION_KEY
require_env B2_BUCKET_NAME

WORK_DIR="$(mktemp -d /tmp/mediaforge-b2-e2e.XXXXXX)"
API_PORT="${MEDIAFORGE_E2E_PORT:-18180}"
API_URL="http://127.0.0.1:${API_PORT}"
API_PID=""
CURRENT_STEP="initializing"
E2E_CARGO_JOBS="${MEDIAFORGE_E2E_CARGO_JOBS:-1}"
E2E_NICE_LEVEL="${MEDIAFORGE_E2E_NICE_LEVEL:-15}"
E2E_FFMPEG_THREADS="${MEDIAFORGE_E2E_FFMPEG_THREADS:-1}"
MEDIAFORGE_BIN="$ROOT_DIR/target/debug/mediaforge"

on_exit() {
  local status="$?"
  if [[ -n "$API_PID" ]] && kill -0 "$API_PID" >/dev/null 2>&1; then
    kill "$API_PID" >/dev/null 2>&1 || true
    wait "$API_PID" >/dev/null 2>&1 || true
  fi

  if [[ "$status" -eq 0 && "${MEDIAFORGE_E2E_KEEP_WORK:-0}" != "1" ]]; then
    rm -rf "$WORK_DIR"
  elif [[ "$status" -ne 0 ]]; then
    echo "E2E failed during step: $CURRENT_STEP" >&2
    echo "Work directory preserved: $WORK_DIR" >&2
    if [[ -f "$WORK_DIR/api.log" ]]; then
      echo "API log tail:" >&2
      tail -n 80 "$WORK_DIR/api.log" >&2 || true
    fi
  fi

  exit "$status"
}
trap on_exit EXIT

run_low_priority() {
  nice -n "$E2E_NICE_LEVEL" "$@"
}

step() {
  CURRENT_STEP="$1"
  echo "==> $CURRENT_STEP"
}

curl_to_file() {
  local output_file="$1"
  shift

  local http_status
  http_status="$(curl -sS -w "%{http_code}" "$@" -o "$output_file")"
  if [[ ! "$http_status" =~ ^2 ]]; then
    echo "HTTP request failed with status $http_status during step: $CURRENT_STEP" >&2
    if [[ -f "$output_file" ]]; then
      echo "Response body:" >&2
      cat "$output_file" >&2 || true
      echo >&2
    fi
    return 1
  fi
}

authorize_b2() {
  local response
  response="$(curl -fsS -u "${B2_APPLICATION_KEY_ID}:${B2_APPLICATION_KEY}" \
    "https://api.backblazeb2.com/b2api/v4/b2_authorize_account" || true)"

  if [[ -z "$response" ]] || ! jq -e '.apiInfo.storageApi.s3ApiUrl' >/dev/null 2>&1 <<<"$response"; then
    response="$(curl -fsS -u "${B2_APPLICATION_KEY_ID}:${B2_APPLICATION_KEY}" \
      "https://api.backblazeb2.com/b2api/v3/b2_authorize_account")"
  fi

  printf '%s' "$response"
}

step "authorize Backblaze B2 and derive S3 endpoint"
AUTH_JSON="$(authorize_b2)"
S3_ENDPOINT="$(jq -r '.apiInfo.storageApi.s3ApiUrl' <<<"$AUTH_JSON")"
if [[ "$S3_ENDPOINT" == "null" || -z "$S3_ENDPOINT" ]]; then
  echo "Backblaze authorization did not return apiInfo.storageApi.s3ApiUrl" >&2
  exit 1
fi

S3_HOST="${S3_ENDPOINT#https://}"
S3_HOST="${S3_HOST#http://}"
S3_REGION="$(awk -F. '{print $2}' <<<"$S3_HOST")"
if [[ -z "$S3_REGION" || "$S3_REGION" == "$S3_HOST" ]]; then
  echo "could not derive S3 region from endpoint host" >&2
  exit 1
fi

export MEDIAFORGE_STORAGE_BACKEND=s3
export MEDIAFORGE_S3_ENDPOINT="$S3_ENDPOINT"
export MEDIAFORGE_S3_REGION="$S3_REGION"
export MEDIAFORGE_S3_BUCKET="$B2_BUCKET_NAME"
export MEDIAFORGE_S3_ACCESS_KEY_ID="$B2_APPLICATION_KEY_ID"
export MEDIAFORGE_S3_SECRET_ACCESS_KEY="$B2_APPLICATION_KEY"
export MEDIAFORGE_S3_FORCE_PATH_STYLE=true
export MEDIAFORGE_HTTP_BIND="127.0.0.1:${API_PORT}"
export MEDIAFORGE_TEMP_DIR="$WORK_DIR/temp"
export MEDIAFORGE_CDN_BASE_URL="${MEDIAFORGE_CDN_BASE_URL:-https://cdn.example.invalid}"
export MEDIAFORGE_LINK_SIGNING_SECRET="${MEDIAFORGE_LINK_SIGNING_SECRET:-mediaforge-e2e-secret}"
export MEDIAFORGE_WORKER_CONCURRENCY=1
export MEDIAFORGE_WORKER_POLL_INTERVAL_SECONDS=1
export MEDIAFORGE_FFMPEG_THREADS="$E2E_FFMPEG_THREADS"
export CARGO_BUILD_JOBS="$E2E_CARGO_JOBS"
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-1}"

IMAGE_PATH="$WORK_DIR/e2e-image.png"
VIDEO_PATH="$WORK_DIR/e2e-video.mp4"

step "build mediaforge CLI with limited Cargo jobs"
run_low_priority cargo build -j "$E2E_CARGO_JOBS" -p mediaforge-cli

step "generate synthetic image"
run_low_priority ffmpeg -hide_banner -loglevel error -threads "$E2E_FFMPEG_THREADS" -y \
  -f lavfi -i testsrc=size=320x180:rate=1 \
  -frames:v 1 "$IMAGE_PATH"

step "generate synthetic video"
run_low_priority ffmpeg -hide_banner -loglevel error -threads "$E2E_FFMPEG_THREADS" -y \
  -f lavfi -i testsrc=size=320x180:rate=24 \
  -f lavfi -i sine=frequency=880:sample_rate=44100 \
  -t 2 \
  -c:v libx264 -pix_fmt yuv420p \
  -c:a aac \
  -threads "$E2E_FFMPEG_THREADS" \
  "$VIDEO_PATH"

step "start API server"
run_low_priority "$MEDIAFORGE_BIN" api >"$WORK_DIR/api.log" 2>&1 &
API_PID="$!"

for _ in $(seq 1 60); do
  if curl -fsS "$API_URL/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

step "verify API health"
curl -fsS "$API_URL/health" | jq -e '.status == "ok"' >/dev/null

IMAGE_UPLOAD_JSON="$WORK_DIR/image-upload.json"
step "upload image to B2-backed API"
curl_to_file "$IMAGE_UPLOAD_JSON" -X POST "$API_URL/v1/images/upload" \
  -F "file=@${IMAGE_PATH};type=image/png"

IMAGE_RESOURCE_ID="$(jq -r '.resource_id' "$IMAGE_UPLOAD_JSON")"
jq -e '.manifest.media_kind == "image"' "$IMAGE_UPLOAD_JSON" >/dev/null

IMAGE_DUPLICATE_UPLOAD_JSON="$WORK_DIR/image-duplicate-upload.json"
step "verify duplicate image upload returns same resource"
curl_to_file "$IMAGE_DUPLICATE_UPLOAD_JSON" -X POST "$API_URL/v1/images/upload" \
  -F "file=@${IMAGE_PATH};type=image/png"
jq -e '.resource_id == "'"$IMAGE_RESOURCE_ID"'"' "$IMAGE_DUPLICATE_UPLOAD_JSON" >/dev/null

IMAGE_QUERY_JSON="$WORK_DIR/image-query.json"
step "query image manifest"
curl_to_file "$IMAGE_QUERY_JSON" "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}"
jq -e '.resource_id == "'"$IMAGE_RESOURCE_ID"'"' "$IMAGE_QUERY_JSON" >/dev/null

IMAGE_PUBLIC_LINK_JSON="$WORK_DIR/image-public-link.json"
step "generate public image link"
curl_to_file "$IMAGE_PUBLIC_LINK_JSON" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/links" \
  -H "content-type: application/json" \
  -d '{"policy":{"type":"public"}}'
jq -e '.link_kind == "public" and (.url | length > 0)' "$IMAGE_PUBLIC_LINK_JSON" >/dev/null

IMAGE_STORAGE_LINK_JSON="$WORK_DIR/image-storage-link.json"
step "generate B2 presigned image link"
curl_to_file "$IMAGE_STORAGE_LINK_JSON" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/links" \
  -H "content-type: application/json" \
  -d '{"policy":{"type":"storage_presigned","expires_in_seconds":300}}'
jq -e '.link_kind == "storage_presigned" and (.url | contains("X-Amz-Signature"))' "$IMAGE_STORAGE_LINK_JSON" >/dev/null

IMAGE_TASK_JSON="$WORK_DIR/image-task.json"
step "create image processing task"
curl_to_file "$IMAGE_TASK_JSON" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/image" \
  -H "content-type: application/json" \
  -d '{"output_format":"webp","quality":80,"operations":[{"type":"resize","width":160,"height":90,"fit":"fill"},{"type":"rotate","degrees":"deg90"}]}'
IMAGE_TASK_ID="$(jq -r '.task_id' "$IMAGE_TASK_JSON")"

step "process image task"
run_low_priority "$MEDIAFORGE_BIN" process-task "$IMAGE_TASK_ID" --worker-id e2e-image-worker >/dev/null

IMAGE_AFTER_TASK_JSON="$WORK_DIR/image-after-task.json"
step "verify image derivative manifest"
curl_to_file "$IMAGE_AFTER_TASK_JSON" "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}"
jq -e '[.derived[] | select(.derived_kind == "image_variant")] | length >= 1' "$IMAGE_AFTER_TASK_JSON" >/dev/null

VIDEO_UPLOAD_JSON="$WORK_DIR/video-upload.json"
step "upload video to B2-backed API"
curl_to_file "$VIDEO_UPLOAD_JSON" -X POST "$API_URL/v1/videos/upload" \
  -F "file=@${VIDEO_PATH};type=video/mp4"

VIDEO_RESOURCE_ID="$(jq -r '.resource_id' "$VIDEO_UPLOAD_JSON")"
VIDEO_DEFAULT_TASK_ID="$(jq -r '.task_id' "$VIDEO_UPLOAD_JSON")"
jq -e '.manifest.media_kind == "video"' "$VIDEO_UPLOAD_JSON" >/dev/null

VIDEO_DUPLICATE_UPLOAD_JSON="$WORK_DIR/video-duplicate-upload.json"
step "verify duplicate video upload returns same resource and task"
curl_to_file "$VIDEO_DUPLICATE_UPLOAD_JSON" -X POST "$API_URL/v1/videos/upload" \
  -F "file=@${VIDEO_PATH};type=video/mp4"
jq -e '.resource_id == "'"$VIDEO_RESOURCE_ID"'" and .task_id == "'"$VIDEO_DEFAULT_TASK_ID"'"' "$VIDEO_DUPLICATE_UPLOAD_JSON" >/dev/null

step "process default video task"
run_low_priority "$MEDIAFORGE_BIN" process-task "$VIDEO_DEFAULT_TASK_ID" --worker-id e2e-video-worker >/dev/null

VIDEO_HLS_TASK_JSON="$WORK_DIR/video-hls-task.json"
step "create HLS video task"
curl_to_file "$VIDEO_HLS_TASK_JSON" -X POST "$API_URL/v1/resources/${VIDEO_RESOURCE_ID}/video" \
  -H "content-type: application/json" \
  -d '{"profile":{"codec":"h264","container":"mp4","resolution":"p360","crf":28,"bitrate_kbps":null},"screenshots":[{"timestamp_seconds":0.5,"output_format":"jpeg"}],"generate_cover":true,"generate_hls":true}'
VIDEO_HLS_TASK_ID="$(jq -r '.task_id' "$VIDEO_HLS_TASK_JSON")"

step "process HLS video task"
run_low_priority "$MEDIAFORGE_BIN" process-task "$VIDEO_HLS_TASK_ID" --worker-id e2e-hls-worker >/dev/null

VIDEO_AFTER_TASK_JSON="$WORK_DIR/video-after-task.json"
step "verify video derivative manifest"
curl_to_file "$VIDEO_AFTER_TASK_JSON" "$API_URL/v1/resources/${VIDEO_RESOURCE_ID}"
jq -e '[.derived[] | select(.derived_kind == "video_variant")] | length >= 1' "$VIDEO_AFTER_TASK_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "cover")] | length >= 1' "$VIDEO_AFTER_TASK_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "screenshot")] | length >= 1' "$VIDEO_AFTER_TASK_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "hls_playlist")] | length >= 1' "$VIDEO_AFTER_TASK_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "hls_segment")] | length >= 1' "$VIDEO_AFTER_TASK_JSON" >/dev/null

VIDEO_TASK_STATUS_JSON="$WORK_DIR/video-task-status.json"
step "verify video task status"
curl_to_file "$VIDEO_TASK_STATUS_JSON" "$API_URL/v1/tasks/${VIDEO_HLS_TASK_ID}"
jq -e '.state == "completed"' "$VIDEO_TASK_STATUS_JSON" >/dev/null

SUMMARY_JSON="$WORK_DIR/summary.json"
jq -n \
  --arg image_resource_id "$IMAGE_RESOURCE_ID" \
  --arg image_task_id "$IMAGE_TASK_ID" \
  --arg video_resource_id "$VIDEO_RESOURCE_ID" \
  --arg video_default_task_id "$VIDEO_DEFAULT_TASK_ID" \
  --arg video_hls_task_id "$VIDEO_HLS_TASK_ID" \
  --arg endpoint_host "$S3_HOST" \
  '{
    status: "ok",
    storage: "backblaze_b2_s3",
    endpoint_host: $endpoint_host,
    image_resource_id: $image_resource_id,
    image_task_id: $image_task_id,
    video_resource_id: $video_resource_id,
    video_default_task_id: $video_default_task_id,
    video_hls_task_id: $video_hls_task_id
  }' >"$SUMMARY_JSON"

cat "$SUMMARY_JSON"
