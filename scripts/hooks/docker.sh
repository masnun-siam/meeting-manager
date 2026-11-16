#!/usr/bin/env bash
# Runs a command in the hook image (docker/hooks.Dockerfile), from the repo root, as the calling user.
# Usage: scripts/hooks/docker.sh <command> [args...]
# - The image is tagged by the Dockerfile's hash and built when missing.
# - The checkout and git's common dir are mounted at their host paths, so worktrees and coverage
#   paths work unchanged. Each package's node_modules is a volume keyed on its lockfile (Linux
#   binaries, never the host's macOS ones), installed with `npm ci` when new.
# - Every container is named meeting-manager-hook-<pid>-<start> and labelled meeting-manager.hook;
#   ones whose host process died are removed first.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
common=$(git rev-parse --path-format=absolute --git-common-dir)
cd "$root"

if ! docker info >/dev/null 2>&1; then
  echo "Docker is not running: start Docker Desktop, then retry (hooks never fall back to the host)." >&2
  exit 1
fi

hash=$(shasum -a 256 docker/hooks.Dockerfile | cut -c1-12)
image=meeting-manager-hooks:$hash
if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "Building $image (first run only, a few minutes)" >&2
  if ! docker build --label meeting-manager.hook=1 -t "$image" -f docker/hooks.Dockerfile docker >&2; then
    echo "Building the hook image failed; see the output above." >&2
    exit 1
  fi
fi

# Remove hook containers left by killed runs (their host PID is gone).
for c in $(docker ps -a --filter label=meeting-manager.hook=1 --format '{{.Names}}'); do
  pid=${c#meeting-manager-hook-}; pid=${pid%%-*}
  case $pid in ''|*[!0-9]*) continue ;; esac
  kill -0 "$pid" 2>/dev/null || docker rm -f "$c" >/dev/null 2>&1 || true
done

started=$(date +%s)
name=meeting-manager-hook-$$-$started

nm_args=()
for pkg in extension desktop; do
  lock=$(shasum -a 256 "$pkg/package-lock.json" | cut -c1-12)
  vol=meeting-manager-nm-$pkg-$lock
  nm_args+=(-v "$vol:$root/$pkg/node_modules")
  if ! docker volume inspect "$vol" >/dev/null 2>&1; then
    docker volume create --label meeting-manager.hook=1 "$vol" >/dev/null
    # A new volume mounted outside the image's folders is root-owned; hand it to the hook user.
    docker run --rm --label meeting-manager.hook=1 -v "$vol:/v" "$image" chown "$(id -u):$(id -g)" /v
    echo "Installing $pkg dependencies into $vol (once per lockfile)" >&2
    if ! docker run --rm --name "$name-npm-$pkg" --label meeting-manager.hook=1 \
        --user "$(id -u):$(id -g)" -v "$root:$root" -v "$vol:$root/$pkg/node_modules" \
        -v meeting-manager-npm-cache:/cache/npm -w "$root/$pkg" "$image" npm ci --no-audit --no-fund >&2; then
      docker volume rm "$vol" >/dev/null 2>&1 || true
      echo "npm ci failed for $pkg; see the output above." >&2
      exit 1
    fi
  fi
done

tty=()
[ -t 1 ] && tty=(-t)
exec docker run --rm "${tty[@]}" --name "$name" --label meeting-manager.hook=1 \
  --user "$(id -u):$(id -g)" \
  -v "$root:$root" -v "$common:$common" "${nm_args[@]}" \
  -v meeting-manager-cargo:/cache/cargo -v meeting-manager-target:/cache/target \
  -v meeting-manager-npm-cache:/cache/npm \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0="*" \
  -e FEATURE_MEETING_CONTEXT -e FEATURE_DESKTOP_APP -e HOOK_JOBS -e BASE_REF -e BLUBIRD_NO_FETCH -e PRE_COMMIT_FROM_REF -e PRE_COMMIT_TO_REF \
  -w "$root" "$image" "$@"
