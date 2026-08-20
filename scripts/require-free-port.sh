#!/usr/bin/env bash
set -euo pipefail

port="${1:-}"
label="${2:-service}"

if [[ -z "${port}" ]]; then
  echo "usage: scripts/require-free-port.sh <port> [label]" >&2
  exit 1
fi

if command -v ss >/dev/null 2>&1; then
  if ss -ltn "( sport = :${port} )" | tail -n +2 | grep -q .; then
    echo "${label} cannot start: port ${port} is already in use." >&2
    if command -v lsof >/dev/null 2>&1; then
      lsof -iTCP:"${port}" -sTCP:LISTEN >&2 || true
    fi
    exit 1
  fi
  exit 0
fi

if command -v lsof >/dev/null 2>&1; then
  if lsof -iTCP:"${port}" -sTCP:LISTEN -t >/dev/null 2>&1; then
    echo "${label} cannot start: port ${port} is already in use." >&2
    lsof -iTCP:"${port}" -sTCP:LISTEN >&2 || true
    exit 1
  fi
  exit 0
fi

echo "${label} cannot verify port ${port}: install 'ss' or 'lsof' for the local preflight check." >&2
exit 1
