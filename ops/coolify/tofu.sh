#!/usr/bin/env bash
# Runs OpenTofu against the operator's encrypted state: tofu.sh init|import|plan|apply [args].
#   SEQUOIA_COOLIFY_STATE  absolute path of the private state file, the same for every checkout
#   SEQUOIA_COOLIFY_VARS   private tfvars with the project and server UUIDs
#   TF_VAR_state_passphrase, COOLIFY_ENDPOINT, COOLIFY_TOKEN
# Plans are saved encrypted beside the state, and apply only applies that reviewed plan.
# OpenTofu's working data (backend settings, providers) lives beside the state too, so a
# different state path always starts from its own init rather than an old cached backend.
set -euo pipefail
umask 077
cd "$(dirname "${BASH_SOURCE[0]}")"

need() {
  for name in "$@"; do
    [[ -n "${!name:-}" ]] || { echo "tofu.sh: set $name" >&2; exit 2; }
  done
}

command="${1:-}"
shift || true
need SEQUOIA_COOLIFY_STATE TF_VAR_state_passphrase
[[ "$SEQUOIA_COOLIFY_STATE" == /* ]] || { echo "tofu.sh: SEQUOIA_COOLIFY_STATE must be an absolute path" >&2; exit 2; }
export TF_DATA_DIR="${SEQUOIA_COOLIFY_STATE}.data"
plan_file="${SEQUOIA_COOLIFY_STATE}.tfplan"
[[ ! -f "$SEQUOIA_COOLIFY_STATE" ]] || chmod 600 "$SEQUOIA_COOLIFY_STATE"

# This configuration only ever adopts the existing application: plan and apply refuse a state
# that does not hold it, rather than offering to create a second one.
require_adopted() {
  local listed=""
  if [[ -f "$SEQUOIA_COOLIFY_STATE" ]]; then
    listed="$(tofu state list)" || exit 2
  fi
  if ! grep -qx coolify_application.map <<< "$listed"; then
    echo "tofu.sh: $SEQUOIA_COOLIFY_STATE has no coolify_application.map; adopt the application first:" >&2
    echo "  mise run coolify:init" >&2
    echo "  mise run coolify:import -- <project>:<server>:production:<application>" >&2
    exit 2
  fi
}

case "$command" in
  init)
    exec tofu init -input=false -backend-config="path=${SEQUOIA_COOLIFY_STATE}" "$@" ;;
  import)
    need SEQUOIA_COOLIFY_VARS COOLIFY_ENDPOINT COOLIFY_TOKEN
    [[ $# -eq 1 ]] || { echo "tofu.sh: import needs <project>:<server>:production:<application>" >&2; exit 2; }
    exec tofu import -input=false -var-file="$SEQUOIA_COOLIFY_VARS" coolify_application.map "$1" ;;
  plan)
    need SEQUOIA_COOLIFY_VARS COOLIFY_ENDPOINT COOLIFY_TOKEN
    require_adopted
    exec tofu plan -input=false -var-file="$SEQUOIA_COOLIFY_VARS" -out="$plan_file" "$@" ;;
  apply)
    need COOLIFY_ENDPOINT COOLIFY_TOKEN
    [[ -f "$plan_file" ]] || { echo "tofu.sh: no saved plan at $plan_file; run plan first" >&2; exit 2; }
    require_adopted
    exec tofu apply -input=false "$plan_file" ;;
  *)
    echo "usage: tofu.sh init|import <id>|plan [tofu plan args]|apply" >&2
    exit 2 ;;
esac
