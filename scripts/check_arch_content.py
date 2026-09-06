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

Both ways of being wrong are cheap here, so the threshold is deliberately
generous: a module trimmed by less than 400 characters was edited, not gutted,
and asking for a section it does not need costs one heading.

Usage: check_arch_content.py [base-revision]   (default: origin/main)
Exit 0 when every shortened module has a home, 1 when one does not.
"""

import re
import subprocess
import sys

PATH = "docs/ARCHITECTURE.md"
# Below this, a shrunken line is an edit rather than a gutting.
SHRINK_THRESHOLD = 400


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
    try:
        opening = next(
            i for i, l in enumerate(lines) if l.strip() == "```" and i > 12
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


def main():
    base = sys.argv[1] if len(sys.argv) > 1 else "origin/main"
    old = subprocess.check_output(["git", "show", f"{base}:{PATH}"], text=True)
    new = open(PATH).read()

    before, after = tree_descriptions(old), tree_descriptions(new)
    # A section may be headed by the full tree path, by the bare filename, or
    # by a directory with its trailing slash. Normalise all three to compare.
    def keys(name):
        bare = name.rstrip("/")
        return {bare, bare + "/", bare.split("/")[-1], bare.split("/")[-1] + "/"}

    documented = set()
    for h in re.findall(r"^### (\S+)", new, re.M):
        documented |= keys(h)

    gutted = [
        (name, len(desc), len(after[name]))
        for name, desc in before.items()
        if name in after
        and len(desc) - len(after[name]) > SHRINK_THRESHOLD
        and not (keys(name) & documented)
    ]
    dropped = sorted(set(before) - set(after))

    for name, was, now in sorted(gutted, key=lambda r: r[2] - r[1]):
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
