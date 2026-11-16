#!/usr/bin/env bash
# Entry point for the git hooks and make targets.
# Usage: scripts/hooks/run.sh commit | push | all
#   commit  pre-commit: no commits on main, then the commit gates in the hook image
#   push    pre-push: no pushes to main; fetches origin/main once on the host (the remote may need
#           SSH keys the container lacks), then the push gates in the hook image and the Tauri crate
#   all     make check: every gate on every file
set -uo pipefail

stage=${1:?usage: run.sh commit|push|all}
export BASE_REF=${BASE_REF:-main}
root=$(git rev-parse --show-toplevel)
cd "$root"
started=$(date +%s)

protected() {
  [ "$1" = "$BASE_REF" ] && { echo "✗ '$BASE_REF' is protected: work on an issue-<n>-<slug> branch and open a pull request."; exit 1; }
  return 0
}

case $stage in
  commit)
    protected "$(git branch --show-current)"
    scripts/hooks/docker.sh scripts/hooks/gates.sh commit
    rc=$? ;;
  push)
    r=${PRE_COMMIT_REMOTE_BRANCH:-}; protected "${r#refs/heads/}"
    scripts/repo-guards/changed-files.sh --push >/dev/null || exit 1
    BLUBIRD_NO_FETCH=1 scripts/hooks/docker.sh scripts/hooks/gates.sh push
    rc=$?
    BLUBIRD_NO_FETCH=1 scripts/hooks/host-rust.sh push || rc=1 ;;
  all)
    scripts/repo-guards/changed-files.sh --push >/dev/null 2>&1 || true
    BLUBIRD_NO_FETCH=1 scripts/hooks/docker.sh scripts/hooks/gates.sh all
    rc=$?
    BLUBIRD_NO_FETCH=1 scripts/hooks/host-rust.sh all || rc=1 ;;
  *) echo "usage: run.sh commit|push|all" >&2; exit 2 ;;
esac
echo "$stage gates: $(( $(date +%s) - started )) s"
exit $rc
