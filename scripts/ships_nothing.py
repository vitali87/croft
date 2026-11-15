#!/usr/bin/env python3
"""Whether one file's change between two revisions ships nothing.

The release gate exempts "docs / CI / test-only changes" and encodes that as a
path list, so `src/app/tests/` and `tests/` are exempt while a change
confined to a `#[cfg(test)] mod tests` inside a shipped file is not (#461).
croft keeps most unit tests beside the code they cover, so that is the ordinary
shape of a test-only fix here: the release binary is byte-identical, and the
only way to satisfy the gate is to write user-facing notes for a change the
binary does not contain.

The two ways to be wrong are not symmetric. Waiving notes that were needed
ships a change no note describes, and with no fragment pending the
version-bump workflow releases nothing for it, so two different binaries share
a version; asking for notes that were not needed costs a line of notes. So
every uncertainty resolves to "ships": a module whose opening brace is not on
the `mod` line, a `#[cfg(test)]` on anything other than a module, a file that
is not Rust.

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
# missing it only costs a release-notes fragment.
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


# A module declared without a body, `mod name;` (its body is another file).
# Matched against a line's code, so a trailing `// comment` does not hide it.
MOD_DECLARATION = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_]\w*\s*;\s*$")

# An attribute that names a module's file, `#[path = "x.rs"]`, directly or
# through `cfg_attr`. Matched loosely: a false hit only costs an exemption.
PATH_ATTRIBUTE = re.compile(r"#!?\[.*\bpath\s*=")


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

    A block comment anywhere in a line's code yields no spans at all. The
    tracker strips strings, chars and `//` but not `/* */`, so a brace inside
    one is counted as structure and a span can swallow the shipped code below
    it. Rare enough that refusing to answer is better than parsing it. Read
    off the code with its literals removed: a glob in a string
    (`"src/**/*.rs"`) opens no comment, and a comment right after a string
    (`"x" /* { */`) still does.
    """
    lines = text.splitlines()
    kinds = classify(lines)
    stripper = BlockTracker()
    if any(kinds[i] == CODE and "/*" in stripper.feed(l) for i, l in enumerate(lines)):
        return []
    tracker = BlockTracker()
    ranges = []
    attr_at = None
    open_at = None
    for i, line in enumerate(lines):
        depth_before = tracker.depth
        # A line that starts inside a multi-line string is that string's
        # content: a `#[cfg(test)]` or `mod name;` there is text the binary
        # carries, not an item.
        in_literal = tracker.open_string is not None
        code = tracker.feed(line) if kinds[i] == CODE else ""
        if open_at is not None:
            start, outer = open_at
            if tracker.depth <= outer:
                ranges.append((start + 1, i + 1))
                open_at = None
            continue
        if in_literal:
            attr_at = None
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
        elif kinds[i] == CODE and MOD_DECLARATION.match(code):
            # `#[cfg(test)] mod name;`: the declaration is the whole span. Its
            # body is another file, which compiles only for tests too.
            ranges.append((attr_at + 1, i + 1))
        attr_at = None
    # A module still open at EOF is a miscount or a truncated file. Reporting
    # it as a range would exempt everything below the attribute.
    return ranges


def changed_lines(base, head, path, cwd=None):
    """(lines removed from base, lines added at head, the raw diff, the
    hunks as `(base_start, base_len, head_start, head_len)`), 1-indexed.

    The diff text comes back because "no hunk headers" has two causes: the
    file did not change, and git did not describe the change. `git diff -U0`
    on a file git treats as binary prints `Binary files ... differ` and no
    `@@` line at all, and inferring "unchanged" from that waives a bump.
    """
    diff = git("diff", "--no-renames", "-U0", base, head, "--", path, cwd=cwd)
    removed, added, hunks = [], [], []
    for line in diff.splitlines():
        m = HUNK.match(line)
        if not m:
            continue
        hunk = (
            int(m.group(1)),
            int(m.group(2) or 1),
            int(m.group(3)),
            int(m.group(4) or 1),
        )
        base_start, base_len, head_start, head_len = hunk
        hunks.append(hunk)
        removed.extend(range(base_start, base_start + base_len))
        added.extend(range(head_start, head_start + head_len))
    return removed, added, diff, hunks


def counterpart(n, hunks, side):
    """The line an unchanged line `n` was on the other side of the diff:
    `side` is "head" to map a head line to the base, "base" for the reverse.
    None when `n` is itself a changed line.

    With `-U0` a hunk of length 0 sits AFTER its start line, so every line
    past it shifts by what the hunk took out or put in.
    """
    shift = 0
    for base_start, base_len, head_start, head_len in hunks:
        start, length, other = (
            (head_start, head_len, base_len) if side == "head" else (base_start, base_len, head_len)
        )
        end = start + length - 1 if length else start
        if length and start <= n <= end:
            return None
        if n > end:
            shift += other - length
    return n + shift


def inside(line_no, ranges):
    return any(start <= line_no <= end for start, end in ranges)


def path_attribute_anywhere(rev, cwd=None):
    """Whether any Rust file at `rev` names a module's file with a
    `#[path]` attribute. Such a declaration can load a file under any module
    name, from anywhere, so no name-based reading of the parents can rule out
    a release build loading it. croft has none; the day it does, a whole-file
    exemption costs a line of notes instead."""
    done = subprocess.run(
        ("git", "grep", "-z", "-I", "-E", "-e", r"#!?\[.*path[[:space:]]*=", rev, "--", "*.rs"),
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    if done.returncode == 1:
        return False
    if done.returncode != 0:
        raise SystemExit(f"git grep failed: {done.stderr.strip()}")
    return any(
        PATH_ATTRIBUTE.search(hit.split("\0", 1)[-1]) for hit in done.stdout.splitlines()
    )


def declared_for_tests_only(rev, path, cwd=None):
    """Whether, at `rev`, the module in `path` is declared only for tests:
    its parent file declares it as `#[cfg(test)] mod name;` at least once and
    never otherwise, so nothing in it compiles into a release build. A test
    helper module like `src/test_git.rs` is the case.

    Anything else that names the module in a parent's code - an inline
    `mod name { .. }`, two declarations on one line, `#[cfg(test)] mod name;`
    written on one line - leaves the file counted as shipped, and so does a
    `#[path]` attribute anywhere in the tree. Literals and `//` comments are
    removed first, so a trailing comment cannot hide a release declaration
    and a string cannot fake a test-only one.
    """
    parts = Path(path).parts
    if len(parts) < 2 or parts[0] != "src" or not path.endswith(".rs"):
        return False
    if parts[-1] == "mod.rs":
        if len(parts) < 3:
            return False
        name, dir_parts = parts[-2], parts[:-2]
    else:
        name, dir_parts = Path(parts[-1]).stem, parts[:-1]
    if dir_parts == ("src",):
        if name in ("main", "lib"):
            return False
        parents = ["src/main.rs", "src/lib.rs"]
    else:
        folder = "/".join(dir_parts)
        parents = [f"{folder}.rs", f"{folder}/mod.rs"]
    if path_attribute_anywhere(rev, cwd=cwd):
        return False
    mention = re.compile(rf"\bmod\s+{re.escape(name)}\b")
    found = False
    for parent in parents:
        text = git("show", f"{rev}:{parent}", cwd=cwd, allow_missing_path=True)
        ranges = cfg_test_ranges(text)
        lines = text.splitlines()
        kinds = classify(lines)
        tracker = BlockTracker()
        for n, line in enumerate(lines, 1):
            if kinds[n - 1] != CODE:
                continue
            in_literal = tracker.open_string is not None
            code = tracker.feed(line)
            if not mention.search(code):
                continue
            if in_literal or not MOD_DECLARATION.match(code) or not inside(n, ranges):
                return False
            found = True
    return found


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
    # A module only tests compile, on every side it exists: a change to any
    # line of it ships nothing.
    sides = [rev for rev, text in ((base, base_text), (head, head_text)) if text]
    if sides and all(declared_for_tests_only(rev, path, cwd=cwd) for rev in sides):
        return True
    removed, added, diff, hunks = changed_lines(base, head, path, cwd=cwd)
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
    if not (
        _whole_span_moved(base_ranges, removed, base_lines)
        and _whole_span_moved(head_ranges, added, head_lines)
    ):
        return False
    # An attribute the diff did not touch can still change what it applies
    # to. `mod tests;` inserted under an existing `#[cfg(test)]` takes the
    # attribute off the item below, which then ships; deleting it does the
    # reverse. Every span the change touched must therefore open at an
    # untouched attribute that opened a span on the other side too, or have
    # moved with its attribute (checked above).
    return _attribute_kept_its_span(
        head_ranges, added, base_ranges, hunks, "head"
    ) and _attribute_kept_its_span(base_ranges, removed, head_ranges, hunks, "base")


def _attribute_kept_its_span(ranges, touched, other_ranges, hunks, side):
    """True when every span holding a touched line either had its attribute
    touched too (a module added or removed whole) or opens at an attribute
    that opens a span on the other side of the diff as well."""
    marked = set(touched)
    other_starts = {start for start, _ in other_ranges}
    for start, end in ranges:
        if start in marked or not any(start <= n <= end for n in marked):
            continue
        if counterpart(start, hunks, side) not in other_starts:
            return False
    return True


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
