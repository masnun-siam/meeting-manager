#!/usr/bin/env bash
# blubird repo guard changed-files.sh, plugin 0.22.1, body sha256 212739b4bac0b3d5baeeba4c75ede9d5e84684c5397218b1e03eadda580acd7a
# Copy as is. Body = line 4 on (tail -n +4 | shasum -a 256); a copy with another hash is reported, then updated after a yes.
# The files a hook should check, so hooks run on what changed, never the whole repo.
# Usage: changed-files.sh [--push | --all-files] [-- <pathspec>...]
#   default      files staged for this commit
#   --push       files changed on this branch: origin/<BASE_REF or staging>...HEAD, the base fetched once
#                per push; pre-commit's FROM/TO refs only when no base resolves (with a warning)
#   --all-files  every tracked file (pre-commit run --all-files)
#   --jobs       print the worker count: HOOK_JOBS, else max(1, CPUs - 2); run it where the work runs
# Prints one path per line (added, copied, modified or renamed). An empty scope prints nothing and
# "nothing to check" on stderr, and exits 0: the caller then passes without running anything.
set -euo pipefail

mode=staged
while [ $# -gt 0 ]; do
  case $1 in
    --push) mode=push ;;
    --all-files) mode=all ;;
    --jobs)
      cpus=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 1)
      jobs=${HOOK_JOBS:-$((cpus - 2))}
      [ "$jobs" -ge 1 ] 2>/dev/null || jobs=1
      echo "$jobs"; exit 0 ;;
    --) shift; break ;;
    *) echo "usage: $0 [--push | --all-files | --jobs] [-- <pathspec>...]" >&2; exit 2 ;;
  esac
  shift
done

case $mode in
  staged) files=$(git diff --cached --name-only --diff-filter=ACMR -- "$@") ;;
  all) files=$(git ls-files -- "$@") ;;
  push)
    base=${BASE_REF:-staging}
    # One fetch per push: every hook of the same push reuses it (stamp = HEAD, under 10 minutes old).
    stamp=$(git rev-parse --git-path blubird-base-fetch)
    head=$(git rev-parse HEAD)
    now=$(date +%s)
    read -r s_head s_time s_base 2>/dev/null <"$stamp" || s_head=""
    if [ "${BLUBIRD_NO_FETCH:-}" != 1 ] && { [ "$s_head" != "$head" ] || [ "${s_base:-}" != "$base" ] || [ $((now - ${s_time:-0})) -gt 600 ]; }; then
      if git fetch --quiet origin "+refs/heads/$base:refs/remotes/origin/$base" 2>/dev/null; then
        echo "$head $now $base" >"$stamp"
      else
        echo "warning: could not fetch origin/$base; comparing with the local copy" >&2
      fi
    fi
    # Always the whole branch since it left its base, as CI diffs it; pre-commit's refs only cover
    # the commits since the last push, so they are a fallback when no base can be resolved.
    if git rev-parse -q --verify "origin/$base^{commit}" >/dev/null; then
      range="origin/$base...HEAD"
    elif [ -n "${PRE_COMMIT_FROM_REF:-}" ] && [ -n "${PRE_COMMIT_TO_REF:-}" ]; then
      echo "warning: no origin/$base; checking only $PRE_COMMIT_FROM_REF...$PRE_COMMIT_TO_REF" >&2
      range="$PRE_COMMIT_FROM_REF...$PRE_COMMIT_TO_REF"
    else
      echo "no origin/$base to compare with" >&2; exit 1
    fi
    files=$(git diff --name-only --diff-filter=ACMR "$range" -- "$@") ;;
esac

if [ -z "$files" ]; then
  echo "nothing to check" >&2
  exit 0
fi
printf '%s\n' "$files"
