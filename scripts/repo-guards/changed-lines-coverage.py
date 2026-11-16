#!/usr/bin/env python3
# blubird repo guard changed-lines-coverage.py, plugin 0.24.1, body sha256 4505dd66612d646503f18ba80925b8d024865150971c84de6a26ab69e0ccd90f
# Copy as is. Body = line 4 on (tail -n +4 | shasum -a 256); a copy with another hash is reported, then updated after a yes.
"""Fail when the lines this branch changed are not covered enough.

Usage: python3 -I changed-lines-coverage.py [--base <rev>] [--min <percent>] <report>... -- <pathspec>...
  --base    default origin/<BASE_REF or staging>; diffed as <base>...HEAD (three dots, as CI does),
            renames followed, so a moved file's untouched lines never count as changed
  --min     default 80
  report    lcov (.info), clover or cobertura XML; several (one per shard) are merged per line
  pathspec  required: the source files the reports measure (for example '*.php' ':!config/');
            every changed file it matches must be in a report, so a path mismatch fails, never passes
Paths are compared after resolving symlinks, so a report written under a symlinked temp or checkout
path still matches. A missing base, or a missing, empty or unreadable report, fails.
"""
import codecs
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET


def fail(msg, code=1):
    print(msg)
    sys.exit(code)


def git(*args):
    r = subprocess.run(["git", "-c", "core.quotePath=false", *args], capture_output=True, text=True)
    if r.returncode:
        fail(f"git {' '.join(args)} failed: {r.stderr.strip()}")
    return r.stdout


args = sys.argv[1:]
if "--" not in args:
    fail(__doc__, 2)
i = args.index("--")
args, spec = args[:i], args[i + 1:]
base = "origin/" + os.environ.get("BASE_REF", "staging")
minimum = 80.0
reports = []
while args:
    a = args.pop(0)
    if a == "--base" and args:
        base = args.pop(0)
    elif a == "--min" and args:
        minimum = float(args.pop(0))
    elif a.startswith("-"):
        fail(__doc__, 2)
    else:
        reports.append(a)
if not reports or not spec:
    fail(__doc__, 2)

root = os.path.realpath(git("rev-parse", "--show-toplevel").strip())
git("rev-parse", "--verify", "--quiet", base + "^{commit}")

hits = {}  # repo-relative path -> {line: hits}, merged across reports: covered in any shard = covered


def file_hits(path):
    full = os.path.realpath(path.strip() if os.path.isabs(path.strip()) else os.path.join(root, path.strip()))
    return hits.setdefault(os.path.relpath(full, root), {}) if full.startswith(root + os.sep) else {}


def add(f, line, count):
    f[line] = max(f.get(line, 0), count)


for rep in reports:
    if not os.path.isfile(rep) or os.path.getsize(rep) == 0:
        fail(f"coverage report missing or empty: {rep}")
    with open(rep, encoding="utf-8", errors="replace") as fh:
        text = fh.read()
    if not text.lstrip().startswith("<"):  # lcov
        f = {}
        for line in text.splitlines():
            if line.startswith("SF:"):
                f = file_hits(line[3:])
            elif line.startswith("DA:"):
                n, c = line[3:].split(",")[:2]
                add(f, int(n), int(float(c)))
        continue
    try:
        tree = ET.fromstring(text)
    except ET.ParseError as e:
        fail(f"coverage report unreadable: {rep}: {e}")
    if tree.find("project") is None:  # cobertura: filenames relative to a <source>
        sources = [s.text.strip() for s in tree.iter("source") if s.text] or [root]
        for cls in tree.iter("class"):
            cands = [os.path.join(root, s, cls.get("filename", "")) for s in sources]
            f = file_hits(next((c for c in cands if os.path.exists(c)), cands[0]))
            for ln in cls.iter("line"):
                add(f, int(ln.get("number")), int(ln.get("hits", 0)))
    else:  # clover
        for el in tree.iter("file"):
            f = file_hits(el.get("path") or el.get("name", ""))
            for ln in el.iter("line"):
                if ln.get("type", "stmt") != "method":
                    add(f, int(ln.get("num")), int(ln.get("count", 0)))

# Added lines per file. Fixed prefixes and no external diff, so diff.noprefix and the like change nothing.
diff = git("diff", "--no-color", "--no-ext-diff", "-M", "-U0", "--src-prefix=a/", "--dst-prefix=b/",
           f"{base}...HEAD", "--", *spec)
changed, cur = {}, None
for line in diff.splitlines():
    if line.startswith("+++ "):
        p = line[4:].rstrip("\t")  # git ends a path holding a space with a tab
        if p.startswith('"'):
            p = codecs.escape_decode(p[1:-1].encode())[0].decode("utf-8")
        cur = None if p == "/dev/null" else p[2:]
    elif line.startswith("@@") and cur:
        m = re.match(r"@@ -\S+ \+(\d+)(?:,(\d+))? @@", line)
        start, count = int(m.group(1)), int(m.group(2) or 1)
        changed.setdefault(cur, set()).update(range(start, start + count))

problems, measured, covered = [], 0, 0
for path, lines in sorted(changed.items()):
    if path not in hits:
        problems.append(f"{path}: changed but in no coverage report")
        continue
    exe = [n for n in sorted(lines) if n in hits[path]]
    miss = [n for n in exe if hits[path][n] == 0]
    measured += len(exe)
    covered += len(exe) - len(miss)
    if miss:
        problems.append(f"{path}: uncovered changed lines {','.join(map(str, miss))}")

pct = 100.0 * covered / measured if measured else 100.0
for p in problems:
    print(p)
if not changed:
    print("nothing to check", file=sys.stderr)
    sys.exit(0)
print(f"changed-line coverage {pct:.1f}% ({covered}/{measured}), minimum {minimum:g}%")
sys.exit(1 if pct < minimum or any("no coverage report" in p for p in problems) else 0)
