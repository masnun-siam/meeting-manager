#!/usr/bin/env bash
# Contract tests for scripts/hooks/, the Makefile and the CI gates. Each runs in a throwaway repo
# with a clean environment and stub tools (they take the real tools' flags, log every call and
# exit as told), so no Docker, cargo or npm runs. The real .git/config is proven unchanged.
# Run: scripts/hooks/tests/run.sh
set -u
unset HOOK_JOBS BASE_REF PRE_COMMIT_FROM_REF PRE_COMMIT_TO_REF PRE_COMMIT_REMOTE_BRANCH BLUBIRD_NO_FETCH AGENT
for v in $(env | sed -n 's/^\(GIT_[A-Z_]*\)=.*/\1/p'); do unset "$v"; done
repo=$(cd "$(dirname "$0")/../../.." && pwd -P)
real_cfg=$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)/config
real_sum=$(shasum -a 256 <"$real_cfg")
t=$(mktemp -d); case $t in /tmp/* | /var/folders/* | /private/*) ;; *) echo "unexpected temp dir: $t" >&2; exit 1 ;; esac
trap 'rm -rf "${t:?}"' EXIT
fails=0; n=0
check() { n=$((n + 1)); if eval "$2" >/dev/null 2>&1; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }
newrepo() { rm -rf "$1"; git init -q -b main "$1" && for kv in user.email=t@example.com user.name=t commit.gpgsign=false gc.auto=0 maintenance.auto=false core.hooksPath=/dev/null; do git -C "$1" config "${kv%%=*}" "${kv#*=}"; done; }

# Stub tools: log "<tool> <args>" to $t/calls; exit with STUB_<TOOL>_RC (default 0).
bin=$t/bin; mkdir -p "$bin"
for tool in gitleaks rustfmt npx cargo docker; do
  cat >"$bin/$tool" <<STUB
#!/bin/sh
echo "$tool \$*" >>"$t/calls"
up=\$(echo "$tool" | tr a-z A-Z)
eval "rc=\\\${STUB_\${up}_RC:-0}"
# cargo llvm-cov / vitest --coverage write the report the gate reads (STUB_LCOV holds its body).
case "\$*" in
  *llvm-cov*--output-path*) out=\$(echo "\$*" | sed 's/.*--output-path \([^ ]*\).*/\1/'); mkdir -p "\$(dirname "\$out")"; printf '%b' "\${STUB_LCOV:-}" >"\$out" ;;
  *vitest*--coverage*) dir=\$(echo "\$*" | sed 's/.*--coverage.reportsDirectory=\([^ ]*\).*/\1/'); mkdir -p "\$dir"; printf '%b' "\${STUB_LCOV:-}" >"\$dir/lcov.info" ;;
esac
exit \$rc
STUB
  chmod +x "$bin/$tool"
done
export PATH="$bin:$PATH"

# A fixture repo with the real scripts, an origin and a base branch.
o=$t/origin.git; git init -q --bare "$o"
r=$t/repo; newrepo "$r"; cd "$r" || exit 1
mkdir -p scripts crates/meeting/src extension/src desktop/src
cp -R "$repo/scripts/hooks" "$repo/scripts/repo-guards" scripts/
cp "$repo/Makefile" .
echo 'fn a() {}' >crates/meeting/src/a.rs; echo 'export const a = 1;' >extension/src/a.ts
echo 'export const b = 1;' >extension/src/b.test.ts
git add -A; git commit -qm base; git remote add origin "$o"; git push -q origin main
git checkout -qb issue-1-x
g=scripts/hooks/gates.sh
calls() { cat "$t/calls" 2>/dev/null; }
reset_calls() { rm -f "$t/calls"; }

# --- commit stage ---
reset_calls
check "commit: empty scope says nothing to check, exits 0" '$g commit | grep -qx "nothing to check"'
check "commit: empty scope runs no tool" '[ ! -s "$t/calls" ]'
echo 'fn b() {}' >>crates/meeting/src/a.rs; git add crates/meeting/src/a.rs; reset_calls
check "commit: clean Rust change passes" '$g commit'
check "commit: rustfmt gets the staged file with edition 2024" 'calls | grep -qx "rustfmt --edition 2024 --check crates/meeting/src/a.rs"'
check "commit: gitleaks scans staged lines only" 'calls | grep -q "^gitleaks git --pre-commit --staged --redact"'
check "commit: failing rustfmt fails the commit" 'STUB_RUSTFMT_RC=1 && export STUB_RUSTFMT_RC && ! $g commit; unset STUB_RUSTFMT_RC'
check "commit: a found secret fails the commit" 'export STUB_GITLEAKS_RC=1; ! $g commit; rc=$?; unset STUB_GITLEAKS_RC; [ $rc = 0 ]'
printf '#[allow(dead_code)]\nfn c() {}\n' >>crates/meeting/src/a.rs; git add crates/meeting/src/a.rs
check "commit: a new #[allow(...)] fails" '! $g commit'
git checkout -q -- crates/meeting/src/a.rs 2>/dev/null; git reset -q; git checkout -q crates/meeting/src/a.rs
printf '// @ts-ignore\n' >>extension/src/a.ts; git add extension/src/a.ts
check "commit: a new @ts-ignore fails" '! $g commit'
git reset -q; git checkout -q extension/src/a.ts
head -c 600000 /dev/zero >big.bin; git add big.bin
check "commit: a 586 KB file fails" '! $g commit'
git reset -q; rm big.bin
echo 'export const c = 2;' >>extension/src/a.ts; git add extension/src/a.ts; reset_calls
check "commit: biome gets the file relative to its package" '$g commit && calls | grep -q "^npx biome check .* src/a.ts$"'
git reset -q; git checkout -q extension/src/a.ts

# --- run.sh: protected branches (exit before Docker) ---
check "run: a commit on main is refused" 'git checkout -q main && ! scripts/hooks/run.sh commit | grep -q x; git checkout -q issue-1-x'
check "run: a commit on main names the rule" 'git checkout -q main; scripts/hooks/run.sh commit | grep -q "protected"; rc=$?; git checkout -q issue-1-x; [ $rc = 0 ]'
check "run: a push to main is refused" 'PRE_COMMIT_REMOTE_BRANCH=refs/heads/main scripts/hooks/run.sh push; [ $? = 1 ]'

# --- docker.sh: Docker down fails with a message, never falls back ---
check "docker: Docker down fails" 'STUB_DOCKER_RC=1 scripts/hooks/docker.sh true; [ $? = 1 ]'
check "docker: Docker down says so" 'STUB_DOCKER_RC=1 scripts/hooks/docker.sh true 2>&1 | grep -q "Docker is not running"'

# --- push stage scopes (exit status, not just output) ---
export BLUBIRD_NO_FETCH=1
git fetch -q origin
check "push: nothing changed exits 0 with nothing to check" '$g push | grep -qx "nothing to check"'
echo 'export const b = 2;' >>extension/src/b.test.ts; git commit -qam "tests only"; reset_calls
check "push: tests-only change passes" 'STUB_LCOV="SF:src/a.ts\nDA:1,1\nend_of_record\n" $g push'
check "push: tests-only change runs related tests" 'calls | grep -q "npx vitest related src/b.test.ts --run --coverage"'
git rm -q extension/src/b.test.ts; git commit -qm "delete test"; git reset -q --soft origin/main; git commit -qm "only a deletion"; reset_calls
check "push: a deleted test alone exits 0" '$g push'
check "push: a deleted test alone runs no test tool" '! calls | grep -qE "vitest|llvm-cov"'
git reset -q --hard origin/main
printf 'fn c() {\n    let _ = 1;\n}\n' >>crates/meeting/src/a.rs; git commit -qam "rust change"; reset_calls
lines=$(wc -l <crates/meeting/src/a.rs)
cov_ok="SF:$r/crates/meeting/src/a.rs\n$(for i in $(seq 1 "$lines"); do printf 'DA:%s,1\\n' "$i"; done)end_of_record\n"
cov_bad="SF:$r/crates/meeting/src/a.rs\n$(for i in $(seq 1 "$lines"); do printf 'DA:%s,0\\n' "$i"; done)end_of_record\n"
check "push: covered Rust change passes" 'STUB_LCOV="$cov_ok" $g push'
check "push: Rust change runs clippy and llvm-cov" 'calls | grep -q "cargo clippy -p meeting" && calls | grep -q "cargo llvm-cov -p meeting"'
check "push: uncovered Rust change fails" 'STUB_LCOV="$cov_bad" $g push; [ $? = 1 ]'
check "push: a missing coverage report fails" 'STUB_LCOV="" $g push; [ $? = 1 ]'
check "push: failing tests fail" 'STUB_CARGO_RC=1 STUB_LCOV="$cov_ok" $g push; [ $? = 1 ]'
unset BLUBIRD_NO_FETCH

# --- Makefile ---
check "make: check runs every gate" 'make -n check | grep -qx "scripts/hooks/run.sh all"'
check "make: up builds the extension in Docker" 'make -n up | grep -q "scripts/hooks/docker.sh sh -c .cd extension && npx vite build."'
check "make: up runs tauri dev on 1420 without AGENT" 'make -n desktop | grep -q "localhost:1420"'
p1=$(make -n desktop AGENT=alpha | grep -o 'localhost:[0-9]*' | head -1); p2=$(make -n desktop AGENT=alpha | grep -o 'localhost:[0-9]*' | head -1); p3=$(make -n desktop AGENT=beta | grep -o 'localhost:[0-9]*' | head -1)
check "make: AGENT gives a stable port of its own" '[ "$p1" = "$p2" ] && [ "$p1" != "localhost:1420" ] && [ "$p1" != "$p3" ]'
check "make: all and with= start the same as up" 'make -n up all with=x | grep -q "this product has one repo"'

# --- CI and image pins agree ---
w=$repo/.github/workflows
check "ci: tool runs the changed-line coverage gate" 'grep -q "changed-lines-coverage.py" "$w/tool.yml"'
check "ci: extension and desktop guard their coverage logs" 'grep -q coverage-log-guard "$w/extension.yml" && grep -q coverage-log-guard "$w/desktop.yml"'
gl_ci=$(grep -o '[0-9a-f]\{64\}  gl.tgz' "$w/guards.yml" | cut -d' ' -f1)
check "ci: gitleaks checksum matches the hook image" '[ -n "$gl_ci" ] && grep -q "gl_sum=$gl_ci" "$repo/docker/hooks.Dockerfile"'
check "ci: Rust comes from rust-toolchain.toml" '! grep -q "rust-toolchain@stable" "$w/tool.yml" && grep -q "channel = \"1.100.0\"" "$repo/rust-toolchain.toml" && grep -q "rust:1.100.0" "$repo/docker/hooks.Dockerfile"'
check "ci: Node 24 everywhere" 'grep -qx 24 "$repo/.nvmrc" && grep -q "node:24" "$repo/docker/hooks.Dockerfile" && ! grep -h "node-version:" "$w"/*.yml | grep -v "node-version: 24"'

check "real .git/config unchanged" '[ "$(shasum -a 256 <"$real_cfg")" = "$real_sum" ]'
echo "$((n - fails))/$n passed"
[ "$fails" = 0 ]
