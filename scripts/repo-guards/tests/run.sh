#!/usr/bin/env bash
# Tests for the shared repo guards, each in a throwaway repo with a clean environment: no GIT_* or
# exported task-runner variables leak in, and the real repo's .git/config is proven unchanged.
# shellcheck disable=SC2034  # variables are read inside eval'd checks
set -u
unset HOOK_JOBS BASE_REF PRE_COMMIT_FROM_REF PRE_COMMIT_TO_REF BLUBIRD_NO_FETCH
for v in $(env | sed -n 's/^\(GIT_[A-Z_]*\)=.*/\1/p'); do unset "$v"; done
here=$(cd "$(dirname "$0")/.." && pwd -P)
real_cfg=$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)/config
real_sum=$(shasum -a 256 <"$real_cfg")
t=$(mktemp -d); case $t in /tmp/* | /var/folders/* | /private/*) ;; *) echo "unexpected temp dir: $t" >&2; exit 1 ;; esac
trap 'rm -rf "${t:?}"' EXIT
fails=0; n=0
check() { n=$((n + 1)); if eval "$2" >/dev/null 2>&1; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }
# Fixture repos never run auto-maintenance in the background (it races the rm in the trap).
newrepo() { rm -rf "$1"; git init -q -b main "$1" && for kv in user.email=t@example.com user.name=t commit.gpgsign=false gc.auto=0 maintenance.auto=false; do git -C "$1" config "${kv%%=*}" "${kv#*=}"; done; }

# Headers: plugin version and the hash of the body (line 4 on).
for f in "$here"/claude-md-guard.sh "$here"/changed-files.sh "$here"/coverage-log-guard.sh "$here"/check-docs.py "$here"/changed-lines-coverage.py; do
  want=$(sed -n '2s/.*body sha256 \([0-9a-f]*\).*/\1/p' "$f"); got=$(tail -n +4 "$f" | shasum -a 256 | cut -d' ' -f1)
  check "header hash matches body: ${f##*/}" '[ -n "$want" ] && [ "$want" = "$got" ]'
done

# claude-md-guard.sh
g=$here/claude-md-guard.sh; r=$t/guard; newrepo "$r"; cd "$r" || exit 1
echo root >CLAUDE.md; ln -s CLAUDE.md AGENTS.md; git add -A; git commit -qm init
check "guard: root CLAUDE.md only passes" '"$g" nested && "$g" generated'
mkdir sub; echo x >sub/CLAUDE.md; git add sub
check "guard: a nested CLAUDE.md fails" '! "$g" nested'
git rm -q --cached sub/CLAUDE.md; rm -r sub
printf 'x\n<!-- gitnexus:start -->\n' >CLAUDE.md; git add CLAUDE.md
check "guard: a GitNexus block fails" '! "$g" generated'
check "guard: warn never fails" '"$g" warn | grep -q WARNING'
git checkout -q CLAUDE.md 2>/dev/null; git reset -q --hard
rm AGENTS.md; echo copy >AGENTS.md; git add AGENTS.md
check "guard: AGENTS.md that is not a symlink fails" '! "$g" generated'
git reset -q --hard; mkdir -p .claude/skills/gitnexus-cli; echo s >.claude/skills/gitnexus-cli/SKILL.md; git add .claude
check "guard: committed GitNexus skills fail" '! "$g" generated'
git rm -rq --cached .claude; rm -rf .claude
git reset -q --hard; git remote add origin https://github.com/acme/shop-api.git
check "guard: pointer missing fails" '! "$g" pointer'
mkdir -p .blubird "$t/wiki/.blubird"; echo ../wiki >.blubird/docs-repo; git add .blubird; git commit -qm ptr
check "guard: pointer shape passes without a sibling clone" '"$g" pointer'
check "guard: docs-repo fails when the map does not list this repo" '! "$g" docs-repo'
echo '[{"repo": "acme/shop-api", "path": "../guard"}]' >"$t/wiki/.blubird/repos.json"
check "guard: docs-repo passes when the map lists it" '"$g" docs-repo'
git worktree add -q "$r/.worktrees/w" -b w
check "guard: docs-repo passes from a worktree" '(cd "$r/.worktrees/w" && "$g" docs-repo)'
mkdir -p docs/prds; echo p >docs/prds/a.md; git add docs
check "guard: a docs/prds/ folder fails" '! "$g" pointer'
check "guard: hook env (GIT_DIR, GIT_INDEX_FILE) works" 'GIT_DIR="$r/.git" GIT_INDEX_FILE="$r/.git/index" "$g" nested'

# changed-files.sh
c=$here/changed-files.sh; o=$t/origin.git; git init -q --bare "$o"; r=$t/cf; newrepo "$r"; cd "$r" || exit 1
echo a >a.php; echo b >b.md; git add -A; git commit -qm base; git remote add origin "$o"; git push -q origin main:staging
git checkout -qb feature; echo a2 >>a.php; echo c >c.php; git add -A
check "changed: staged files are listed" '[ "$("$c")" = "$(printf "a.php\nc.php")" ]'
check "changed: pathspec filters" '[ "$("$c" -- "*.md")" = "" ]'
check "changed: empty scope says nothing to check and exits 0" '"$c" -- "*.md" 2>&1 | grep -qx "nothing to check"'
check "changed: --all-files lists every tracked file" '"$c" --all-files | grep -qx b.md'
git commit -qm work
check "changed: --push lists the branch's changes" '[ "$("$c" --push)" = "$(printf "a.php\nc.php")" ]'
git clone -q -b staging "$o" "$t/other" && (cd "$t/other" && git -c user.email=t@example.com -c user.name=t commit -q --allow-empty -m later && git push -q origin staging)
before=$(git rev-parse origin/staging)
"$c" --push >/dev/null
check "changed: the base is fetched once per push (a second hook reuses it)" '[ "$(git rev-parse origin/staging)" = "$before" ]'
rm -f "$(git rev-parse --git-path blubird-base-fetch)"; "$c" --push >/dev/null
check "changed: a new push fetches the base again" '[ "$(git rev-parse origin/staging)" != "$before" ]'
git push -q origin feature; echo d >d.php; git add d.php; git commit -qm second
check "changed: a second push still sees the first push's files" '[ "$(PRE_COMMIT_FROM_REF=origin/feature PRE_COMMIT_TO_REF=HEAD "$c" --push)" = "$(printf "a.php\nc.php\nd.php")" ]'
check "changed: pre-commit refs are a warned fallback without a base" 'BASE_REF=nobase BLUBIRD_NO_FETCH=1 PRE_COMMIT_FROM_REF=HEAD~1 PRE_COMMIT_TO_REF=HEAD "$c" --push 2>&1 | grep -q warning && [ "$(BASE_REF=nobase BLUBIRD_NO_FETCH=1 PRE_COMMIT_FROM_REF=HEAD~1 PRE_COMMIT_TO_REF=HEAD "$c" --push 2>/dev/null)" = d.php ]'
check "changed: HOOK_JOBS sets workers, never below 1" '[ "$(HOOK_JOBS=3 "$c" --jobs)" = 3 ] && [ "$(HOOK_JOBS=0 "$c" --jobs)" = 1 ] && [ "$("$c" --jobs)" -ge 1 ]'
sum=$(shasum -a 256 <.git/config)
check "changed: hook env leaves .git/config unchanged" 'GIT_DIR="$r/.git" GIT_INDEX_FILE="$r/.git/index" "$c" --push >/dev/null && [ "$(shasum -a 256 <.git/config)" = "$sum" ]'

# coverage-log-guard.sh
v=$here/coverage-log-guard.sh; cd "$t" || exit 1
echo '<coverage/>' >clover.xml; echo 'Tests: 12 passed' >ok.log
check "coverage: a complete run passes" '"$v" ok.log clover.xml'
check "coverage: a missing report fails" '! "$v" ok.log missing.xml'
echo 'Failed to parse src/a.vue. Excluding it from coverage.' >bad.log
check "coverage: an unparseable file fails" '! "$v" bad.log clover.xml'

# changed-lines-coverage.py: a.php gains lines 11-12; old.php moves to "src dir/new.php" with line 3 edited;
# then the base edits a.php line 1, which three dots must not count as this branch's change.
k=$here/changed-lines-coverage.py; o=$t/cl-origin.git; git init -q --bare "$o"; r=$t/cl; newrepo "$r"; cd "$r" || exit 1
seq 10 >a.php; seq 5 >old.php; git add -A; git commit -qm base; git remote add origin "$o"; git push -q origin main:staging
git checkout -qb feature; printf '11\n12\n' >>a.php; mkdir "src dir"; git mv old.php "src dir/new.php"
sed '3s/.*/three/' "src dir/new.php" >x && mv x "src dir/new.php"; git add -A; git commit -qm work
git clone -q -b staging "$o" "$t/cl2" && (cd "$t/cl2" && sed '1s/.*/one/' a.php >x && mv x a.php && git -c user.email=t@example.com -c user.name=t commit -qam later && git push -q origin staging) && git fetch -q origin
cov() { python3 -I "$k" "$@" -- '*.php'; }
lcov() { printf 'SF:%s\nDA:1,0\nDA:11,%s\nDA:12,%s\nend_of_record\nSF:src dir/new.php\nDA:1,0\nDA:2,0\nDA:3,%s\nDA:4,0\nend_of_record\n' "$@"; }
lcov "$r/a.php" 1 1 1 >full.info; lcov "$r/a.php" 0 0 1 >bad.info; printf 'SF:a.php\nDA:11,1\nDA:12,1\n' >partial.info
lcov "$r/a.php" 1 0 0 >s1.info; lcov "$r/a.php" 0 1 1 >s2.info
check "lines: covered changed lines pass (renames followed, three-dot base)" 'cov full.info'
check "lines: uncovered changed lines fail" '! cov bad.info && cov bad.info | grep -q "a.php: uncovered changed lines 11,12"'
check "lines: below --min fails, at it passes" '! cov --min 100 s1.info && cov --min 25 s1.info'
check "lines: shards merge per line" 'cov s1.info s2.info'
check "lines: a changed file missing from every report fails" '! cov partial.info && cov partial.info | grep -q "src dir/new.php: changed but in no coverage report"'
ln -s "$r" "$t/cl-link"; lcov "$t/cl-link/a.php" 1 1 1 >link.info
check "lines: a report written under a symlinked path still matches" 'cov link.info'
git config diff.noprefix true; git config diff.renames false
check "lines: diff.noprefix and diff.renames=false change nothing" 'cov full.info && ! cov bad.info'
git config --unset diff.noprefix; git config --unset diff.renames
check "lines: a missing or empty report fails" '! cov nope.info && : >empty.info && ! cov empty.info'
check "lines: a missing base fails" '! BASE_REF=nobase cov full.info'
check "lines: no pathspec is a usage error" 'python3 -I "$k" full.info; [ $? = 2 ]'
check "lines: nothing changed in scope passes" 'python3 -I "$k" full.info -- "*.md" 2>&1 | grep -qx "nothing to check"'
printf '<coverage><project><file name="%s/a.php"><line num="11" type="stmt" count="1"/><line num="12" type="stmt" count="2"/></file><file name="%s/src dir/new.php"><line num="3" type="stmt" count="1"/></file></project></coverage>' "$r" "$r" >clover.xml
check "lines: clover reports are read" 'cov clover.xml && sed "s/count=\"2\"/count=\"0\"/; s/count=\"1\"/count=\"0\"/g" clover.xml >clover0.xml && ! cov clover0.xml'
printf '<coverage><sources><source>%s</source></sources><packages><package><classes><class filename="a.php"><lines><line number="11" hits="1"/><line number="12" hits="1"/></lines></class><class filename="src dir/new.php"><lines><line number="3" hits="0"/></lines></class></classes></package></packages></coverage>' "$r" >cobertura.xml
check "lines: cobertura reports are read" '! cov --min 100 cobertura.xml && cov --min 60 cobertura.xml'

# check-docs.py
d=$here/check-docs.py; r=$t/docs; newrepo "$r"; cd "$r" || exit 1
mkdir -p docs app/Models; touch app/Models/User.php
printf 'Covers: `app/Models/User.php`\n\nSee [index](README.md).\n' >docs/users.md; echo idx >docs/README.md; git add -A
check "docs: specific Covers and good links pass" 'python3 -I "$d" --require-covers'
printf 'Covers: `app/`\n' >docs/all.md; git add -A
check "docs: a whole top-level folder in Covers fails" '! python3 -I "$d"'
printf 'Covers: `app/Gone.php`\n' >docs/all.md; git add -A
check "docs: a missing Covers path fails" '! python3 -I "$d"'
printf 'no covers, [x](nope.md)\n' >docs/all.md; git add -A
check "docs: a broken link fails" 'python3 -I "$d" | grep -q "broken link"'
check "docs: --require-covers fails a doc without Covers" 'python3 -I "$d" --require-covers | grep -q "no Covers"'
printf 'e2e:\n\techo e2e\n' >Makefile; printf 'Covers: `app/Models/User.php` (user side), `app/Models/` (structure only)\n' >docs/all.md; printf 'Run `make e2e`.\n' >CLAUDE.md; git add -A
check "docs: a make target with digits passes" '! python3 -I "$d" | grep -q "make e"'
check "docs: a note after a backticked Covers path is not part of the path" '! python3 -I "$d" | grep -q "does not exist"'
printf 'Run `make e3e`.\n' >CLAUDE.md; git add -A
check "docs: an unknown make target with digits fails" 'python3 -I "$d" | grep -q "make e3e"'
printf 'This repo has no `docs/prds/`.\n' >CLAUDE.md; git add -A
check "docs: naming the absent docs/prds/ is not a missing path" 'python3 -I "$d"'

check "the plugin's own .git/config is unchanged" '[ "$(shasum -a 256 <"$real_cfg")" = "$real_sum" ]'
echo "$((n - fails))/$n passed"
[ $fails = 0 ]
