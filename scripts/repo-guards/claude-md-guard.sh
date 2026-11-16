#!/usr/bin/env bash
# blubird repo guard claude-md-guard.sh, plugin 0.22.1, body sha256 8d7db67261330d577bd5b199ba250f81dc16b9ac467f890e992a4a63839d4491
# Copy as is. Body = line 4 on (tail -n +4 | shasum -a 256); a copy with another hash is reported, then updated after a yes.
# Guards the agent constitution (root CLAUDE.md).
#   nested: fail if any CLAUDE.md other than the root one is tracked or staged.
#   warn:   print a warning when a commit changes the root CLAUDE.md (never fails).
#   generated: fail if the staged tree holds tool-generated agent context:
#              a GitNexus block in CLAUDE.md/AGENTS.md, an AGENTS.md that is not
#              a symlink to CLAUDE.md, or other tools' agent files and skills.
#   docs-repo: fail unless .blubird/docs-repo points to a Documentation Repo whose
#              map lists this repo's remote, and no docs/prds/ is tracked here.
#   pointer:   the docs-repo check without the map (no sibling clone in CI).
set -u

mode="${1:-}"

case "$mode" in
  nested)
    nested=$(git ls-files -- '*CLAUDE.md' | grep -iE '(^|/)CLAUDE\.md$' | grep -vx 'CLAUDE.md' || true)
    if [ -n "$nested" ]; then
      echo "Only the root CLAUDE.md is allowed. Remove:"
      echo "$nested"
      echo "Put the content in docs/ and reference it from the root CLAUDE.md."
      exit 1
    fi
    ;;
  warn)
    if git diff --cached --name-only | grep -qx 'CLAUDE.md'; then
      echo "WARNING: this commit changes CLAUDE.md, the protected agent constitution."
      echo "Make sure a human asked for this change; it needs CODEOWNERS review."
    fi
    ;;
  generated)
    problems=""
    for f in CLAUDE.md AGENTS.md; do
      if git show ":$f" 2>/dev/null | grep -q '<!-- *gitnexus:start'; then
        problems="$problems$f has a GitNexus block; build the index with --index-only.\n"
      fi
    done
    agents=$(git ls-files -s -- AGENTS.md)
    if [ -n "$agents" ]; then
      if [ "${agents%% *}" != "120000" ] || [ "$(git show :AGENTS.md)" != "CLAUDE.md" ]; then
        problems="${problems}AGENTS.md must be a symlink to CLAUDE.md.\n"
      fi
    fi
    extra=$(git ls-files | grep -E '(^|/)AGENTS\.md$|(^|/)CLAUDE\.local\.md$|^\.claude/skills/[^/]*gitnexus|^\.(cursor|codex|windsurf)/|^\.cursorrules$|^\.github/copilot-instructions\.md$|^\.github/skills/' | grep -vx 'AGENTS.md' || true)
    if [ -n "$extra" ]; then
      problems="${problems}Generated or other-tool agent files are not allowed:\n$extra\n"
    fi
    if [ -n "$problems" ]; then
      printf '%b' "$problems"
      exit 1
    fi
    ;;
  docs-repo|pointer)
    # pointer: the shape only (CI has no sibling clone); docs-repo: also the map.
    problems=""
    if git ls-files -- docs/prds | grep -q .; then
      problems="docs/prds/ is not allowed here: PRDs live in the Documentation Repo.\n"
    fi
    pointer=$(git show :.blubird/docs-repo 2>/dev/null | head -n1)
    # Relative to the main checkout, so the check also holds in a worktree.
    main=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")
    remote=$(git remote get-url origin 2>/dev/null | sed -E 's#^(git@[^:]+:|[a-z]+://[^/]+/)##; s#\.git$##')
    if [ -z "$pointer" ]; then
      problems="${problems}.blubird/docs-repo is missing or empty: it holds the relative path to the Documentation Repo.\n"
    elif [ "$mode" = pointer ]; then
      :
    elif [ ! -f "$main/$pointer/.blubird/repos.json" ]; then
      problems="${problems}.blubird/docs-repo points to $pointer, which has no .blubird/repos.json. Clone the Documentation Repo there.\n"
    elif [ -z "$remote" ] || ! grep -Eq "\"repo\"[[:space:]]*:[[:space:]]*\"$remote\"" "$main/$pointer/.blubird/repos.json"; then
      problems="${problems}The repo map in $pointer does not list this repo (${remote:-no origin remote}). Add it there first.\n"
    fi
    if [ -n "$problems" ]; then
      printf '%b' "$problems"
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 nested|warn|generated|docs-repo|pointer" >&2
    exit 2
    ;;
esac
exit 0
