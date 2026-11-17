#!/usr/bin/env bash
# The Tauri crate's gates, on the host: it links macOS frameworks, so it cannot build in the hook
# image (recorded deviation, onboarding #186). Same toolchain as the image via rust-toolchain.toml.
# Usage: host-rust.sh push | all
set -uo pipefail

stage=${1:?usage: host-rust.sh push|all}
export BASE_REF=${BASE_REF:-main}
export PATH="$HOME/.cargo/bin:$PATH"
root=$(git rev-parse --show-toplevel)
cd "$root"
guards=scripts/repo-guards
jobs=$("$guards/changed-files.sh" --jobs)
logs=.hook-logs
mkdir -p "$logs" coverage

# Pre-push checks the crate only when it changed (owner, #186); make check and CI always do.
if [ "$stage" = push ]; then
  files=$(BLUBIRD_NO_FETCH=1 "$guards/changed-files.sh" --push) || { echo "cannot resolve the files to check"; exit 1; }
  if ! printf '%s\n' "$files" | grep -q '^desktop/src-tauri/'; then
    echo "Tauri crate: nothing to check"
    exit 0
  fi
fi

command -v cargo-llvm-cov >/dev/null || { echo "cargo-llvm-cov missing on the host: run make setup"; exit 1; }

# The crate embeds desktop/dist at compile time; build it in the hook image when missing.
if [ ! -f desktop/dist/index.html ]; then
  echo "── desktop/dist missing: building it in the hook image"
  scripts/hooks/docker.sh sh -c 'cd desktop && npx vite build' || { echo "✗ desktop build"; exit 1; }
fi

failed=0
echo "── clippy meeting-desktop"
cargo clippy -p meeting-desktop --all-targets --locked -- -D warnings || { echo "✗ clippy meeting-desktop"; failed=1; }
echo "── tests + coverage meeting-desktop ($jobs workers)"
if cargo llvm-cov -p meeting-desktop --locked --lcov --output-path coverage/meeting-desktop.lcov -- --test-threads="$jobs" 2>&1 | tee "$logs/meeting-desktop-cov.log"; then
  "$guards/coverage-log-guard.sh" "$logs/meeting-desktop-cov.log" coverage/meeting-desktop.lcov || failed=1
  python3 -I "$guards/changed-lines-coverage.py" coverage/meeting-desktop.lcov -- 'desktop/src-tauri/*.rs' || failed=1
else
  echo "✗ tests meeting-desktop"; failed=1
fi
[ $failed = 0 ] && echo "✓ Tauri crate gates"
exit $failed
