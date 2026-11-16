#!/usr/bin/env python3
# blubird repo guard check-docs.py, plugin 0.22.1, body sha256 014f66193b2322f954a16edff78338b26b877b139317f027528e95d15d125fc4
# Copy as is. Body = line 4 on (tail -n +4 | shasum -a 256); a copy with another hash is reported, then updated after a yes.
"""Fail when the docs point at something that is not there.

1. Every relative Markdown link in a tracked .md file resolves to a tracked or present file.
2. README.md and CLAUDE.md: every `make <target>` names a Makefile target, and every
   backticked path whose first segment exists at the repo root exists in full.
3. Every `Covers:` line in docs/ names paths that exist, and never a whole top-level folder,
   which would make coverage trivially complete. --require-covers also fails a doc under docs/
   (not README.md, not docs/prds/) that has no `Covers:` line.
Run from anywhere: python3 -I check-docs.py [--require-covers]
"""
import os
import re
import subprocess
import sys
from urllib.parse import unquote

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True).stdout.strip()
LINK = re.compile(r"\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
TICK = re.compile(r"`([^`\n]+)`")
MAKE = re.compile(r"\bmake ([a-z][a-z0-9_-]*)")
FENCE = re.compile(r"```.*?```", re.S)

errors = []


def tracked_md():
    out = subprocess.run(["git", "ls-files", "-z", "--", "*.md"], capture_output=True, text=True, cwd=ROOT, check=True).stdout
    return [p for p in out.split("\0") if p]


def check_links(path, text):
    base = os.path.dirname(os.path.join(ROOT, path))
    for m in LINK.finditer(FENCE.sub("", text)):
        target = m.group(1).strip("<>")
        if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.I) or target.startswith("#"):
            continue
        target = unquote(target.split("#", 1)[0])
        if target and not os.path.exists(os.path.join(base, target)):
            errors.append(f"{path}: broken link -> {m.group(1)}")


def make_targets():
    mk = os.path.join(ROOT, "Makefile")
    if not os.path.exists(mk):
        return set()
    names = set()
    for line in open(mk, encoding="utf-8"):
        m = re.match(r"^([a-z][a-z0-9_ -]*):(?!=)", line)
        if m:
            names.update(m.group(1).split())
    return names


def check_commands(path, text):
    targets = make_targets()
    for m in MAKE.finditer(text):
        if m.group(1) not in targets:
            errors.append(f"{path}: 'make {m.group(1)}' is not a Makefile target")
    roots = set(os.listdir(ROOT))
    for m in TICK.finditer(text):
        token = m.group(1).strip()
        if " " in token or "<" in token or "*" in token or "/" not in token:
            continue
        if token.rstrip("/") == "docs/prds" or token.startswith("docs/prds/"):
            continue  # product repos must not have it (claude-md-guard pointer); naming it is not a broken path
        if token.split("/", 1)[0] in roots and not os.path.exists(os.path.join(ROOT, token)):
            errors.append(f"{path}: `{token}` does not exist")


def check_covers(path, text):
    m = re.search(r"(?m)^\**Covers:?\**:?\s*(.*)$", text)
    if not m:
        if REQUIRE_COVERS and os.path.basename(path) != "README.md" and not path.startswith("docs/prds/"):
            errors.append(f"{path}: no Covers: line")
        return
    line = m.group(1)
    # Backticked paths are the paths; text outside them (e.g. "(trade side)") is a note.
    items = TICK.findall(line) if "`" in line else [re.sub(r"\s*\(.*?\)\s*$", "", i) for i in line.split(",")]
    for item in [i.strip() for i in items]:
        if not item:
            continue
        if not os.path.exists(os.path.join(ROOT, item.rstrip("/"))):
            errors.append(f"{path}: Covers `{item}` does not exist")
        elif "/" not in item.rstrip("/") and os.path.isdir(os.path.join(ROOT, item)):
            errors.append(f"{path}: Covers `{item}` is a whole top-level folder; name specific paths")


REQUIRE_COVERS = "--require-covers" in sys.argv[1:]

for md in tracked_md():
    full = os.path.join(ROOT, md)
    if not os.path.isfile(full):
        continue
    text = open(full, encoding="utf-8", errors="replace").read()
    check_links(md, text)
    if md.startswith("docs/"):
        check_covers(md, text)
    if md in ("README.md", "CLAUDE.md"):
        check_commands(md, text)

if errors:
    print("\n".join(errors))
    sys.exit(1)
print("docs references ok")
