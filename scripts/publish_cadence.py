#!/usr/bin/env python3
"""Decide whether main's head is due for crates.io.

croft bumps its version on every shipped merge (the release-hygiene gate in
ci.yml), so "publish every N bumps" means: count the versions main has
carried since the newest one crates.io already holds, and publish when that
count reaches N. The same cadence code-graph-rag's version-bump workflow
applies to PyPI, with one difference in where the count comes from. That
repo counts tags since its latest GitHub release; croft tags only what it
publishes, so the anchor here is the crates.io index itself and the history
is Cargo.toml's version line along main's first-parent chain. Neither is a
counter stored in the repo, so a repeated run reaches the same answer.

Usage (CI):
  publish_cadence.py --every N --head <Cargo.toml version> --from-git --fetch [--force]

Usage (tests, offline):
  publish_cadence.py --every N --head V --history FILE --published FILE [--force]

  --history   one version per line, newest first (what --from-git produces)
  --published the crate's sparse-index file, one JSON object per line (what
              --fetch downloads)

Prints `key=value` lines ready for $GITHUB_OUTPUT:

  version=            main's head version
  latest_published=   the newest crates.io version found in main's history
  bumps=              versions main carried after it, head included
  already_published=  whether head itself is on crates.io
  publish=            bumps >= N, or --force

Exit status 2 means the inputs were refused: a malformed version, a history
that does not start with --head (read from the wrong commit, or truncated),
or a history in which no published version appears (a shallow clone). Every
refusal prints nothing on stdout, so a partial answer never reaches the job.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from next_version import VERSION  # noqa: E402

CRATE = "croft-software"
INDEX = "https://index.crates.io"
# `git log -p` prints an added line as `+version = "..."`; nothing else in
# Cargo.toml starts a line with `version =`, dependencies name themselves.
ADDED_VERSION = '+version = "'


class CadenceError(Exception):
    """An input the decision must not be made from."""


def parse_history(text: str) -> list[str]:
    """The distinct versions main has carried, newest first."""
    seen: list[str] = []
    for line in text.splitlines():
        version = line.strip()
        if not version:
            continue
        if not VERSION.fullmatch(version):
            raise CadenceError(f"malformed version in history: {version!r}")
        if version not in seen:
            seen.append(version)
    if not seen:
        raise CadenceError("history is empty")
    return seen


def parse_index(text: str) -> set[str]:
    """Every version the crates.io index lists, yanked ones included.

    A yanked version was still published, and the cadence counts publishes.
    """
    versions: set[str] = set()
    for number, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError as err:
            raise CadenceError(f"index line {number} is not JSON: {err}") from err
        version = entry.get("vers") if isinstance(entry, dict) else None
        if not isinstance(version, str) or not VERSION.fullmatch(version):
            raise CadenceError(f"index line {number} has no usable vers: {line!r}")
        versions.add(version)
    if not versions:
        raise CadenceError("index lists no versions")
    return versions


def index_path(name: str) -> str:
    """Where the sparse index keeps a crate, by the registry's layout rule."""
    if not name or not all(c.isalnum() or c in "-_" for c in name):
        raise CadenceError(f"not a crate name: {name!r}")
    name = name.lower()
    if len(name) == 1:
        return f"1/{name}"
    if len(name) == 2:
        return f"2/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def bumps_since(history: list[str], published: set[str]) -> tuple[int, str]:
    """How many versions main carried after its newest published one.

    Walks newest first and stops at the first version crates.io holds, so
    an older publish never shortens the count. Head itself counts as a bump
    unless it is the published one.
    """
    for count, version in enumerate(history):
        if version in published:
            return count, version
    raise CadenceError(
        "no published version appears in main's history; refusing to guess"
    )


def due(bumps: int, every: int) -> bool:
    """Whether `bumps` versions since the last publish reaches the cadence."""
    if every < 1:
        raise CadenceError(f"cadence must be at least 1, got {every}")
    return bumps >= every


def history_from_git(repo: Path) -> str:
    """Cargo.toml's version line at every first-parent step of HEAD's chain."""
    log = subprocess.run(
        ["git", "log", "--first-parent", "-p", "--format=", "--", "Cargo.toml"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return "".join(
        line[len(ADDED_VERSION) :].split('"', 1)[0] + "\n"
        for line in log.splitlines()
        if line.startswith(ADDED_VERSION)
    )


def fetch_index(name: str) -> str:
    request = urllib.request.Request(
        f"{INDEX}/{index_path(name)}",
        headers={"User-Agent": "croft publish_cadence (github.com/vitali87/croft)"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:  # noqa: S310
        return response.read().decode("utf-8")


def decide(
    history: list[str], published: set[str], head: str, every: int, force: bool
) -> dict[str, str]:
    if history[0] != head:
        raise CadenceError(
            f"history starts at {history[0]}, not at head version {head}; "
            "it was read from the wrong commit or is truncated"
        )
    bumps, latest = bumps_since(history, published)
    return {
        "version": head,
        "latest_published": latest,
        "bumps": str(bumps),
        "already_published": str(head in published).lower(),
        "publish": str(force or due(bumps, every)).lower(),
    }


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--every", type=int, required=True)
    parser.add_argument("--head", required=True, help="Cargo.toml's version")
    parser.add_argument("--force", action="store_true")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--history", type=Path)
    source.add_argument("--from-git", action="store_true")
    index = parser.add_mutually_exclusive_group(required=True)
    index.add_argument("--published", type=Path)
    index.add_argument("--fetch", action="store_true")
    args = parser.parse_args(argv)
    try:
        if not VERSION.fullmatch(args.head):
            raise CadenceError(f"malformed head version {args.head!r}")
        history_text = (
            history_from_git(Path.cwd()) if args.from_git else args.history.read_text()
        )
        index_text = fetch_index(CRATE) if args.fetch else args.published.read_text()
        result = decide(
            parse_history(history_text),
            parse_index(index_text),
            args.head,
            args.every,
            args.force,
        )
    except (CadenceError, OSError, subprocess.CalledProcessError) as err:
        print(f"publish_cadence: {err}", file=sys.stderr)
        return 2
    for key, value in result.items():
        print(f"{key}={value}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
