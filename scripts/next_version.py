#!/usr/bin/env python3
"""croft's version numbering: an odometer whose components stay below 1000.

The same rule code-graph-rag's version-bump workflow applies. MINOR and PATCH
never reach CAP: a bump that would take one there carries into the next
component and resets the lower ones to 0, so 0.1.999 is followed by 0.2.0 and
0.999.999 by 1.0.0. A version already past the cap (0.1.1244 was, before the
rule existed) carries on its next bump the same way, so the patch after
0.1.1244 is 0.2.0, not 0.1.1245.

The release gate in `.github/workflows/ci.yml` runs `--check` on a PR's
version, which is what makes the rule binding for croft's hand-picked bumps.

Usage:
  next_version.py <current> [patch|minor|major]   print the version after <current>
  next_version.py --check <version>               exit 1 unless every component is below CAP

Exit status 2 means the input itself was refused (malformed version, unknown
bump type, bad arguments).
"""

from __future__ import annotations

import re
import sys

CAP = 1000

# Three plain ASCII numbers and nothing else: `fullmatch` rejects a trailing
# newline that `$` would let through, and `[0-9]` rejects the non-ASCII digits
# that `\d` accepts and `int` would then quietly parse.
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")


def parse(version: str) -> tuple[int, int, int]:
    """Split `major.minor.patch` into numbers, refusing anything else."""
    if not VERSION.fullmatch(version):
        raise ValueError(f"refusing malformed version {version!r}")
    major, minor, patch = (int(part) for part in version.split("."))
    return major, minor, patch


def within_cap(version: str) -> bool:
    """Whether MINOR and PATCH are both below CAP."""
    _, minor, patch = parse(version)
    return minor < CAP and patch < CAP


def bump(current: str, kind: str = "patch") -> str:
    """The version after `current` for a `kind` bump, carried at CAP."""
    major, minor, patch = parse(current)
    if kind == "major":
        major, minor, patch = major + 1, 0, 0
    elif kind == "minor":
        minor, patch = minor + 1, 0
    elif kind == "patch":
        patch += 1
    else:
        raise ValueError(f"unknown bump type {kind!r}")
    if patch >= CAP:
        minor, patch = minor + 1, 0
    if minor >= CAP:
        major, minor, patch = major + 1, 0, 0
    return f"{major}.{minor}.{patch}"


def main(argv: list[str]) -> int:
    try:
        if len(argv) == 2 and argv[0] == "--check":
            if within_cap(argv[1]):
                return 0
            print(
                f"{argv[1]} has a component at or above {CAP}",
                file=sys.stderr,
            )
            return 1
        if len(argv) in (1, 2) and not argv[0].startswith("-"):
            print(bump(*argv))
            return 0
    except ValueError as err:
        print(err, file=sys.stderr)
        return 2
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
