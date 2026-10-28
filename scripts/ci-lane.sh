#!/usr/bin/env bash
# Choose what the hosted CI job runs for this event:
#   full   - formatting, Clippy and tests
#   docs   - nothing: only files the checks never read changed
#   reuse  - a successful run already checked this tree; the job still runs the
#            checks when the dependency cache lacks this exact key, to save it
# Prints lane=, reason= and evidence= lines, and appends them to $GITHUB_OUTPUT.
# Any missing history or failed lookup selects full.
set -euo pipefail

: "${EVENT:?}"
api="repos/${REPO:-}/actions/workflows/ci.yml/runs"
checks_job=rust
checks_step="Run the hosted checks"

# Files the hosted checks never read. Unknown paths, deletions and both sides of
# renames are judged by name, so anything else selects the checks. No Rust source
# includes client/input.css: Tailwind compiles it, and only the release builds run that.
outside_checks() {
  case "$1" in
    README.md|AGENTS.md|LICENSE|docs/*|ops/*|.ci/*|.cargo/audit.toml|\
    Dockerfile|.dockerignore|services/sequoia-ingest/Dockerfile|compose.yaml|\
    .github/workflows/deploy-images.yml|client/input.css) return 0 ;;
    *) return 1 ;;
  esac
}

# Succeeds when every path that differs between two commits is outside the checks.
only_outside() {
  local file
  git diff --name-only --no-renames -z "$1" "$2" > "${tmp}/changed" || return 1
  while IFS= read -r -d '' file; do
    outside_checks "${file}" || return 1
  done < "${tmp}/changed"
}

have_commit() {
  [[ "$1" =~ ^[0-9a-f]{40}$ && "$1" != 0000000000000000000000000000000000000000 ]] || return 1
  git cat-file -e "$1^{commit}" 2>/dev/null || git fetch --quiet --no-tags origin "$1" 2>/dev/null
}

finish() {
  printf 'lane=%s\nreason=%s\nevidence=%s\n' "$1" "$2" "${3:-}" | tee -a "${GITHUB_OUTPUT:-/dev/null}"
  exit 0
}

# A successful main push run certifies its tree: it either ran the checks or
# reused a run that did. A pull request run counts only if its checks step ran.
push_run() {
  gh api --method GET "${api}" -f head_sha="$1" -f event=push -f branch=main -f status=success \
    --jq '[.workflow_runs[].id] | first // empty'
}

checked_pr_run() {
  local id
  for id in $(gh api --method GET "${api}" -f head_sha="$1" -f event=pull_request -f status=success --jq '.workflow_runs[].id'); do
    if [ "$(gh api --method GET "repos/${REPO}/actions/runs/${id}/jobs" -f filter=latest \
      --jq "[.jobs[] | select(.name == \"${checks_job}\") | .steps[] | select(.name == \"${checks_step}\") | .conclusion] == [\"success\"]")" = true ]; then
      echo "${id}"
      return 0
    fi
  done
  return 1
}

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
head="$(git rev-parse HEAD)"

case "${EVENT}" in
  workflow_dispatch)
    finish full "manual run" ;;
  pull_request|merge_group)
    # A pull request checks out its merge commit; the first parent is the base it was merged onto.
    if [ "${EVENT}" = pull_request ]; then base="$(git rev-parse --verify --quiet HEAD^1 || true)"; else base="${BEFORE:-}"; fi
    have_commit "${base}" || finish full "base commit unavailable"
    only_outside "${base}" "${head}" || finish full "changes reach the checks"
    finish docs "only files outside the checks changed" ;;
  push)
    : "${REPO:?}"
    # A direct push whose predecessor is certified and which changes only unread files.
    if have_commit "${BEFORE:-}" && run="$(push_run "${BEFORE}")" && [ -n "${run}" ] && only_outside "${BEFORE}" "${head}"; then
      finish docs "only files outside the checks changed since a successful run" "${run}"
    fi
    # A merged pull request whose checked head has this tree's checked content. Requiring the
    # head to contain the merge's parent means the pull request run tested that head's tree.
    if pr_head="$(gh api --method GET "repos/${REPO}/commits/${head}/pulls" \
         --jq "[.[] | select(.merged_at != null and .merge_commit_sha == \"${head}\" and .base.ref == \"main\") | .head.sha] | if length == 1 then .[0] else empty end")" \
       && have_commit "${pr_head}" \
       && git merge-base --is-ancestor HEAD^1 "${pr_head}" 2>/dev/null \
       && only_outside "${pr_head}" "${head}" \
       && run="$(checked_pr_run "${pr_head}")"; then
      finish reuse "the merged pull request head passed the checks with this content" "${run}"
    fi
    finish full "no successful run checked this content" ;;
  *)
    finish full "unrecognised event ${EVENT}" ;;
esac
