#!/usr/bin/env bash
# The local gates, run inside the hook image by scripts/hooks/docker.sh.
# Usage: gates.sh commit | push | all
#   commit  staged files: secrets in added lines, suppressions, hygiene, rustfmt, biome (seconds)
#   push    files changed since origin/$BASE_REF: the same over the whole branch, plus clippy, types,
#           the related tests with coverage and >=80% of changed lines covered
#   all     every file and the full suites (make check), same coverage gate
# The Tauri crate (desktop/src-tauri) is checked on the host by scripts/hooks/host-rust.sh.
set -uo pipefail

stage=${1:?usage: gates.sh commit|push|all}
export BASE_REF=${BASE_REF:-main}
base=origin/$BASE_REF
guards=scripts/repo-guards
jobs=$("$guards/changed-files.sh" --jobs)
logs=.hook-logs
mkdir -p "$logs"
failed=()

step() { echo "── $1"; }
fail() { echo "✗ $1"; failed+=("$1"); }

case $stage in
  commit) files=$("$guards/changed-files.sh") ;;
  push) files=$(BLUBIRD_NO_FETCH=1 "$guards/changed-files.sh" --push) || { echo "cannot resolve the files to check"; exit 1; } ;;
  all) files=$("$guards/changed-files.sh" --all-files) ;;
  *) echo "usage: gates.sh commit|push|all" >&2; exit 2 ;;
esac
if [ -z "$files" ]; then
  echo "nothing to check"
  exit 0
fi
pick() { printf '%s\n' "$files" | grep -E "$1" || true; }

# Added lines: the staged diff for a commit, the branch diff for a push (catches merge commits).
added() {
  case $stage in
    commit) git diff --cached -U0 --no-color --diff-filter=ACMR -- "$@" ;;
    push) git diff -U0 --no-color --diff-filter=ACMR "$base...HEAD" -- "$@" ;;
    all) git diff -U0 --no-color --diff-filter=ACMR "$(git hash-object -t tree /dev/null)" HEAD -- "$@" ;;
  esac
}

step "secrets"
case $stage in
  commit) gitleaks git --pre-commit --staged --redact --no-banner . || fail "secrets in staged lines" ;;
  push) gitleaks git --log-opts="$base..HEAD" --redact --no-banner . || fail "secrets in branch commits" ;;
  all)
    # Tracked files only: node_modules, target and local scratch are never scanned.
    tree=$(mktemp -d)
    git archive HEAD | tar -x -C "$tree"
    gitleaks dir --redact --no-banner "$tree" || fail "secrets in tracked files"
    rm -rf "$tree" ;;
esac

if [ "$stage" != all ]; then
  step "no new lint, type or test suppressions"
  # This script holds the patterns themselves, so it is not scanned.
  hits=$(added . ':!scripts/hooks/gates.sh' | grep -E '^\+' | grep -v '^+++' \
    | grep -nE '#!?\[(allow|expect)\(|cfg_attr\(.*(allow|expect)\(|biome-ignore|@ts-(ignore|nocheck|expect-error)|eslint-disable|\b(it|test|describe)\.(skip|only)\(|#\[ignore\]' || true)
  [ -z "$hits" ] || { printf '%s\n' "$hits"; fail "new suppression (fix the cause instead)"; }

  step "hygiene"
  conflict=$(added . | grep -nE '^\+(<<<<<<<|>>>>>>>)( |$)' || true)
  [ -z "$conflict" ] || { printf '%s\n' "$conflict"; fail "merge conflict markers"; }
  while IFS= read -r f; do
    [ -f "$f" ] || continue
    size=$(wc -c <"$f")
    [ "$size" -le 512000 ] || fail "$f is $((size / 1024)) KB (limit 500 KB)"
  done <<<"$files"
fi

rs=$(pick '\.rs$')
if [ -n "$rs" ]; then
  step "rustfmt"
  # shellcheck disable=SC2086
  rustfmt --edition 2024 --check $rs || fail "rustfmt (run: make fmt)"
fi

for pkg in extension desktop; do
  web=$(pick "^$pkg/.*\.(ts|tsx|js|json|css)$" | sed "s|^$pkg/||")
  if [ -n "$web" ]; then
    step "biome $pkg"
    # shellcheck disable=SC2086
    (cd "$pkg" && npx biome check --no-errors-on-unmatched --files-ignore-unknown=true $web) || fail "biome $pkg"
  fi
done

[ "$stage" = commit ] && { [ ${#failed[@]} -eq 0 ] && echo "✓ commit gates" && exit 0; printf 'failed: %s\n' "${failed[@]}"; exit 1; }

# --- push and all: compile, test, coverage ---
rm -rf coverage/meeting.lcov coverage/extension coverage/desktop && mkdir -p coverage

if [ "$stage" = all ] || [ -n "$(pick '^(crates/meeting/|Cargo\.(toml|lock)$|rust-toolchain\.toml$)')" ]; then
  step "clippy meeting"
  cargo clippy -p meeting --all-targets --locked -- -D warnings || fail "clippy meeting"
  step "tests + coverage meeting ($jobs workers)"
  if cargo llvm-cov -p meeting --locked --lcov --output-path coverage/meeting.lcov -- --test-threads="$jobs" 2>&1 | tee "$logs/meeting-cov.log"; then
    "$guards/coverage-log-guard.sh" "$logs/meeting-cov.log" coverage/meeting.lcov || fail "coverage report meeting"
    python3 -I "$guards/changed-lines-coverage.py" coverage/meeting.lcov -- 'crates/meeting/*.rs' || fail "changed-line coverage meeting"
  else
    fail "tests meeting"
  fi
fi

for pkg in extension desktop; do
  changed=$(pick "^$pkg/")
  [ "$stage" = all ] || [ -n "$changed" ] || continue
  step "types $pkg"
  (cd "$pkg" && npx tsc --noEmit) || fail "types $pkg"
  [ "$stage" = all ] && { step "biome ci $pkg"; (cd "$pkg" && npx biome ci .) || fail "biome ci $pkg"; }
  src=$(printf '%s\n' "$changed" | grep -E "^$pkg/src/.*\.(ts|tsx)$" | sed "s|^$pkg/||" || true)
  other=$(printf '%s\n' "$changed" | grep -vE "^$pkg/src/" | grep . || true)
  if [ "$stage" = all ] || [ -n "$other" ]; then
    mode=(run)                      # config or dependency change: every test
  elif [ -n "$src" ]; then
    # shellcheck disable=SC2206
    mode=(related $src --run)       # only the tests that import the changed files
  else
    continue
  fi
  step "tests + coverage $pkg (${mode[0]}, $jobs workers)"
  if (cd "$pkg" && npx vitest "${mode[@]}" --coverage --coverage.reportsDirectory=../coverage/$pkg --maxWorkers="$jobs") 2>&1 | tee "$logs/$pkg-cov.log"; then
    # vitest writes paths relative to the package; the gate reads them from the repo root.
    sed -i "s|^SF:src/|SF:$pkg/src/|" "coverage/$pkg/lcov.info"
    "$guards/coverage-log-guard.sh" "$logs/$pkg-cov.log" "coverage/$pkg/lcov.info" || fail "coverage report $pkg"
    python3 -I "$guards/changed-lines-coverage.py" "coverage/$pkg/lcov.info" -- "$pkg/src/*.ts" "$pkg/src/*.tsx" \
      ":!$pkg/src/*.test.ts" ":!$pkg/src/*.test.tsx" ":!$pkg/src/test/*" || fail "changed-line coverage $pkg"
  else
    fail "tests $pkg"
  fi
done

if [ ${#failed[@]} -gt 0 ]; then
  printf 'failed: %s\n' "${failed[@]}"
  exit 1
fi
echo "✓ $stage gates"
