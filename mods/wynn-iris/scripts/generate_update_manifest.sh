#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "Usage: $0 <dist-dir> <release-tag> <repo>"
  exit 1
fi

DIST_DIR="$1"
RELEASE_TAG="$2"
REPO="$3"
OUT_FILE="${DIST_DIR}/iris-update-manifest.json"

if [[ ! -d "$DIST_DIR" ]]; then
  echo "Missing dist dir: $DIST_DIR"
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required"
  exit 1
fi

shopt -s nullglob
jars=("${DIST_DIR}"/*.jar)
if [[ ${#jars[@]} -eq 0 ]]; then
  echo "No jar assets found in ${DIST_DIR}"
  exit 1
fi

assets='[]'
for jar in "${jars[@]}"; do
  name="$(basename "$jar")"
  sha="$(sha256sum "$jar" | awk '{print $1}')"
  size="$(stat -c%s "$jar")"

  version=""
  mc=""
  if [[ "$name" =~ wynn-iris-mc([0-9]+\.[0-9]+\.[0-9]+)-([0-9]+\.[0-9]+\.[0-9]+.*)\.jar$ ]]; then
    mc="${BASH_REMATCH[1]}"
    version="${BASH_REMATCH[2]}"
  fi

  asset_type="mod"
  if [[ "$name" == *"-sources.jar" ]]; then
    asset_type="sources"
  fi

  assets="$(jq \
    --arg name "$name" \
    --arg type "$asset_type" \
    --arg minecraft "$mc" \
    --arg version "$version" \
    --arg sha256 "$sha" \
    --argjson size "$size" \
    '. + [{name:$name,type:$type,minecraft:$minecraft,version:$version,sha256:$sha256,size:$size}]' \
    <<<"$assets")"
done

jq -n \
  --arg schema "iris-update-manifest/v1" \
  --arg release_tag "$RELEASE_TAG" \
  --arg repo "$REPO" \
  --arg created_at "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
  --argjson assets "$assets" \
  '{schema:$schema,release_tag:$release_tag,repo:$repo,created_at:$created_at,assets:$assets}' \
  > "$OUT_FILE"

echo "Wrote ${OUT_FILE}"
