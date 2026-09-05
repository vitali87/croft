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

from check_doc_ownership import (  # noqa: E402
    ATTR,
    CODE,
    DOC,
    MISSING_PATH,
    SKIP,
    BlockTracker,
    classify,
)

# The attribute on its own line, which is what rustfmt produces and what every
# test module in this repo has. `#[cfg(test)] mod tests {` on one line is legal
# and is deliberately not matched: it is not a shape that occurs here, and
# missing it only costs a version bump.
CFG_TEST = re.compile(r"^\s*#\[cfg\(test\)\]\s*$")

MOD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_]\w*")

HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def git(*args, cwd=None, allow_missing_path=False):
    """Run git, tolerating "that path is not in that revision" only where the
    caller says a missing path is a legitimate answer.

    Opt-in for the same reason `check_doc_ownership.git` is: tolerating it
    everywhere lets an invalid revision or a malformed object read as empty
    content, and empty content is the WAIVING answer here.
    """
    done = subprocess.run(
        ("git",) + args, cwd=cwd, capture_output=True, text=True
    )
    if done.returncode != 0:
        if allow_missing_path and MISSING_PATH.search(done.stderr):
            return ""
        raise SystemExit(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


# A `/*` that opens mid-line. `BlockTracker` strips strings, chars and `//`
# but not block comments, so a brace inside one is counted as structure and a
# span can swallow the shipped code below it. Rare enough (no instance in
# `src/` today) that refusing to answer is better than parsing it.
MID_LINE_BLOCK_COMMENT = re.compile(r"\S.*/\*")


def literal_lines(text):
    """Line numbers (1-indexed) that sit INSIDE a multi-line string literal.

    A blank line is only whitespace when it is code. Inside a raw string it is
    content: croft's model prompt and its tree-sitter queries are multi-line
    literals, and adding or removing a blank line there changes what the
    binary does. `BlockTracker` already tracks the open-literal state for its
    brace counting, so this reads it rather than re-deriving it.
    """
    lines = text.splitlines()
    kinds = classify(lines)
    tracker = BlockTracker()
    inside = set()
    for i, line in enumerate(lines):
        if tracker.open_string is not None:
            inside.add(i + 1)
        if kinds[i] == CODE:
            tracker.feed(line)
    return inside


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
    if any(kinds[i] == CODE and MID_LINE_BLOCK_COMMENT.search(l) for i, l in enumerate(lines)):
        return []
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
        if kinds[i] in (SKIP, ATTR, DOC):
            # Blank lines, comments, doc comments and further attributes all
            # sit legally between `#[cfg(test)]` and the module it applies
            # to. `classify` labels `///` DOC rather than SKIP, and a
            # documented test module is an ordinary shape: missing it loses
            # the exemption silently rather than waiving wrongly.
            continue
        if MOD.match(line) and tracker.depth > depth_before:
            open_at = (attr_at, depth_before)
        attr_at = None
    # A module still open at EOF is a miscount or a truncated file. Reporting
    # it as a range would exempt everything below the attribute.
    return ranges


def changed_lines(base, head, path, cwd=None):
    """(lines removed from base, lines added at head, the raw diff), 1-indexed.

    The diff text comes back because "no hunk headers" has two causes: the
    file did not change, and git did not describe the change. `git diff -U0`
    on a file git treats as binary prints `Binary files ... differ` and no
    `@@` line at all, and inferring "unchanged" from that waives a bump.
    """
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
    return removed, added, diff


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
    base_text = git("show", f"{base}:{path}", cwd=cwd, allow_missing_path=True)
    head_text = git("show", f"{head}:{path}", cwd=cwd, allow_missing_path=True)
    removed, added, diff = changed_lines(base, head, path, cwd=cwd)
    if not removed and not added and diff.strip():
        # git described a change it did not give hunk headers for - a file it
        # treats as binary. Nothing here can read that, so it ships.
        return False
    if not removed and not added:
        # An empty diff means "nothing changed" for a path that exists at both
        # ends, and "git could not resolve that pathspec" otherwise. Only the
        # first is a reason to waive a bump, and the second is reachable: a
        # path that matches nothing makes `git diff` exit 0 with no output.
        return bool(base_text) and bool(head_text)
    base_ranges = cfg_test_ranges(base_text)
    head_ranges = cfg_test_ranges(head_text)
    base_lines = base_text.splitlines()
    head_lines = head_text.splitlines()

    base_literals = literal_lines(base_text)
    head_literals = literal_lines(head_text)

    def blank(lines, n, literals):
        """A line that carries no CODE cannot change the binary, whichever
        side of the diff it is on: adding or removing the blank line above a
        new test module is the common case. Inside a string literal it is not
        whitespace at all but content, and croft has 42 such lines in `src/`
        - the model's system prompt and the tree-sitter queries - where a
        blank line changes what the binary does."""
        return 1 <= n <= len(lines) and not lines[n - 1].strip() and n not in literals

    removed = [n for n in removed if not blank(base_lines, n, base_literals)]
    added = [n for n in added if not blank(head_lines, n, head_literals)]
    if not all(inside(n, base_ranges) for n in removed):
        return False
    if not all(inside(n, head_ranges) for n in added):
        return False
    # The `#[cfg(test)]` line is what DECIDES whether the module compiles, and
    # it sits inside the span it opens, so a diff that only adds or removes it
    # falls inside the span and would be waived. Both directions change the
    # binary: adding the attribute takes a shipping module out, removing it
    # puts a test module in. A touched attribute is therefore only test-only
    # when its WHOLE module moved with it, which is what a test module added
    # or deleted in one piece looks like.
    return _whole_span_moved(base_ranges, removed, base_lines) and _whole_span_moved(
        head_ranges, added, head_lines
    )


def _whole_span_moved(ranges, touched, lines):
    """True unless a span's attribute line was touched without the rest.

    Blank lines inside the span are not required to have moved: they were
    already dropped from `touched` above, and every one of the 159
    `#[cfg(test)]` modules in `src/` contains at least one, so demanding them
    made a whole module added or deleted in one piece - the case this
    exemption exists for - unreachable for any module shaped like the ones
    already here.
    """
    marked = set(touched)
    return all(
        start not in marked
        or all(n in marked for n in range(start, end + 1) if lines[n - 1].strip())
        for start, end in ranges
    )


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: ships_nothing.py <base> <head> <path>")
    base, head, path = sys.argv[1:4]
    return 0 if ships_nothing(base, head, path) else 1


if __name__ == "__main__":
    sys.exit(main())
