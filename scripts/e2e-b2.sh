#!/usr/bin/env bash
set -euo pipefail

# End-to-end test against Backblaze B2 (S3-compatible) exercising the presigned
# direct-to-storage upload flow:
#   presign -> client PUTs straight to S3 -> complete -> worker finalizes
#   (download, hash, content-address, manifest) -> processing -> derivatives.
#
# Runs `mediaforge combined` so the worker loop drains the finalize and task
# queues automatically. By default it also starts a second auth-enabled API
# instance to verify JWT gating; set MEDIAFORGE_E2E_AUTH=0 to skip that section.

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

port_is_listening() {
  local port="$1"
  command -v ss >/dev/null 2>&1 && ss -ltn "sport = :${port}" | grep -q LISTEN
}

pick_api_port() {
  if [[ -n "${MEDIAFORGE_E2E_PORT:-}" ]]; then
    printf '%s\n' "$MEDIAFORGE_E2E_PORT"
    return
  fi

  local port
  for _ in $(seq 1 100); do
    port="$((18000 + (RANDOM % 1000) * 2))"
    if ! port_is_listening "$port" && ! port_is_listening "$((port + 1))"; then
      printf '%s\n' "$port"
      return
    fi
  done

  echo "could not find a free local API port" >&2
  exit 1
}

require_command cargo
require_command curl
require_command ffmpeg
require_command jq

# Provider controls only how the S3 endpoint/region are resolved below.
PROVIDER="${MEDIAFORGE_E2E_PROVIDER:-s3}"
require_env MEDIAFORGE_S3_BUCKET
require_env MEDIAFORGE_S3_ACCESS_KEY_ID
require_env MEDIAFORGE_S3_SECRET_ACCESS_KEY

WORK_DIR="$(mktemp -d /tmp/mediaforge-b2-e2e.XXXXXX)"
API_PORT="$(pick_api_port)"
API_URL="http://127.0.0.1:${API_PORT}"
API_PID=""
CURRENT_STEP="initializing"
E2E_CARGO_JOBS="${MEDIAFORGE_E2E_CARGO_JOBS:-1}"
E2E_NICE_LEVEL="${MEDIAFORGE_E2E_NICE_LEVEL:-15}"
E2E_FFMPEG_THREADS="${MEDIAFORGE_E2E_FFMPEG_THREADS:-1}"
MEDIAFORGE_BIN="$ROOT_DIR/target/debug/mediaforge"

ensure_no_existing_mediaforge() {
  if [[ "${MEDIAFORGE_E2E_ALLOW_EXISTING_MEDIAFORGE:-0}" == "1" ]]; then
    return
  fi

  local existing_pids
  existing_pids="$(pgrep -f "$MEDIAFORGE_BIN" || true)"
  if [[ -n "$existing_pids" ]]; then
    echo "existing MediaForge process(es) would share the same object-storage queues:" >&2
    ps -fp $existing_pids >&2 || true
    echo "stop them first, or set MEDIAFORGE_E2E_ALLOW_EXISTING_MEDIAFORGE=1 if this is intentional" >&2
    exit 1
  fi
}

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

ensure_no_existing_mediaforge

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

curl_status_to_file() {
  local output_file="$1" expected_status="$2"
  shift 2

  local http_status
  http_status="$(curl -sS -w "%{http_code}" "$@" -o "$output_file")"
  if [[ "$http_status" != "$expected_status" ]]; then
    echo "HTTP request returned status $http_status, expected $expected_status during step: $CURRENT_STEP" >&2
    if [[ -f "$output_file" ]]; then
      echo "Response body:" >&2
      cat "$output_file" >&2 || true
      echo >&2
    fi
    return 1
  fi
}

# Polls an upload session until it finalizes; leaves the JSON body in $2.
poll_upload() {
  local upload_id="$1" out="$2"
  local state
  for _ in $(seq 1 120); do
    curl_to_file "$out" "$API_URL/v1/uploads/${upload_id}"
    state="$(jq -r '.state' "$out")"
    case "$state" in
      completed) return 0 ;;
      failed)
        echo "upload $upload_id failed: $(jq -r '.error' "$out")" >&2
        return 1
        ;;
    esac
    sleep 1
  done
  echo "upload $upload_id did not finalize in time" >&2
  return 1
}

poll_task_state() {
  local task_id="$1" expected_state="$2" out="$3"
  local state
  for _ in $(seq 1 180); do
    curl_to_file "$out" "$API_URL/v1/tasks/${task_id}"
    state="$(jq -r '.state' "$out")"
    if [[ "$state" == "$expected_state" ]]; then
      return 0
    fi
    if [[ "$state" == "completed" || "$state" == "failed" ]]; then
      echo "task $task_id reached terminal state $state, expected $expected_state" >&2
      cat "$out" >&2 || true
      echo >&2
      return 1
    fi
    sleep 1
  done
  echo "task $task_id did not reach $expected_state in time" >&2
  return 1
}

# Polls a task until it reaches a terminal state; leaves the JSON body in $2.
poll_task() {
  local task_id="$1" out="$2"
  local state
  for _ in $(seq 1 180); do
    curl_to_file "$out" "$API_URL/v1/tasks/${task_id}"
    state="$(jq -r '.state' "$out")"
    case "$state" in
      completed) return 0 ;;
      failed)
        echo "task $task_id failed: $(jq -r '.message' "$out")" >&2
        return 1
        ;;
    esac
    sleep 1
  done
  echo "task $task_id did not complete in time" >&2
  return 1
}

# presign -> direct PUT to storage -> complete -> wait for finalization.
# Usage: upload_via_presign <media_kind> <file> <content_type> <request_json> <result_out>
# Echoes the finalized resource_id on success.
upload_via_presign() {
  local media_kind="$1" file="$2" content_type="$3" request_json="$4" result_out="$5"
  local presign_out="$WORK_DIR/presign.json"

  curl_to_file "$presign_out" -X POST "$API_URL/v1/uploads" \
    -H "content-type: application/json" \
    -d "$request_json"

  local upload_id upload_url
  upload_id="$(jq -r '.upload_id' "$presign_out")"
  upload_url="$(jq -r '.upload_url' "$presign_out")"

  # Direct-to-storage upload: bytes never touch the API.
  curl_to_file "$WORK_DIR/put-${upload_id}.txt" -X PUT \
    --upload-file "$file" \
    -H "content-type: ${content_type}" \
    "$upload_url"
  sleep "${MEDIAFORGE_E2E_PUT_SETTLE_SECONDS:-1}"

  curl_to_file "$WORK_DIR/complete.json" -X POST "$API_URL/v1/uploads/${upload_id}/complete"
  poll_upload "$upload_id" "$result_out"
  jq -r '.resource_id' "$result_out"
}

step "resolve S3 endpoint and region (provider: $PROVIDER)"

# Backblaze B2 exposes its S3 endpoint via an authorize call; the S3 access
# key id / secret are the B2 applicationKeyId / applicationKey.
authorize_b2() {
  local key_id="$1" app_key="$2" response
  response="$(curl -fsS -u "${key_id}:${app_key}" \
    "https://api.backblazeb2.com/b2api/v4/b2_authorize_account" || true)"
  if [[ -z "$response" ]] || ! jq -e '.apiInfo.storageApi.s3ApiUrl' >/dev/null 2>&1 <<<"$response"; then
    response="$(curl -fsS -u "${key_id}:${app_key}" \
      "https://api.backblazeb2.com/b2api/v3/b2_authorize_account")"
  fi
  printf '%s' "$response"
}

S3_ENDPOINT="${MEDIAFORGE_S3_ENDPOINT:-}"
S3_REGION="${MEDIAFORGE_S3_REGION:-auto}"

if [[ "$PROVIDER" == "b2" && -z "$S3_ENDPOINT" ]]; then
  AUTH_JSON="$(authorize_b2 "$MEDIAFORGE_S3_ACCESS_KEY_ID" "$MEDIAFORGE_S3_SECRET_ACCESS_KEY")"
  S3_ENDPOINT="$(jq -r '.apiInfo.storageApi.s3ApiUrl' <<<"$AUTH_JSON")"
  if [[ "$S3_ENDPOINT" == "null" || -z "$S3_ENDPOINT" ]]; then
    echo "Backblaze authorization did not return apiInfo.storageApi.s3ApiUrl" >&2
    exit 1
  fi
fi

if [[ -z "$S3_ENDPOINT" ]]; then
  echo "MEDIAFORGE_S3_ENDPOINT must be set for provider '$PROVIDER' (only 'b2' auto-derives it)" >&2
  exit 1
fi

S3_HOST="${S3_ENDPOINT#https://}"
S3_HOST="${S3_HOST#http://}"

# B2 requires the region embedded in its endpoint host (e.g. s3.us-west-004...);
# R2 and most others use "auto" or an explicit region.
if [[ "$PROVIDER" == "b2" && ( -z "$S3_REGION" || "$S3_REGION" == "auto" ) ]]; then
  S3_REGION="$(awk -F. '{print $2}' <<<"$S3_HOST")"
  if [[ -z "$S3_REGION" || "$S3_REGION" == "$S3_HOST" ]]; then
    echo "could not derive S3 region from endpoint host" >&2
    exit 1
  fi
fi

echo "    endpoint: $S3_ENDPOINT"
echo "    region:   $S3_REGION"

export MEDIAFORGE_MODE=combined
export MEDIAFORGE_STORAGE_BACKEND=s3
export MEDIAFORGE_S3_ENDPOINT="$S3_ENDPOINT"
export MEDIAFORGE_S3_REGION="$S3_REGION"
export MEDIAFORGE_S3_BUCKET="$MEDIAFORGE_S3_BUCKET"
export MEDIAFORGE_S3_ACCESS_KEY_ID="$MEDIAFORGE_S3_ACCESS_KEY_ID"
export MEDIAFORGE_S3_SECRET_ACCESS_KEY="$MEDIAFORGE_S3_SECRET_ACCESS_KEY"
export MEDIAFORGE_S3_FORCE_PATH_STYLE="${MEDIAFORGE_S3_FORCE_PATH_STYLE:-true}"
export MEDIAFORGE_HTTP_BIND="127.0.0.1:${API_PORT}"
export MEDIAFORGE_TEMP_DIR="$WORK_DIR/temp"
export MEDIAFORGE_CDN_BASE_URL="${MEDIAFORGE_CDN_BASE_URL:-https://cdn.example.invalid}"
export MEDIAFORGE_LINK_SIGNING_SECRET="${MEDIAFORGE_LINK_SIGNING_SECRET:-mediaforge-e2e-secret}"
export MEDIAFORGE_AUTH_ENABLED=false
export MEDIAFORGE_UPLOAD_LIMIT_ENABLED=true
export MEDIAFORGE_MAX_UPLOAD_BYTES="${MEDIAFORGE_MAX_UPLOAD_BYTES:-5368709120}"
export MEDIAFORGE_WORKER_CONCURRENCY=2
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

step "start API + worker (combined)"
run_low_priority "$MEDIAFORGE_BIN" combined >"$WORK_DIR/api.log" 2>&1 &
API_PID="$!"

API_READY=0
for _ in $(seq 1 60); do
  if ! kill -0 "$API_PID" >/dev/null 2>&1; then
    echo "API process exited before becoming healthy" >&2
    exit 1
  fi
  if curl -fsS "$API_URL/health" >/dev/null 2>&1; then
    API_READY=1
    break
  fi
  sleep 1
done
if [[ "$API_READY" != "1" ]]; then
  echo "API did not become healthy in time" >&2
  exit 1
fi

step "verify API health"
curl -fsS "$API_URL/health" | jq -e '.status == "ok"' >/dev/null

step "verify upload request validation and cancellation paths"
OVERSIZED_DECLARED="$((MEDIAFORGE_MAX_UPLOAD_BYTES + 1))"
jq -n --argjson size "$OVERSIZED_DECLARED" \
  '{media_kind:"image",file_name:"too-large.png",content_type:"image/png",declared_size_bytes:$size}' \
  >"$WORK_DIR/oversized-upload-request.json"
curl_status_to_file "$WORK_DIR/oversized-upload.json" 413 -X POST "$API_URL/v1/uploads" \
  -H "content-type: application/json" \
  -d @"$WORK_DIR/oversized-upload-request.json"

TTL_TOO_LONG="$((365 * 24 * 60 * 60 + 1))"
jq -n --argjson ttl "$TTL_TOO_LONG" \
  '{media_kind:"image",file_name:"ttl.png",content_type:"image/png",source_expires_in_seconds:$ttl}' \
  >"$WORK_DIR/ttl-upload-request.json"
curl_status_to_file "$WORK_DIR/ttl-upload.json" 400 -X POST "$API_URL/v1/uploads" \
  -H "content-type: application/json" \
  -d @"$WORK_DIR/ttl-upload-request.json"

curl_to_file "$WORK_DIR/missing-put-upload.json" -X POST "$API_URL/v1/uploads" \
  -H "content-type: application/json" \
  -d '{"media_kind":"image","file_name":"missing-put.png","content_type":"image/png"}'
MISSING_PUT_UPLOAD_ID="$(jq -r '.upload_id' "$WORK_DIR/missing-put-upload.json")"
curl_status_to_file "$WORK_DIR/missing-put-complete.json" 400 -X POST \
  "$API_URL/v1/uploads/${MISSING_PUT_UPLOAD_ID}/complete"
curl_status_to_file "$WORK_DIR/missing-put-delete.txt" 204 -X DELETE \
  "$API_URL/v1/uploads/${MISSING_PUT_UPLOAD_ID}"

curl_to_file "$WORK_DIR/cancel-upload.json" -X POST "$API_URL/v1/uploads" \
  -H "content-type: application/json" \
  -d '{"media_kind":"image","file_name":"cancel.png","content_type":"image/png"}'
CANCEL_UPLOAD_ID="$(jq -r '.upload_id' "$WORK_DIR/cancel-upload.json")"
curl_status_to_file "$WORK_DIR/cancel-delete.txt" 204 -X DELETE \
  "$API_URL/v1/uploads/${CANCEL_UPLOAD_ID}"
curl_status_to_file "$WORK_DIR/cancel-get.json" 404 \
  "$API_URL/v1/uploads/${CANCEL_UPLOAD_ID}"

step "presigned upload of image directly to B2"
IMAGE_RESULT_JSON="$WORK_DIR/image-upload.json"
IMAGE_RESOURCE_ID="$(upload_via_presign image "$IMAGE_PATH" "image/png" \
  '{"media_kind":"image","file_name":"e2e-image.png","content_type":"image/png"}' \
  "$IMAGE_RESULT_JSON")"
jq -e '.state == "completed"' "$IMAGE_RESULT_JSON" >/dev/null

step "verify duplicate complete is idempotent for a finalized image upload"
IMAGE_UPLOAD_ID="$(jq -r '.upload_id' "$IMAGE_RESULT_JSON")"
curl_to_file "$WORK_DIR/image-complete-again.json" -X POST "$API_URL/v1/uploads/${IMAGE_UPLOAD_ID}/complete"
jq -e '.state == "completed" and .resource_id == "'"$IMAGE_RESOURCE_ID"'"' \
  "$WORK_DIR/image-complete-again.json" >/dev/null

step "verify duplicate image upload is content-addressed to the same resource"
IMAGE_DUP_JSON="$WORK_DIR/image-dup.json"
IMAGE_DUP_RESOURCE_ID="$(upload_via_presign image "$IMAGE_PATH" "image/png" \
  '{"media_kind":"image","file_name":"e2e-image.png","content_type":"image/png"}' \
  "$IMAGE_DUP_JSON")"
[[ "$IMAGE_DUP_RESOURCE_ID" == "$IMAGE_RESOURCE_ID" ]] || {
  echo "duplicate upload produced a different resource id" >&2
  exit 1
}

step "query image manifest"
IMAGE_QUERY_JSON="$WORK_DIR/image-query.json"
curl_to_file "$IMAGE_QUERY_JSON" "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}"
jq -e '.media_kind == "image"' "$IMAGE_QUERY_JSON" >/dev/null

step "verify link policies and object-key authorization"
curl_to_file "$WORK_DIR/image-public-link.json" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/links" \
  -H "content-type: application/json" -d '{"policy":{"type":"public"}}'
jq -e '.link_kind == "public" and (.url | length > 0)' "$WORK_DIR/image-public-link.json" >/dev/null
curl_to_file "$WORK_DIR/image-storage-link.json" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/links" \
  -H "content-type: application/json" -d '{"policy":{"type":"storage_presigned","expires_in_seconds":300}}'
jq -e '.link_kind == "storage_presigned" and (.url | contains("X-Amz-Signature"))' "$WORK_DIR/image-storage-link.json" >/dev/null
curl_status_to_file "$WORK_DIR/image-invalid-expiry-link.json" 400 -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/links" \
  -H "content-type: application/json" -d '{"policy":{"type":"temporary","expires_in_seconds":0}}'
curl_status_to_file "$WORK_DIR/image-foreign-object-link.json" 400 -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/links" \
  -H "content-type: application/json" \
  -d '{"object_key":"uploads/sessions/not-owned.json","policy":{"type":"storage_presigned","expires_in_seconds":300}}'

step "verify image resource rejects video processing requests"
curl_status_to_file "$WORK_DIR/image-video-task.json" 415 -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/video" \
  -H "content-type: application/json" \
  -d '{"profile":{"codec":"h264","container":"mp4","resolution":"p360","crf":28,"bitrate_kbps":null},"screenshots":[],"generate_cover":false,"generate_hls":false}'

step "create + process image task"
IMAGE_TASK_REQUEST="$WORK_DIR/image-task-request.json"
cat >"$IMAGE_TASK_REQUEST" <<'JSON'
{"output_format":"webp","quality":80,"operations":[{"type":"resize","width":160,"height":90,"fit":"fill"},{"type":"rotate","degrees":"deg90"}]}
JSON
curl_to_file "$WORK_DIR/image-task.json" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/image" \
  -H "content-type: application/json" \
  -d @"$IMAGE_TASK_REQUEST"
IMAGE_TASK_ID="$(jq -r '.task_id' "$WORK_DIR/image-task.json")"
poll_task "$IMAGE_TASK_ID" "$WORK_DIR/image-task-status.json"

step "verify duplicate image processing request reuses completed task"
curl_to_file "$WORK_DIR/image-task-duplicate.json" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/image" \
  -H "content-type: application/json" \
  -d @"$IMAGE_TASK_REQUEST"
jq -e --arg task_id "$IMAGE_TASK_ID" '.task_id == $task_id and .status.state == "completed"' \
  "$WORK_DIR/image-task-duplicate.json" >/dev/null

step "verify image derivative manifest"
curl_to_file "$WORK_DIR/image-after.json" "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}"
jq -e '[.derived[] | select(.derived_kind == "image_variant")] | length >= 1' "$WORK_DIR/image-after.json" >/dev/null
IMAGE_DERIVED_KEY="$(jq -r '[.derived[] | select(.derived_kind == "image_variant")][0].object.object_key' "$WORK_DIR/image-after.json")"
jq -n --arg object_key "$IMAGE_DERIVED_KEY" \
  '{object_key:$object_key,policy:{type:"storage_presigned",expires_in_seconds:300}}' \
  >"$WORK_DIR/image-derived-link-request.json"
curl_to_file "$WORK_DIR/image-derived-link.json" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/links" \
  -H "content-type: application/json" \
  -d @"$WORK_DIR/image-derived-link-request.json"
jq -e '.link_kind == "storage_presigned" and (.url | contains("X-Amz-Signature"))' \
  "$WORK_DIR/image-derived-link.json" >/dev/null

step "verify invalid image task reaches failed state"
curl_to_file "$WORK_DIR/image-invalid-task.json" -X POST "$API_URL/v1/resources/${IMAGE_RESOURCE_ID}/image" \
  -H "content-type: application/json" \
  -d '{"output_format":"webp","quality":80,"operations":[{"type":"resize","width":0,"height":90,"fit":"fill"}]}'
IMAGE_INVALID_TASK_ID="$(jq -r '.task_id' "$WORK_DIR/image-invalid-task.json")"
poll_task_state "$IMAGE_INVALID_TASK_ID" failed "$WORK_DIR/image-invalid-task-status.json"

step "presigned upload of video directly to B2 (with HLS processing request)"
VIDEO_RESULT_JSON="$WORK_DIR/video-upload.json"
VIDEO_RESOURCE_ID="$(upload_via_presign video "$VIDEO_PATH" "video/mp4" \
  '{"media_kind":"video","file_name":"e2e-video.mp4","content_type":"video/mp4","processing":{"profile":{"codec":"h264","container":"mp4","resolution":"p360","crf":28,"bitrate_kbps":null},"screenshots":[{"timestamp_seconds":0.5,"output_format":"jpeg"}],"generate_cover":true,"generate_hls":true}}' \
  "$VIDEO_RESULT_JSON")"
VIDEO_TASK_ID="$(jq -r '.processing_task_id' "$VIDEO_RESULT_JSON")"
[[ "$VIDEO_TASK_ID" != "null" && -n "$VIDEO_TASK_ID" ]] || {
  echo "video upload did not schedule a processing task" >&2
  exit 1
}

step "wait for video processing task"
poll_task "$VIDEO_TASK_ID" "$WORK_DIR/video-task-status.json"

step "verify video derivative manifest"
VIDEO_AFTER_JSON="$WORK_DIR/video-after.json"
curl_to_file "$VIDEO_AFTER_JSON" "$API_URL/v1/resources/${VIDEO_RESOURCE_ID}"
jq -e '[.derived[] | select(.derived_kind == "video_variant")] | length >= 1' "$VIDEO_AFTER_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "cover")] | length >= 1' "$VIDEO_AFTER_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "screenshot")] | length >= 1' "$VIDEO_AFTER_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "hls_playlist")] | length >= 1' "$VIDEO_AFTER_JSON" >/dev/null
jq -e '[.derived[] | select(.derived_kind == "hls_segment")] | length >= 1' "$VIDEO_AFTER_JSON" >/dev/null

step "verify video resource rejects image processing requests"
curl_status_to_file "$WORK_DIR/video-image-task.json" 415 -X POST "$API_URL/v1/resources/${VIDEO_RESOURCE_ID}/image" \
  -H "content-type: application/json" \
  -d '{"output_format":"webp","quality":80,"operations":[{"type":"resize","width":160,"height":90,"fit":"fill"}]}'

step "download HLS playlist and segment through B2 presigned links"
HLS_PLAYLIST_KEY="$(jq -r '[.derived[] | select(.derived_kind == "hls_playlist")][0].object.object_key' "$VIDEO_AFTER_JSON")"
HLS_SEGMENT_KEY="$(jq -r '[.derived[] | select(.derived_kind == "hls_segment")][0].object.object_key' "$VIDEO_AFTER_JSON")"
jq -n --arg object_key "$HLS_PLAYLIST_KEY" \
  '{object_key:$object_key,policy:{type:"storage_presigned",expires_in_seconds:300}}' \
  >"$WORK_DIR/hls-playlist-link-request.json"
curl_to_file "$WORK_DIR/hls-playlist-link.json" -X POST "$API_URL/v1/resources/${VIDEO_RESOURCE_ID}/links" \
  -H "content-type: application/json" \
  -d @"$WORK_DIR/hls-playlist-link-request.json"
curl -fsS "$(jq -r '.url' "$WORK_DIR/hls-playlist-link.json")" -o "$WORK_DIR/hls-master.m3u8"
grep -q '#EXTM3U' "$WORK_DIR/hls-master.m3u8"

jq -n --arg object_key "$HLS_SEGMENT_KEY" \
  '{object_key:$object_key,policy:{type:"storage_presigned",expires_in_seconds:300}}' \
  >"$WORK_DIR/hls-segment-link-request.json"
curl_to_file "$WORK_DIR/hls-segment-link.json" -X POST "$API_URL/v1/resources/${VIDEO_RESOURCE_ID}/links" \
  -H "content-type: application/json" \
  -d @"$WORK_DIR/hls-segment-link-request.json"
curl -fsS "$(jq -r '.url' "$WORK_DIR/hls-segment-link.json")" -o "$WORK_DIR/hls-segment.ts"
test -s "$WORK_DIR/hls-segment.ts"

if [[ "${MEDIAFORGE_E2E_AUTH:-1}" == "1" ]]; then
  step "verify JWT auth gating (separate auth-enabled server)"
  AUTH_PORT="$((API_PORT + 1))"
  AUTH_URL="http://127.0.0.1:${AUTH_PORT}"
  AUTH_SECRET="e2e-jwt-secret"
  (
    export MEDIAFORGE_MODE=api
    export MEDIAFORGE_HTTP_BIND="127.0.0.1:${AUTH_PORT}"
    export MEDIAFORGE_AUTH_ENABLED=true
    export MEDIAFORGE_JWT_SECRET="$AUTH_SECRET"
    run_low_priority "$MEDIAFORGE_BIN" api >"$WORK_DIR/auth-api.log" 2>&1 &
    echo "$!" >"$WORK_DIR/auth-api.pid"
  )
  AUTH_PID="$(cat "$WORK_DIR/auth-api.pid")"
  AUTH_READY=0
  for _ in $(seq 1 60); do
    if ! kill -0 "$AUTH_PID" >/dev/null 2>&1; then
      echo "auth API process exited before becoming healthy" >&2
      exit 1
    fi
    if curl -fsS "$AUTH_URL/health" >/dev/null 2>&1; then
      AUTH_READY=1
      break
    fi
    sleep 1
  done
  if [[ "$AUTH_READY" != "1" ]]; then
    echo "auth API did not become healthy in time" >&2
    exit 1
  fi
  # Health is always open.
  curl -fsS "$AUTH_URL/health" | jq -e '.status == "ok"' >/dev/null
  # A protected endpoint without a token must be rejected.
  UNAUTH_STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$AUTH_URL/v1/uploads" \
    -H 'content-type: application/json' -d '{"media_kind":"image"}')"
  [[ "$UNAUTH_STATUS" == "401" ]] || { echo "expected 401 without token, got $UNAUTH_STATUS" >&2; kill "$AUTH_PID" 2>/dev/null || true; exit 1; }
  # With a freshly minted token it must be accepted.
  TOKEN="$(MEDIAFORGE_JWT_SECRET="$AUTH_SECRET" "$MEDIAFORGE_BIN" mint-token --subject e2e --ttl-seconds 600)"
  AUTH_STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$AUTH_URL/v1/uploads" \
    -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' -d '{"media_kind":"image"}')"
  kill "$AUTH_PID" 2>/dev/null || true
  [[ "$AUTH_STATUS" =~ ^2 ]] || { echo "expected 2xx with token, got $AUTH_STATUS" >&2; exit 1; }
fi

SUMMARY_JSON="$WORK_DIR/summary.json"
jq -n \
  --arg image_resource_id "$IMAGE_RESOURCE_ID" \
  --arg image_task_id "$IMAGE_TASK_ID" \
  --arg video_resource_id "$VIDEO_RESOURCE_ID" \
  --arg video_task_id "$VIDEO_TASK_ID" \
  --arg provider "$PROVIDER" \
  --arg endpoint_host "$S3_HOST" \
  --arg auth_checked "${MEDIAFORGE_E2E_AUTH:-1}" \
  '{
    status: "ok",
    provider: $provider,
    upload_flow: "presigned_direct_to_storage",
    endpoint_host: $endpoint_host,
    image_resource_id: $image_resource_id,
    image_task_id: $image_task_id,
    video_resource_id: $video_resource_id,
    video_task_id: $video_task_id,
    checks: [
      "upload_validation",
      "missing_put_complete",
      "cancel_upload",
      "duplicate_complete",
      "duplicate_upload_dedupe",
      "link_object_key_authorization",
      "link_expiry_validation",
      "media_kind_mismatch",
      "duplicate_task_reuse",
      "invalid_task_failure",
      "image_derivative_link",
      "video_hls_manifest_entries",
      "hls_presigned_download",
      "jwt_auth"
    ],
    auth_checked: ($auth_checked == "1")
  }' >"$SUMMARY_JSON"

cat "$SUMMARY_JSON"
