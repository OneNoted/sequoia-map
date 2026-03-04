#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

GRADLE_BIN="${GRADLE_BIN:-gradle}"
MODE="${MODE:-prism}"
WATCH_LOG="${WATCH_LOG:-build/live-reload-compile.log}"
SYNC_LOG="${SYNC_LOG:-build/live-reload-sync.log}"
PRISM_BIN="${PRISM_BIN:-prismlauncher}"
PRISM_ROOT_DIR="${PRISM_ROOT_DIR:-$HOME/.local/share/PrismLauncher}"
PRISM_INSTANCE_ID="${PRISM_INSTANCE_ID:-Wynncraft A}"
PRISM_INSTANCE_DIR="${PRISM_INSTANCE_DIR:-$PRISM_ROOT_DIR/instances/$PRISM_INSTANCE_ID}"
MOD_OUTPUT_NAME="${MOD_OUTPUT_NAME:-wynn-iris-live.jar}"
TARGET_MC_VERSION="${TARGET_MC_VERSION:-}"
LOOM_INSTALL_WYNN_MODS="${LOOM_INSTALL_WYNN_MODS:-1}"
LOOM_INSTALL_PERF_MODS="${LOOM_INSTALL_PERF_MODS:-1}"
LOOM_MODS_DIR="${LOOM_MODS_DIR:-$ROOT_DIR/run/mods}"
MODRINTH_API="${MODRINTH_API:-https://api.modrinth.com/v2}"
MODRINTH_USER_AGENT="${MODRINTH_USER_AGENT:-wynn-iris-live-dev/1.0}"
GRADLE_MC_ARGS=()

if [[ -d "$PRISM_INSTANCE_DIR/minecraft" ]]; then
  PRISM_GAME_DIR_DEFAULT="$PRISM_INSTANCE_DIR/minecraft"
elif [[ -d "$PRISM_INSTANCE_DIR/.minecraft" ]]; then
  PRISM_GAME_DIR_DEFAULT="$PRISM_INSTANCE_DIR/.minecraft"
else
  PRISM_GAME_DIR_DEFAULT="$PRISM_INSTANCE_DIR/minecraft"
fi

PRISM_GAME_DIR="${PRISM_GAME_DIR:-$PRISM_GAME_DIR_DEFAULT}"
PRISM_MODS_DIR="${PRISM_MODS_DIR:-$PRISM_GAME_DIR/mods}"

if [[ "${1:-}" == "--loom" ]]; then
  MODE="loom"
  shift
fi

if ! command -v "$GRADLE_BIN" >/dev/null 2>&1; then
  echo "Gradle binary not found: ${GRADLE_BIN}" >&2
  exit 1
fi

mkdir -p "$(dirname "$WATCH_LOG")" "$(dirname "$SYNC_LOG")"

urlencode_json_list() {
  local value="$1"
  printf '%s' "[\"${value}\"]"
}

modrinth_versions_json() {
  local project_slug="$1"
  local loader="$2"
  local game_version="$3"
  local loader_json
  local version_json

  loader_json="$(urlencode_json_list "$loader")"
  version_json="$(urlencode_json_list "$game_version")"

  curl -fsSL -A "$MODRINTH_USER_AGENT" --get \
    --data-urlencode "loaders=${loader_json}" \
    --data-urlencode "game_versions=${version_json}" \
    "${MODRINTH_API}/project/${project_slug}/version"
}

resolve_modrinth_file() {
  local versions_json="$1"
  local version_type="$2"

  local preferred
  preferred="$(jq -r --arg version_type "$version_type" '
    [ .[]
      | select(.status == "listed")
      | select(.version_type == $version_type)
    ] as $versions
    | if ($versions | length) == 0 then
        empty
      else
        $versions[0].files
        | map(select(.primary == true))[0]
        | "\(.url)\t\(.filename)\t\($versions[0].version_number)"
      end
  ' <<<"$versions_json")"
  if [[ -n "$preferred" ]]; then
    printf '%s\n' "$preferred"
    return 0
  fi

  jq -r '
    [ .[] | select(.status == "listed") ] as $versions
    | if ($versions | length) == 0 then
        empty
      else
        $versions[0].files
        | map(select(.primary == true))[0]
        | "\(.url)\t\(.filename)\t\($versions[0].version_number)"
      end
  ' <<<"$versions_json"
}

install_modrinth_project() {
  local project_slug="$1"
  local game_version="$2"
  local cleanup_pattern="$3"
  local preferred_version_type="${4:-release}"

  local versions_json resolved url filename version_number target
  versions_json="$(modrinth_versions_json "$project_slug" "fabric" "$game_version")"
  resolved="$(resolve_modrinth_file "$versions_json" "$preferred_version_type")"
  if [[ -z "$resolved" ]]; then
    echo "[live-dev] No compatible versions found for '${project_slug}' on Fabric ${game_version}" >&2
    return 1
  fi

  IFS=$'\t' read -r url filename version_number <<<"$resolved"
  if [[ -z "$url" || -z "$filename" ]]; then
    echo "[live-dev] Could not resolve primary file for '${project_slug}'" >&2
    return 1
  fi

  find "$LOOM_MODS_DIR" -maxdepth 1 -type f -name "$cleanup_pattern" ! -name "$filename" -delete
  target="$LOOM_MODS_DIR/$filename"

  if [[ -f "$target" ]]; then
    echo "[live-dev] ${project_slug} already installed: ${filename} (${version_number})"
    return 0
  fi

  echo "[live-dev] Installing ${project_slug} ${version_number} for ${game_version}..."
  curl -fsSL -A "$MODRINTH_USER_AGENT" "$url" -o "${target}.tmp"
  mv "${target}.tmp" "$target"
}

install_loom_wynncraft_mods() {
  if [[ "$LOOM_INSTALL_WYNN_MODS" != "1" && "$LOOM_INSTALL_WYNN_MODS" != "true" ]]; then
    return 0
  fi

  if ! command -v jq >/dev/null 2>&1; then
    echo "[live-dev] Missing required dependency: jq (needed to resolve Modrinth versions)." >&2
    echo "[live-dev] Install jq or set LOOM_INSTALL_WYNN_MODS=0 to skip auto-install." >&2
    return 1
  fi

  local loom_mc_version
  loom_mc_version="$(awk -F= '/^minecraft_version=/{print $2}' gradle.properties | tr -d '[:space:]')"
  if [[ -z "$loom_mc_version" ]]; then
    echo "[live-dev] Could not determine minecraft_version from gradle.properties" >&2
    return 1
  fi

  mkdir -p "$LOOM_MODS_DIR"
  install_modrinth_project "auth-me" "$loom_mc_version" "authme-*.jar" "release"
  install_modrinth_project "wynntils" "$loom_mc_version" "wynntils-*.jar" "release"
}

install_loom_performance_mods() {
  if [[ "$LOOM_INSTALL_PERF_MODS" != "1" && "$LOOM_INSTALL_PERF_MODS" != "true" ]]; then
    return 0
  fi

  if ! command -v jq >/dev/null 2>&1; then
    echo "[live-dev] Missing required dependency: jq (needed to resolve Modrinth versions)." >&2
    echo "[live-dev] Install jq or set LOOM_INSTALL_PERF_MODS=0 to skip performance bundle." >&2
    return 1
  fi

  local loom_mc_version
  loom_mc_version="$(awk -F= '/^minecraft_version=/{print $2}' gradle.properties | tr -d '[:space:]')"
  if [[ -z "$loom_mc_version" ]]; then
    echo "[live-dev] Could not determine minecraft_version from gradle.properties" >&2
    return 1
  fi

  mkdir -p "$LOOM_MODS_DIR"
  install_modrinth_project "sodium" "$loom_mc_version" "sodium-*.jar" "release"
  install_modrinth_project "lithium" "$loom_mc_version" "lithium-*.jar" "release"
  install_modrinth_project "ferrite-core" "$loom_mc_version" "ferritecore-*.jar" "release"
  install_modrinth_project "entityculling" "$loom_mc_version" "entityculling-*.jar" "release"
  install_modrinth_project "immediatelyfast" "$loom_mc_version" "ImmediatelyFast-*.jar" "release"
  # Voxy currently ships alpha builds for this MC target; explicitly allow alpha.
  install_modrinth_project "voxy" "$loom_mc_version" "voxy-*.jar" "alpha"
}

find_latest_mod_jar() {
  find build/libs -maxdepth 1 -type f -name '*.jar' \
    ! -name '*-sources.jar' \
    ! -name '*-dev.jar' \
    -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-
}

install_latest_jar() {
  local jar
  jar="$(find_latest_mod_jar || true)"
  if [[ -z "$jar" || ! -f "$jar" ]]; then
    echo "[live-dev] No built mod jar found under build/libs" >&2
    return 1
  fi

  local target="${PRISM_MODS_DIR}/${MOD_OUTPUT_NAME}"
  local tmp_target="${target}.tmp"
  cp "$jar" "$tmp_target"
  mv "$tmp_target" "$target"
  echo "[$(date '+%H:%M:%S')] installed ${jar##*/} -> ${target}" >>"$SYNC_LOG"
}

cleanup() {
  if [[ -n "${SYNC_PID:-}" ]] && kill -0 "$SYNC_PID" >/dev/null 2>&1; then
    echo "[live-dev] Stopping sync watcher (${SYNC_PID})..."
    kill "$SYNC_PID" >/dev/null 2>&1 || true
    wait "$SYNC_PID" 2>/dev/null || true
  fi

  if [[ -n "${WATCH_PID:-}" ]] && kill -0 "$WATCH_PID" >/dev/null 2>&1; then
    echo "[live-dev] Stopping compiler watcher (${WATCH_PID})..."
    kill "$WATCH_PID" >/dev/null 2>&1 || true
    wait "$WATCH_PID" 2>/dev/null || true
  fi
}

trap cleanup EXIT INT TERM

if [[ "$MODE" == "loom" ]]; then
  echo "[live-dev] Mode: loom"
  echo "[live-dev] Loom mods dir: ${LOOM_MODS_DIR}"
  install_loom_wynncraft_mods
  install_loom_performance_mods
  echo "[live-dev] Starting continuous compiler..."
  "$GRADLE_BIN" classes --continuous >"$WATCH_LOG" 2>&1 &
  WATCH_PID=$!
  echo "[live-dev] Compiler watcher PID: ${WATCH_PID}"
  echo "[live-dev] Compiler log: ${WATCH_LOG}"
  echo "[live-dev] Starting Fabric client with live reload enabled..."
  "$GRADLE_BIN" -PliveReload=true runClient "$@"
  exit $?
fi

if [[ "$MODE" != "prism" ]]; then
  echo "[live-dev] Unknown mode: $MODE (expected 'prism' or 'loom')." >&2
  exit 1
fi

if ! command -v "$PRISM_BIN" >/dev/null 2>&1; then
  echo "PrismLauncher binary not found: ${PRISM_BIN}" >&2
  exit 1
fi

if [[ ! -d "$PRISM_INSTANCE_DIR" ]]; then
  echo "Prism instance not found: ${PRISM_INSTANCE_DIR}" >&2
  exit 1
fi

if [[ -z "$TARGET_MC_VERSION" && -f "$PRISM_INSTANCE_DIR/mmc-pack.json" ]]; then
  TARGET_MC_VERSION="$(grep -A4 '"uid": "net.minecraft"' "$PRISM_INSTANCE_DIR/mmc-pack.json" | grep '"version"' | head -n1 | sed -E 's/.*"([0-9.]+)".*/\1/' || true)"
fi

if [[ -n "$TARGET_MC_VERSION" && -f "profiles/${TARGET_MC_VERSION}.properties" ]]; then
  # shellcheck disable=SC1090
  source "profiles/${TARGET_MC_VERSION}.properties"
  GRADLE_MC_ARGS+=(
    "-Pminecraft_version=${minecraft_version}"
    "-Pyarn_mappings=${yarn_mappings}"
    "-Ploader_version=${loader_version}"
    "-Pfabric_version=${fabric_version}"
    "-Parchives_base_name=wynn-iris-mc${minecraft_version}"
    "-Pmod_version=0.1.4+${TARGET_MC_VERSION//./_}"
  )
fi

mkdir -p "$PRISM_MODS_DIR"

echo "[live-dev] Mode: prism"
echo "[live-dev] Prism root: ${PRISM_ROOT_DIR}"
echo "[live-dev] Prism instance: ${PRISM_INSTANCE_ID}"
echo "[live-dev] Prism game dir: ${PRISM_GAME_DIR}"
echo "[live-dev] Prism mods dir: ${PRISM_MODS_DIR}"
if [[ -n "$TARGET_MC_VERSION" ]]; then
  echo "[live-dev] Target MC version: ${TARGET_MC_VERSION}"
fi
echo "[live-dev] Running initial remap build..."
"$GRADLE_BIN" remapJar "${GRADLE_MC_ARGS[@]}"
echo "[live-dev] Installing initial jar..."
install_latest_jar
echo "[live-dev] Starting continuous remap build..."
"$GRADLE_BIN" remapJar --continuous "${GRADLE_MC_ARGS[@]}" >"$WATCH_LOG" 2>&1 &
WATCH_PID=$!

sync_jar_loop() {
  local last_signature=""

  while kill -0 "$WATCH_PID" >/dev/null 2>&1; do
    local jar
    jar="$(find_latest_mod_jar || true)"
    if [[ -n "$jar" && -f "$jar" ]]; then
      local sig
      sig="$(stat -c '%Y:%s' "$jar")"
      if [[ "$sig" != "$last_signature" ]]; then
        install_latest_jar
        last_signature="$sig"
      fi
    fi
    sleep 1
  done
}

sync_jar_loop &
SYNC_PID=$!

echo "[live-dev] Compiler watcher PID: ${WATCH_PID}"
echo "[live-dev] Compiler log: ${WATCH_LOG}"
echo "[live-dev] Sync watcher PID: ${SYNC_PID}"
echo "[live-dev] Sync log: ${SYNC_LOG}"
echo "[live-dev] Launching Prism instance '${PRISM_INSTANCE_ID}'..."
"$PRISM_BIN" --dir "$PRISM_ROOT_DIR" --launch "$PRISM_INSTANCE_ID" "$@"

echo "[live-dev] Prism launch command returned; watchers remain active."
echo "[live-dev] Press Ctrl+C to stop."
wait "$WATCH_PID"
