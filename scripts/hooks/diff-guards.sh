#!/usr/bin/env bash
# Suppressions and hygiene over the lines a diff adds. One script for the commit hook (--cached),
# pre-push (origin/<base>...HEAD) and CI (the PR's origin/<base>...HEAD), so all three agree.
# Usage: diff-guards.sh <git diff range or --cached>
set -uo pipefail

range=${1:?usage: diff-guards.sh <range|--cached>}
failed=0
added() { git diff -U0 --no-color --diff-filter=ACMR "$range" -- "$@"; }
files=$(git diff --name-only --diff-filter=ACMR "$range") || { echo "✗ cannot diff $range"; exit 1; }

echo "── no new lint, type or test suppressions"
# Code only: docs and these scripts name the patterns without using them.
hits=$(added '*.rs' '*.ts' '*.tsx' '*.js' | grep -E '^\+' | grep -v '^+++' \
  | grep -nE '#!?\[(allow|expect)\(|cfg_attr\(.*(allow|expect)\(|biome-ignore|@ts-(ignore|nocheck|expect-error)|eslint-disable|\b(it|test|describe)\.(skip|only)\(|#\[ignore\]' || true)
[ -z "$hits" ] || { printf '%s\n' "$hits"; echo "✗ new suppression (fix the cause instead)"; failed=1; }

echo "── hygiene"
conflict=$(added . | grep -nE '^\+(<<<<<<<|>>>>>>>)( |$)' || true)
[ -z "$conflict" ] || { printf '%s\n' "$conflict"; echo "✗ merge conflict markers"; failed=1; }
while IFS= read -r f; do
  [ -n "$f" ] && [ -f "$f" ] || continue
  size=$(wc -c <"$f")
  [ "$size" -le 512000 ] || { echo "✗ $f is $((size / 1024)) KB (limit 500 KB)"; failed=1; }
done <<<"$files"
exit $failed
