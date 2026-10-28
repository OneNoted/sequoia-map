#!/usr/bin/env bash
# Exercises scripts/ci-lane.sh against throwaway Git histories and canned GitHub API answers.
set -euo pipefail

lane_script="$(cd "$(dirname "$0")" && pwd)/ci-lane.sh"
root="$(mktemp -d)"
trap 'rm -rf "${root}"' EXIT
failures=0

# Answers `gh api --method GET PATH -f k=v ... --jq EXPR` from $root/api/<PATH>?<sorted k=v>.json.
mkdir -p "${root}/bin" "${root}/api"
cat > "${root}/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ -z "${FAKE_GH_FAIL:-}" ] || exit 1
shift 3 # api --method GET
path="$1"; shift
params=(); jq_expr=.
while [ "$#" -gt 0 ]; do
  case "$1" in
    -f) params+=("$2"); shift 2 ;;
    --jq) jq_expr="$2"; shift 2 ;;
    *) echo "unexpected gh argument $1" >&2; exit 2 ;;
  esac
done
query="$(printf '%s\n' "${params[@]}" | sort | paste -sd'&')"
file="${FAKE_GH_API}/$(printf '%s?%s' "${path}" "${query}" | tr '/?&=' '____').json"
[ -f "${file}" ] || exit 1
jq -r "${jq_expr}" "${file}"
EOF
chmod +x "${root}/bin/gh"

answer() { # PATH QUERY JSON
  printf '%s\n' "$3" > "${root}/api/$(printf '%s?%s' "$1" "$2" | tr '/?&=' '____').json"
}

git_quiet() { git -c user.name=ci -c user.email=ci@example.invalid -c commit.gpgsign=false "$@" >/dev/null; }

# origin holds every commit; the checkout fetches by SHA as Actions does.
git init --quiet --bare "${root}/origin.git"
git -C "${root}/origin.git" config uploadpack.allowAnySHA1InWant true
git init --quiet -b main "${root}/work"
cd "${root}/work"
git remote add origin "${root}/origin.git"
mkdir -p server/src
echo 'fn main() {}' > server/src/main.rs
echo '# Map' > README.md
git add -A && git_quiet commit -m base
base="$(git rev-parse HEAD)"

commit() { # MESSAGE FILE CONTENT -> SHA
  mkdir -p "$(dirname "$2")"
  printf '%s\n' "$3" > "$2"
  git add -A && git_quiet commit -m "$1"
  git rev-parse HEAD
}

expect() { # NAME LANE [ENV...]
  local name="$1" want="$2" got
  shift 2
  got="$(env -i PATH="${root}/bin:${PATH}" HOME="${HOME}" FAKE_GH_API="${root}/api" REPO=o/r "$@" bash "${lane_script}" | sed -n 's/^lane=//p')"
  if [ "${got}" = "${want}" ]; then
    echo "ok   ${name}: ${got}"
  else
    echo "FAIL ${name}: wanted ${want}, got ${got:-nothing}"
    failures=$((failures + 1))
  fi
}

# A pull request merge commit with the given head, on top of base.
pr_merge() { # HEAD_SHA -> merge SHA
  git checkout --quiet --detach "${base}"
  git_quiet merge --no-ff --no-edit "$1"
  git rev-parse HEAD
}

branch() { # NAME FILE CONTENT -> head SHA
  git checkout --quiet -B "$1" "${base}"
  commit "$1" "$2" "$3"
}

docs_head="$(branch docs docs/guide.md 'How to run')"
src_head="$(branch src server/src/main.rs 'fn main() { println!("x"); }')"
git checkout --quiet -B mixed "${base}"
mkdir -p docs && echo 'Notes' > docs/notes.md
mixed_head="$(commit mixed server/src/lib.rs '')"
git checkout --quiet -B rename "${base}"
mkdir -p docs && git mv server/src/main.rs docs/main.rs.md && git_quiet commit -m rename
rename_head="$(git rev-parse HEAD)"
git checkout --quiet -B delete "${base}"
git rm --quiet server/src/main.rs && git_quiet commit -m delete
delete_head="$(git rev-parse HEAD)"
git checkout --quiet -B unknown "${base}"
unknown_head="$(commit unknown .github/workflows/ci.yml 'name: CI')"

git push --quiet origin --all
for head in "${docs_head}" "${src_head}" "${mixed_head}" "${rename_head}" "${delete_head}" "${unknown_head}"; do
  merge="$(pr_merge "${head}")"
  git push --quiet origin "${merge}:refs/merges/${head}"
done

pr() { # NAME HEAD WANT
  git checkout --quiet --detach "refs/remotes/origin/merges/$2"
  expect "pull request, $1" "$3" EVENT=pull_request
}
git fetch --quiet origin '+refs/merges/*:refs/remotes/origin/merges/*'
pr "docs only" "${docs_head}" docs
pr "source change" "${src_head}" full
pr "docs and source" "${mixed_head}" full
pr "source renamed into docs" "${rename_head}" full
pr "source deleted" "${delete_head}" full
pr "workflow change" "${unknown_head}" full

# A shallow pull request checkout without the base parent runs everything.
git clone --quiet --depth 1 "file://${root}/origin.git" "${root}/shallow"
git -C "${root}/shallow" fetch --quiet --depth 1 origin "refs/merges/${docs_head}"
git -C "${root}/shallow" checkout --quiet --detach FETCH_HEAD
(cd "${root}/shallow" && expect "pull request without its base" full EVENT=pull_request)

git checkout --quiet --detach "${docs_head}"
expect "merge group, docs only" docs EVENT=merge_group BEFORE="${base}"
expect "merge group, base unknown" full EVENT=merge_group BEFORE=1111111111111111111111111111111111111111
expect "manual dispatch" full EVENT=workflow_dispatch
expect "unrecognised event" full EVENT=schedule

# Main pushes. A squash merge of an up-to-date pull request has the head's tree.
runs=repos/o/r/actions/workflows/ci.yml/runs
git checkout --quiet -B main "${base}"
squash="$(commit squash server/src/main.rs 'fn main() { println!("x"); }')"
git push --quiet --force origin main
pulls() { answer "repos/o/r/commits/$1/pulls" "" "[{\"merged_at\":\"t\",\"merge_commit_sha\":\"$1\",\"base\":{\"ref\":\"main\"},\"head\":{\"sha\":\"$2\"}}]"; }
pr_runs() { answer "${runs}" "event=pull_request&head_sha=$1&status=success" "{\"workflow_runs\":[{\"id\":$2}]}"; }
checks_step() { answer "repos/o/r/actions/runs/$1/jobs" "filter=latest" "{\"jobs\":[{\"name\":\"rust\",\"steps\":[{\"name\":\"Run the hosted checks\",\"conclusion\":\"$2\"}]}]}"; }
no_push_run() { answer "${runs}" "branch=main&event=push&head_sha=$1&status=success" '{"workflow_runs":[]}'; }

no_push_run "${base}"
expect "push, merged pull request without evidence" full EVENT=push BEFORE="${base}"
pulls "${squash}" "${src_head}"
pr_runs "${src_head}" 7
checks_step 7 success
expect "push, merged pull request that passed the checks" reuse EVENT=push BEFORE="${base}"
checks_step 7 skipped
expect "push, merged pull request whose checks were skipped" full EVENT=push BEFORE="${base}"
checks_step 7 success
expect "push, lookup failure" full EVENT=push BEFORE="${base}" FAKE_GH_FAIL=1

# The pull request head differs in checked content from what landed.
git checkout --quiet -B main "${base}"
other="$(commit other server/src/main.rs 'fn main() { println!("y"); }')"
git push --quiet --force origin main
pulls "${other}" "${src_head}"
expect "push, merged content differs from the checked head" full EVENT=push BEFORE="${base}"

# The head does not contain the merge's parent, so its pull request run tested another merge.
git checkout --quiet -B main "${docs_head}"
stale="$(commit stale server/src/main.rs 'fn main() { println!("x"); }')"
git push --quiet --force origin main
pulls "${stale}" "${src_head}"
no_push_run "${docs_head}"
expect "push, head behind the merge parent" full EVENT=push BEFORE="${docs_head}"

# Direct pushes that only touch unread files rely on the predecessor's successful run.
git checkout --quiet -B main "${squash}"
readme="$(commit readme README.md '# Sequoia map')"
git push --quiet --force origin main
answer "repos/o/r/commits/${readme}/pulls" "" '[]'
answer "${runs}" "branch=main&event=push&head_sha=${squash}&status=success" '{"workflow_runs":[{"id":9}]}'
expect "push, docs after a successful run" docs EVENT=push BEFORE="${squash}"
no_push_run "${squash}"
expect "push, docs after an unsuccessful run" full EVENT=push BEFORE="${squash}"
expect "push, new branch" full EVENT=push BEFORE=0000000000000000000000000000000000000000
expect "push, predecessor missing" full EVENT=push BEFORE=2222222222222222222222222222222222222222

if [ "${failures}" -ne 0 ]; then
  echo "${failures} lane selection case(s) failed"
  exit 1
fi
echo "all lane selection cases passed"
