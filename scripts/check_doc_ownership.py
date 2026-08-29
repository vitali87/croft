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

# Every item kind a doc block can sit above. `fn` alone was the original
# scope and it let the same failure through twice in one day on `const`
# declarations: a doc block does not care what follows it, so neither can
# this. `impl` and bare `mod` are deliberately absent - an `impl` block's
# name is not unique enough to key on, and its methods are matched as `fn`
# in their own right.
ITEM = re.compile(
    # A same-line attribute (`#[cfg(test)] const A: u8 = 1;`) is unusual but
    # legal, and rustfmt leaves it alone inside macro bodies. Skipping it here
    # costs nothing and stops the item from being invisible.
    r"^\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?"
    # Rust's own qualifier order: default, const, async, unsafe, extern. The
    # optional `const` here is the one in `const fn`, and it has to sit where
    # Rust puts it: trailing the others made `pub const unsafe fn f()` match
    # nothing at all, so the function was invisible to the gate rather than
    # merely undocumented. The `const A: u8` declaration is a branch below,
    # reached when this optional one is not taken.
    r"(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:extern\s+\"[^\"]*\"\s+)?(?:"
    r"fn\s+([A-Za-z_]\w*)"
    r"|const\s+([A-Za-z_]\w*)\s*:"
    r"|static\s+(?:mut\s+)?([A-Za-z_]\w*)\s*:"
    r"|struct\s+([A-Za-z_]\w*)"
    r"|enum\s+([A-Za-z_]\w*)"
    r"|union\s+([A-Za-z_]\w*)"
    r"|trait\s+([A-Za-z_]\w*)"
    r"|type\s+([A-Za-z_]\w*)"
    r"|macro_rules!\s+([A-Za-z_]\w*)"
    r")"
)


# The one git failure this checker tolerates, in the two spellings git uses:
# the path is simply not present in that revision, because the branch added or
# deleted the file. Anything else - a bad revision, a malformed object, a
# broken repository - is a failure to report, not an empty file to accept.
MISSING_PATH = re.compile(
    r"fatal: path .*(?:does not exist in|exists on disk, but not in)", re.MULTILINE
)


def git(*args, allow_missing_path=False, allow_fail=False):
    """Run git, failing closed.

    A silent failure here is worse than a crash: an errored `git diff` yields
    an empty file list, the checker reports "no documentation lost" and exits
    zero, and the gate has passed by not running.

    `allow_missing_path` narrows that tolerance to exactly the expected case,
    and `allow_fail` is separate and narrower still: it is for a probe whose
    exit status IS the answer, and its empty return is never read as content.
    Tolerating every non-zero exit would let an invalid revision or a
    malformed object read as empty content, which is the same fail-open bug
    one door further in.
    """
    proc = subprocess.run(["git", *args], capture_output=True, text=True)
    if proc.returncode != 0:
        # `allow_fail` is for a PROBE whose non-zero exit is the answer
        # (`cat-file -e` asking whether a path exists), never for a command
        # whose output is then read as content.
        if allow_fail:
            return ""
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
    """Map item name -> True when ANY definition of it carries a doc comment.

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
        m = ITEM.match(line)
        if not m:
            continue
        # Exactly one alternative captures per match.
        name = next(g for g in m.groups() if g)
        j = i - 1
        while j >= 0:
            s = lines[j].lstrip()
            if s.startswith("#[") or s == "" or (s.startswith("//") and not is_doc(lines[j])):
                j -= 1
                continue
            break
        has_doc = j >= 0 and is_doc(lines[j])
        state[name] = state.get(name, False) or has_doc
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
    # A file the branch ADDS has no base version, so `documented(before)` is
    # empty and no loss can ever be reported in it: a doc captured within the
    # branch is invisible to a merge-base comparison. Those files are checked
    # commit-by-commit instead, which is the only place their history exists.
    added = [f for f in changed if not git("cat-file", "-e", f"{base}:{f}", allow_missing_path=True, allow_fail=True)]
    if added:
        commits = git("log", f"{base}..{head}", "--format=%H", "--reverse").split()
        for older, newer in zip(commits, commits[1:]):
            for f in added:
                before = documented(git("show", f"{older}:{f}", allow_missing_path=True))
                after = documented(git("show", f"{newer}:{f}", allow_missing_path=True))
                for name, had_doc in before.items():
                    if (
                        had_doc
                        and name in after
                        and not after[name]
                        and (f, name) not in exempt
                        and not any(l[0] == f and l[1] == name for l in losses)
                    ):
                        # Name the commit the doc was last seen at. Saying
                        # "at {base}" would be wrong for a file the branch
                        # ADDED: it does not exist there, and a reader sent
                        # to that revision finds nothing.
                        losses.append((f, name, older))
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
                losses.append((f, name, base))
    for f, name, at in losses:
        print(
            f"::error file={f}::`{name}` had a doc comment at {at[:12]} and has none now. "
            "A doc block above it was most likely captured by an item inserted between the two "
            "(#314), which hands one item's prose to another with nothing failing. Restore it, "
            "or declare the removal with `doc-removal: " + f"{f}::{name}` in a commit message."
        )
    if losses:
        print(f"\n{len(losses)} item(s) lost documentation.", file=sys.stderr)
        return 1
    print(f"No documentation lost across {len(changed)} changed Rust file(s).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
