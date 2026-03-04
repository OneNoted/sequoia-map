#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <minecraft-version>"
  echo "Example: $0 1.21.4"
  echo "Example: $0 1.21.11"
  exit 1
fi

TARGET="$1"
PROFILE="profiles/${TARGET}.properties"

if [[ ! -f "$PROFILE" ]]; then
  echo "Unknown profile: ${TARGET}"
  ls -1 profiles | sed 's/\.properties$//' | sed 's/^/ - /'
  exit 1
fi

# shellcheck disable=SC1090
source "$PROFILE"

if [[ -z "${minecraft_version:-}" || -z "${yarn_mappings:-}" || -z "${loader_version:-}" || -z "${fabric_version:-}" ]]; then
  echo "Profile ${PROFILE} is missing required fields"
  exit 1
fi

BASE_NAME="wynn-iris-mc${minecraft_version}"
MOD_VERSION_SUFFIX="${TARGET//./_}"

./gradlew --no-daemon clean build \
  -Pminecraft_version="${minecraft_version}" \
  -Pyarn_mappings="${yarn_mappings}" \
  -Ploader_version="${loader_version}" \
  -Pfabric_version="${fabric_version}" \
  -Parchives_base_name="${BASE_NAME}" \
  -Pmod_version="0.1.4+${MOD_VERSION_SUFFIX}"
