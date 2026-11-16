#!/usr/bin/env bash
# blubird repo guard coverage-log-guard.sh, plugin 0.22.1, body sha256 fc7444437908d995682f04d767bb6c0304a70f5a17c1546cf0c4abc171decd7f
# Copy as is. Body = line 4 on (tail -n +4 | shasum -a 256); a copy with another hash is reported, then updated after a yes.
# A coverage gate that did not really run counts as failed.
# Usage: coverage-log-guard.sh <coverage run log> <report file>...
# Fails when a report is missing or empty, or when the log shows a file left out of coverage
# (the tool could not parse it, or excluded it), so a gate never passes on partial data.
set -u
[ $# -ge 2 ] || { echo "usage: $0 <log> <report>..." >&2; exit 2; }
log=$1; shift
fail=0
for report in "$@"; do
  [ -s "$report" ] || { echo "coverage report missing or empty: $report"; fail=1; }
done
[ -r "$log" ] || { echo "coverage log not readable: $log"; exit 1; }
hits=$(grep -niE 'failed to parse|excluding it from coverage|could not (parse|instrument)|skipp(ed|ing) coverage|no coverage (data|driver)' "$log" | head -20)
if [ -n "$hits" ]; then
  echo "coverage left files out (fix them, never ignore):"
  printf '%s\n' "$hits"
  fail=1
fi
[ $fail = 0 ] && echo "coverage report complete"
exit $fail
