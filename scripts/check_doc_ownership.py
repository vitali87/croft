#!/usr/bin/env python3
"""Flag functions that lost their doc comment between two revisions.

Rust attaches a `///` block to whatever item FOLLOWS it, so inserting a new
`fn` between an existing function and its doc silently hands that prose to the
newcomer. Nothing reports it: the build stays green, clippy is silent, and the
rendered rustdoc is confidently wrong rather than absent (#314).

The snapshot is undetectable - the reassigned prose is well formed and sits
above a plausible function - but the DIFF is not. A function that had a doc
comment and now has none is the exact fingerprint the insertion leaves behind,
and it is what both known instances did.

Deliberate removals are rare and are declared: put `doc-removal: <fn name>` in
a commit message on the branch.

Usage: check_doc_ownership.py <base-rev> <head-rev>
"""

import re
import subprocess
import sys

FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?fn\s+([A-Za-z_]\w*)")


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout


def documented(text):
    """Map fn name -> True when ANY definition of it carries a doc comment.

    Keyed by name because two revisions cannot be lined up by position. Where
    a file defines the same name more than once (`new` across impl blocks),
    "any documented" is the deliberately conservative reading: the check fires
    only when every definition of that name has lost its prose.
    """
    lines = text.splitlines()
    state = {}
    for i, line in enumerate(lines):
        m = FN.match(line)
        if not m:
            continue
        j = i - 1
        # Attributes sit between a doc comment and its item without detaching
        # it; a BLANK line detaches it, which is why this does not skip those.
        while j >= 0 and lines[j].lstrip().startswith("#["):
            j -= 1
        has_doc = j >= 0 and lines[j].lstrip().startswith("///")
        state[m.group(1)] = state.get(m.group(1), False) or has_doc
    return state


def main():
    base, head = sys.argv[1], sys.argv[2]
    declared = git("log", f"{base}..{head}", "--format=%B")
    exempt = set(re.findall(r"doc-removal:\s*([A-Za-z_]\w*)", declared))
    changed = [
        f for f in git("diff", "--name-only", base, head).splitlines()
        if f.endswith(".rs")
    ]
    losses = []
    for f in changed:
        before = documented(git("show", f"{base}:{f}"))
        after = documented(git("show", f"{head}:{f}"))
        for name, had_doc in before.items():
            if had_doc and name in after and not after[name] and name not in exempt:
                losses.append((f, name))
    for f, name in losses:
        print(
            f"::error file={f}::`{name}` had a doc comment at {base[:12]} and has none now. "
            "A doc block above it was most likely captured by a function inserted between the two "
            "(#314), which hands one function's prose to another with nothing failing. Restore it, "
            "or declare the removal with `doc-removal: " + f"{name}` in a commit message."
        )
    if losses:
        print(f"\n{len(losses)} function(s) lost documentation.", file=sys.stderr)
        return 1
    print(f"No documentation lost across {len(changed)} changed Rust file(s).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
