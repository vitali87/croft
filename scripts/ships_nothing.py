#!/usr/bin/env python3
"""Whether one file's change between two revisions ships nothing.

The release gate exempts "docs / CI / test-only changes" and encodes that as a
path list, so `src/app/tests.rs` and `tests/` are exempt while a change
confined to a `#[cfg(test)] mod tests` inside a shipped file is not (#461).
croft keeps most unit tests beside the code they cover, so that is the ordinary
shape of a test-only fix here: the release binary is byte-identical, and the
only way to satisfy the gate is to bump a version and write user-facing notes
for a change the binary does not contain.

The two ways to be wrong are not symmetric. Waiving a bump that was needed puts
two different binaries on one version, which is the thing the gate exists to
prevent; asking for a bump that was not needed costs a version number. So every
uncertainty resolves to "ships": a module whose opening brace is not on the
`mod` line, a `#[cfg(test)]` on anything other than a module, a file that is not
Rust.

Usage: ships_nothing.py <base> <head> <path>
Exit 0 when the change ships nothing, 1 when it ships something.
"""

import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_doc_ownership import ATTR, CODE, MISSING_PATH, SKIP, BlockTracker, classify  # noqa: E402

# The attribute on its own line, which is what rustfmt produces and what every
# test module in this repo has. `#[cfg(test)] mod tests {` on one line is legal
# and is deliberately not matched: it is not a shape that occurs here, and
# missing it only costs a version bump.
CFG_TEST = re.compile(r"^\s*#\[cfg\(test\)\]\s*$")

MOD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_]\w*")

HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def git(*args, cwd=None):
    """Run git, tolerating only "that path is not in that revision"."""
    done = subprocess.run(
        ("git",) + args, cwd=cwd, capture_output=True, text=True
    )
    if done.returncode != 0:
        if MISSING_PATH.search(done.stderr):
            return ""
        raise SystemExit(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


def cfg_test_ranges(text):
    """Line spans (1-indexed, inclusive) of every `#[cfg(test)]` module.

    The span starts at the attribute rather than at the `mod` line, so a diff
    that only edits the attribute itself still reads as test-only. Braces are
    counted through `BlockTracker`, which removes string and char literals
    first: a `format!("{}")` inside a test would otherwise close the module
    early and leave the rest of the file looking like shipped code.
    """
    lines = text.splitlines()
    kinds = classify(lines)
    tracker = BlockTracker()
    ranges = []
    attr_at = None
    open_at = None
    for i, line in enumerate(lines):
        depth_before = tracker.depth
        if kinds[i] == CODE:
            tracker.feed(line)
        if open_at is not None:
            start, outer = open_at
            if tracker.depth <= outer:
                ranges.append((start + 1, i + 1))
                open_at = None
            continue
        if CFG_TEST.match(line):
            attr_at = i
            continue
        if attr_at is None:
            continue
        if kinds[i] in (SKIP, ATTR):
            # Blank lines, comments and further attributes sit legally
            # between `#[cfg(test)]` and the module it applies to.
            continue
        if MOD.match(line) and tracker.depth > depth_before:
            open_at = (attr_at, depth_before)
        attr_at = None
    # A module still open at EOF is a miscount or a truncated file. Reporting
    # it as a range would exempt everything below the attribute.
    return ranges


def changed_lines(base, head, path, cwd=None):
    """(lines removed from base, lines added at head), 1-indexed."""
    diff = git("diff", "--no-renames", "-U0", base, head, "--", path, cwd=cwd)
    removed, added = [], []
    for line in diff.splitlines():
        m = HUNK.match(line)
        if not m:
            continue
        base_start, base_len, head_start, head_len = (
            int(m.group(1)),
            int(m.group(2) or 1),
            int(m.group(3)),
            int(m.group(4) or 1),
        )
        removed.extend(range(base_start, base_start + base_len))
        added.extend(range(head_start, head_start + head_len))
    return removed, added


def inside(line_no, ranges):
    return any(start <= line_no <= end for start, end in ranges)


def ships_nothing(base, head, path, cwd=None):
    """True when every line this change touches is inside a test module.

    Both sides of the diff are read. A deletion leaves no line at the head to
    inspect, so a head-only check would waive the removal of shipped code as
    readily as the removal of a test.
    """
    if not path.endswith(".rs"):
        return False
    removed, added = changed_lines(base, head, path, cwd=cwd)
    if not removed and not added:
        return True
    base_ranges = cfg_test_ranges(git("show", f"{base}:{path}", cwd=cwd))
    head_ranges = cfg_test_ranges(git("show", f"{head}:{path}", cwd=cwd))
    return all(inside(n, base_ranges) for n in removed) and all(
        inside(n, head_ranges) for n in added
    )


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: ships_nothing.py <base> <head> <path>")
    base, head, path = sys.argv[1:4]
    return 0 if ships_nothing(base, head, path) else 1


if __name__ == "__main__":
    sys.exit(main())
