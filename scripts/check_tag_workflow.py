#!/usr/bin/env python3
"""Whether tag.yml runs only pinned code against a pinned API.

tag.yml runs on every merge to main with a token that can create tags and
dispatch release.yml, which publishes signed binaries. Two things it executes
can change after review without a diff in this repo:

  * an action referenced by tag or branch, which its owner can move to new
    code. Only a full 40-hex commit SHA names fixed code.
  * a REST call with no `X-GitHub-Api-Version`, which GitHub answers with its
    oldest supported version and switches to the next one when that retires,
    changing response shapes under the job. `gh` subcommands other than `gh
    api` (such as `gh workflow run`) cannot take the header, so only `gh api`
    is allowed.

Exit 0 when every `uses:` is SHA-pinned and every `gh` call in a `run:` script
is `gh api` with a dated version header; 1 naming each violation otherwise.

String-level, like check_binstall_contract.py: a YAML parser is a dependency
this repo's gates do not have, and the checks are about lines a reader sees.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "tag.yml"

USES = re.compile(r"^\s*(?:-\s+)?uses:\s*['\"]?([^'\"\s#]+)")
FULL_SHA = re.compile(r"[0-9a-f]{40}")
RUN = re.compile(r"^(\s*)(?:-\s+)?run:\s*(.*)$")
# `gh` as a command word: not part of GH_TOKEN, a path, or a longer name.
GH = re.compile(r"(?<![\w$./-])gh\s+(\S+)")
VERSION = re.compile(r"X-GitHub-Api-Version:\s*\d{4}-\d{2}-\d{2}\b")


def unpinned_actions(lines):
    for no, line in enumerate(lines, 1):
        m = USES.match(line)
        if m and not FULL_SHA.fullmatch(m.group(1).rpartition("@")[2]):
            yield f"line {no}: {m.group(1)} is not pinned to a full commit SHA"


def script_lines(lines):
    """(line number, text) for every line inside a `run:` value."""
    i = 0
    while i < len(lines):
        m = RUN.match(lines[i])
        i += 1
        if not m:
            continue
        indent, value = len(m.group(1)), m.group(2).strip()
        if value and value[0] not in "|>":
            yield i, value
            continue
        while i < len(lines) and (
            not lines[i].strip() or len(lines[i]) - len(lines[i].lstrip()) > indent
        ):
            yield i + 1, lines[i].strip()
            i += 1


def commands(lines):
    """Logical script lines: comments dropped, `\\` continuations joined."""
    start, buf = None, []
    for no, text in script_lines(lines):
        if not buf and (not text or text.startswith("#")):
            continue
        start = start or no
        if text.endswith("\\"):
            buf.append(text[:-1])
            continue
        buf.append(text)
        yield start, " ".join(buf)
        start, buf = None, []
    if buf:
        yield start, " ".join(buf)


def unversioned_calls(lines):
    for no, cmd in commands(lines):
        calls = list(GH.finditer(cmd))
        for k, m in enumerate(calls):
            end = calls[k + 1].start() if k + 1 < len(calls) else len(cmd)
            call = cmd[m.start():end].strip()
            if m.group(1) != "api":
                yield f"line {no}: `gh {m.group(1)}` cannot pin an API version; use `gh api`: {call}"
            elif not VERSION.search(call):
                yield f"line {no}: gh api call without a dated X-GitHub-Api-Version header: {call}"


def main():
    if not WORKFLOW.is_file():
        print(f"{WORKFLOW.relative_to(ROOT)} is missing", file=sys.stderr)
        return 1
    lines = WORKFLOW.read_text().splitlines()
    problems = [*unpinned_actions(lines), *unversioned_calls(lines)]
    for p in problems:
        print(f"tag.yml {p}", file=sys.stderr)
    if problems:
        return 1
    print("tag.yml: every action is pinned and every API call names its version")
    return 0


if __name__ == "__main__":
    sys.exit(main())
