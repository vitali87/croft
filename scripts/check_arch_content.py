#!/usr/bin/env python3
"""Whether ARCHITECTURE.md's restructure dropped any module's reasoning.

The project-layout tree once carried each module's whole design rationale on
its own line, some of them thousands of characters long inside a code fence.
That is not an index any more, so the reasoning moved into `### <module>`
prose sections and the tree kept a short label. The move is only safe if every
shortened line has a section to land in.

The check that misses this is the tempting one. Confirming every module ROW
still exists, or that every `Foo::bar` identifier still appears somewhere,
passes while the reasoning AFTER a row is cut away: the row survives, the
identifier survives in its short label, and a thousand characters of why-it-is-
this-way is gone. That check cannot fail in the case it is needed for. This one
compares each module's description LENGTH against the base revision and asks
for a prose section whenever it shrank substantially.

Usage: check_arch_content.py [base-revision]   (default: origin/main)
Exit 0 when every shortened module has a home, 1 when one does not.
"""

import collections
import re
import subprocess
import sys

PATH = "docs/ARCHITECTURE.md"
# A line that lost this many characters lost a sentence, not a comma.
#
# Set from the corpus, not from a round number. The first pass used 400 on
# the reasoning that both ways of being wrong are cheap, and 400 turned out
# to sit just above a cluster of real losses - the largest unguarded shrink
# was 353 characters, so the gate was tuned to pass. Measure before picking:
# a threshold chosen without looking at the distribution it guards can admit
# every case it exists to catch and still report success.
SHRINK_THRESHOLD = 120


def tree_descriptions(text):
    """Map tree PATH -> description, for every row of the layout tree.

    Keyed on the path, not the filename. Nine basenames repeat in this tree
    (`registry.rs` four times, `mod.rs` three, `output.rs`, `remote.rs`,
    `session.rs`, `hover.rs`, `transport.rs`, `install.rs` and `client.rs`
    twice each), so a dict keyed on the name alone keeps only the last of each
    and leaves twelve rows unchecked - including, exactly, the gutting this
    script exists to catch.
    """
    lines = text.split("\n")
    # Anchor on the heading, not on a line number. Keying the search off a
    # bare `i > 12` meant the parse depended on how much prose happened to
    # sit above the tree: adding a fenced example to an earlier section
    # would silently retarget it at that fence and compare unrelated text.
    try:
        heading = next(
            i for i, l in enumerate(lines) if l.strip() == "## Project layout"
        )
        opening = next(
            i for i in range(heading + 1, len(lines)) if lines[i].strip() == "```"
        )
        closing = next(
            i for i in range(opening + 1, len(lines)) if lines[i].strip() == "```"
        )
    except StopIteration:
        sys.exit(f"{PATH}: no project-layout code fence found")
    out = {}
    stack = []  # (indent depth, directory name) for the directories we are inside
    for line in lines[opening + 1 : closing]:
        m = re.match(r"^([│├└─\s]*)(\S+\.rs|\S+/)\s+(.*)$", line)
        if not m:
            # A bare directory row carries no description but still nests.
            d = re.match(r"^([│├└─\s]*)(\S+/)\s*$", line)
            if d:
                depth = len(d.group(1))
                while stack and stack[-1][0] >= depth:
                    stack.pop()
                stack.append((depth, d.group(2).rstrip("/")))
            continue
        indent, name, desc = len(m.group(1)), m.group(2), m.group(3)
        while stack and stack[-1][0] >= indent:
            stack.pop()
        path = "/".join([d for _, d in stack] + [name.rstrip("/")])
        out[path] = desc
        if name.endswith("/"):
            stack.append((indent, name.rstrip("/")))
    return out


def section_keys(name, ambiguous=frozenset()):
    """The names a `### ` heading may use for this module.

    The full path always counts. The bare filename counts only when it names
    exactly one row: `session.rs` and `dap/session.rs` both live in this tree,
    and a single `### session.rs` heading was accepted as covering both, so one
    of them kept a lost fact while the gate reported success.
    """
    bare = name.rstrip("/")
    parts = bare.split("/")
    keys = set()
    # Every trailing slice of the path - `src/lsp/install.rs`, `lsp/install.rs`,
    # `install.rs` - except a bare filename two rows share.
    for i in range(len(parts)):
        suffix = "/".join(parts[i:])
        if "/" not in suffix and suffix in ambiguous:
            continue
        keys |= {suffix, suffix + "/"}
    return keys


def compare(old, new):
    """(shrunk-with-no-section, dropped-rows) between two versions of the doc."""
    before, after = tree_descriptions(old), tree_descriptions(new)
    # A basename carried by more than one row cannot stand in for a path.
    seen = collections.Counter(p.rstrip("/").split("/")[-1] for p in before)
    ambiguous = frozenset(leaf for leaf, n in seen.items() if n > 1)

    documented = set()
    for heading in re.findall(r"^### (\S+)", new, re.M):
        documented |= section_keys(heading, ambiguous)

    shrunk = [
        (name, len(desc), len(after[name]))
        for name, desc in before.items()
        if name in after
        and len(desc) - len(after[name]) > SHRINK_THRESHOLD
        and not (section_keys(name, ambiguous) & documented)
    ]
    return sorted(shrunk, key=lambda r: r[2] - r[1]), sorted(set(before) - set(after))


def main():
    base = sys.argv[1] if len(sys.argv) > 1 else "origin/main"
    old = subprocess.check_output(["git", "show", f"{base}:{PATH}"], text=True)
    gutted, dropped = compare(old, open(PATH).read())

    for name, was, now in gutted:
        print(f"{name}: {was} -> {now} chars, with no `### {name}` section")
    for name in dropped:
        print(f"{name}: row gone from the tree entirely")

    if gutted or dropped:
        print(
            f"\n{len(gutted)} module(s) lost reasoning with nowhere to put it, "
            f"{len(dropped)} row(s) dropped.",
            file=sys.stderr,
        )
        return 1
    print("every shortened module has a prose section; no rows dropped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
