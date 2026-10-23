#!/usr/bin/env python3
"""msg-filter for git filter-branch: drop AI attribution lines from a commit message."""
import re
import sys

DROP = [
    re.compile(r"^\s*co-authored-by:.*$", re.I),
    re.compile(r"^\s*claude-session:.*$", re.I),
    re.compile(r"generated (with|by) \[?claude code", re.I),
    re.compile(r"^\s*🤖"),
    re.compile(r"^\s*https://claude\.ai/code/session_\S*\s*$", re.I),
]

lines = sys.stdin.read().split("\n")
kept = [l for l in lines if not any(p.search(l) for p in DROP)]
text = "\n".join(kept).rstrip()
sys.stdout.write(text + "\n")
