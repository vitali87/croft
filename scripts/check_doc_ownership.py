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

Deliberate removals are rare and are declared: put
`doc-removal: <path>::<fn name>` in a commit message on the branch. The path
qualifier keeps one declared removal from excusing a same-named function
elsewhere.

Usage: check_doc_ownership.py <base-rev> <head-rev>
"""

import re
import subprocess
import sys

FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?fn\s+([A-Za-z_]\w*)")


# The one git failure this checker tolerates, in the two spellings git uses:
# the path is simply not present in that revision, because the branch added or
# deleted the file. Anything else - a bad revision, a malformed object, a
# broken repository - is a failure to report, not an empty file to accept.
MISSING_PATH = re.compile(
    r"fatal: path .*(?:does not exist in|exists on disk, but not in)", re.MULTILINE
)


def git(*args, allow_missing_path=False):
    """Run git, failing closed.

    A silent failure here is worse than a crash: an errored `git diff` yields
    an empty file list, the checker reports "no documentation lost" and exits
    zero, and the gate has passed by not running.

    `allow_missing_path` narrows that tolerance to exactly the expected case.
    Tolerating every non-zero exit would let an invalid revision or a
    malformed object read as empty content, which is the same fail-open bug
    one door further in.
    """
    proc = subprocess.run(["git", *args], capture_output=True, text=True)
    if proc.returncode != 0:
        if allow_missing_path and MISSING_PATH.search(proc.stderr):
            return ""
        raise SystemExit(
            f"git {' '.join(args)} failed ({proc.returncode}): {proc.stderr.strip()}"
        )
    return proc.stdout


def is_doc(line):
    """A `///` outer doc comment, and not a `////` rule.

    Four or more slashes is an ordinary comment to rustc, so counting it as
    documentation invents losses that never happened.
    """
    s = line.lstrip()
    return s.startswith("///") and not s.startswith("////")


def documented(text):
    """Map fn name -> True when ANY definition of it carries a doc comment.

    Keyed by name because two revisions cannot be lined up by position. Where
    a file defines the same name more than once (`new` across impl blocks),
    "any documented" is the deliberately conservative reading: the check fires
    only when every definition of that name has lost its prose.

    `///` lowers to an outer `#[doc]` attribute, and an attribute is not
    detached from its item by blank lines or ordinary comments - verified
    against rustc, which warns `unused_doc_comments` when a doc really is
    orphaned and stays silent here, and against rustdoc, which renders the
    prose on the function in both shapes. So the backward scan steps over
    attributes, blanks and `//` comments alike; stopping at the first blank
    reported documentation as missing when rustc could see it perfectly well.
    """
    lines = text.splitlines()
    state = {}
    for i, line in enumerate(lines):
        m = FN.match(line)
        if not m:
            continue
        j = i - 1
        while j >= 0:
            s = lines[j].lstrip()
            if s.startswith("#[") or s == "" or (s.startswith("//") and not is_doc(lines[j])):
                j -= 1
                continue
            break
        has_doc = j >= 0 and is_doc(lines[j])
        state[m.group(1)] = state.get(m.group(1), False) or has_doc
    return state


def main():
    base, head = sys.argv[1], sys.argv[2]
    declared = git("log", f"{base}..{head}", "--format=%B")
    # File-qualified: `doc-removal: src/foo.rs::bar`. Keyed on the bare name,
    # one deliberate removal of `new` would excuse every `new` in every
    # changed file, including an accidental loss elsewhere in the same branch.
    exempt = {
        (path, name)
        for path, name in re.findall(
            r"doc-removal:\s*([\w./-]+\.rs)::([A-Za-z_]\w*)", declared
        )
    }
    changed = [
        f for f in git("diff", "--name-only", base, head).splitlines()
        if f.endswith(".rs")
    ]
    losses = []
    for f in changed:
        # A path can legitimately be absent on one side: the branch added the
        # file, or deleted it. That is the one git failure this tolerates -
        # `allow_fail` exists for exactly it, and not passing it here made the
        # gate abort on every PR that adds a Rust file, which is how it failed
        # on the first one that did.
        before = documented(git("show", f"{base}:{f}", allow_missing_path=True))
        after = documented(git("show", f"{head}:{f}", allow_missing_path=True))
        for name, had_doc in before.items():
            if had_doc and name in after and not after[name] and (f, name) not in exempt:
                losses.append((f, name))
    for f, name in losses:
        print(
            f"::error file={f}::`{name}` had a doc comment at {base[:12]} and has none now. "
            "A doc block above it was most likely captured by a function inserted between the two "
            "(#314), which hands one function's prose to another with nothing failing. Restore it, "
            "or declare the removal with `doc-removal: " + f"{f}::{name}` in a commit message."
        )
    if losses:
        print(f"\n{len(losses)} function(s) lost documentation.", file=sys.stderr)
        return 1
    print(f"No documentation lost across {len(changed)} changed Rust file(s).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
