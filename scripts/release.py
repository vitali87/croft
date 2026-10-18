#!/usr/bin/env python3
"""Release notes written in pull requests, versions assigned after merge.

A pull request that changes what ships writes its highlights to a fragment,
`src/release_notes/unreleased/<name>.md`, and never touches `version` in
Cargo.toml. Each PR adds a file no other PR names, so two open PRs never edit
the same line. When every PR bumped the version and wrote
`src/release_notes/<version>.md` itself, each merge put every other open PR in
conflict on that line, and a PR whose number main had reached had to take a
new one and rename its notes.

After a merge, `cut` runs once on main (.github/workflows/version-bump.yml):
it folds every pending fragment into `src/release_notes/<next>.md`, removes
the fragments, and bumps Cargo.toml and croft's entry in Cargo.lock. The next
version follows next_version.py's odometer, so MINOR and PATCH stay below
1000.

Usage:
  release.py check <base> <head>         the PR gate: a shipped change carries
                                         a fragment and leaves the version alone
  release.py cut [patch|minor|major]     fold the fragments and bump; prints the
                                         new version, or nothing when no
                                         fragment is pending
  release.py notes <version>             the GitHub release body for <version>:
                                         reads the tags already released, one
                                         per line on stdin, and prints the notes
                                         of every version since the newest one
  release.py commit <version>            the commit on HEAD's first-parent chain
                                         that set HEAD's version, <version>:
                                         what publish-crate.yml publishes and
                                         tags
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from next_version import VERSION, bump, parse  # noqa: E402
from ships_nothing import ships_nothing  # noqa: E402

NOTES_DIR = "src/release_notes"
UNRELEASED_DIR = f"{NOTES_DIR}/unreleased"
# The README that explains the directory, and keeps it in git while no
# fragment is pending. It is never a note.
README = "README.md"
PACKAGE = "croft-software"

# What ships in the binary: sources, baked-in assets, the build script and the
# dependency set. src/app/tests/ (src/app/tests.rs before it was split) and
# tests/ are compiled out of release builds, and a pending fragment is notes,
# not code: a typo fix in one changes no binary and owes no note of its own.
SHIPPED = re.compile(r"^(src/|assets/|build\.rs$|Cargo\.toml$|Cargo\.lock$)")
NOT_SHIPPED = re.compile(r"^(src/app/tests\.rs$|src/app/tests/|tests/|src/release_notes/unreleased/)")
RELEASED_NOTES = re.compile(r"^src/release_notes/[0-9]+\.[0-9]+\.[0-9]+\.md$")
# A dot-file (an editor's `.#name.md` lock file) is never a note, here, in
# `fragment_paths` or in the build's `select::fragments`.
FRAGMENT = re.compile(r"^src/release_notes/unreleased/[^/.][^/]*\.md$")


class ReleaseError(Exception):
    """A cut that cannot go ahead. Nothing has been written when it is raised."""


def has_highlight(text: str) -> bool:
    """The filter build.rs and `release_notes::parse` apply: a line that is
    neither blank nor a `#` heading. A file holding only a heading is
    non-blank and still paints an empty card."""
    return any(line.strip() and not line.strip().startswith("#") for line in text.splitlines())


def git(*args, cwd=None) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


def package_version(toml_text: str) -> str:
    """`version` in Cargo.toml's [package] table, and only there: a
    dependency table can carry a `version` key of its own."""
    section = None
    for line in toml_text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped
        elif section == "[package]" and re.match(r'^version\s*=\s*"[^"]*"', stripped):
            return stripped.split('"')[1]
    raise ReleaseError('Cargo.toml has no version in its [package] table')


def set_package_version(toml_text: str, new: str) -> str:
    out, section, hits = [], None, 0
    for line in toml_text.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped
        elif section == "[package]" and re.match(r'^version\s*=\s*"[^"]*"', stripped):
            line = re.sub(r'"[^"]*"', f'"{new}"', line, count=1)
            hits += 1
        out.append(line)
    if hits != 1:
        raise ReleaseError(f"Cargo.toml's [package] table has {hits} version lines, expected 1")
    return "".join(out)


def set_lock_version(lock_text: str, new: str) -> str:
    """Rewrite the version line of croft's own `[[package]]` stanza.

    Split on stanzas rather than matching `name` then `version` on adjacent
    lines, so a field cargo puts between them does not turn the rewrite into a
    silent no-op, and no dependency that shares the version is touched. The
    lockfile's own `version = 4` line sits before the first stanza and is left
    alone.
    """
    version_line = re.compile(r'^version = "[^"]*"$', re.M)
    stanzas = lock_text.split("[[package]]")
    ours = [
        i
        for i, stanza in enumerate(stanzas[1:], start=1)
        if re.search(rf'^name = "{re.escape(PACKAGE)}"$', stanza, re.M)
    ]
    if len(ours) != 1:
        raise ReleaseError(f"Cargo.lock has {len(ours)} {PACKAGE} entries, expected 1")
    found = version_line.findall(stanzas[ours[0]])
    if len(found) != 1:
        raise ReleaseError(f"Cargo.lock's {PACKAGE} entry has {len(found)} version lines, expected 1")
    stanzas[ours[0]] = version_line.sub(f'version = "{new}"', stanzas[ours[0]])
    return "[[package]]".join(stanzas)


def fragment_paths(root: Path) -> list[Path]:
    """Pending fragments, in file-name order so a cut is reproducible.

    pathlib's glob matches dot-files, so they are left out by name: an
    editor's `.#name.md` lock file is not a note.
    """
    unreleased = Path(root) / UNRELEASED_DIR
    if not unreleased.is_dir():
        return []
    return sorted(
        p
        for p in unreleased.glob("*.md")
        if p.name != README and not p.name.startswith(".") and p.is_file()
    )


def cut(root, kind: str = "patch") -> str | None:
    """Fold the pending fragments into the next version's notes and bump.

    Returns the new version, or None when no fragment is pending: a merge that
    shipped nothing leaves none, and an empty release would paint an empty
    card. Every check runs before the first write, so a refused cut leaves
    the tree as it found it.
    """
    root = Path(root)
    fragments = fragment_paths(root)
    if not fragments:
        return None
    texts = []
    for path in fragments:
        text = path.read_text(encoding="utf-8")
        if not has_highlight(text):
            raise ReleaseError(
                f"{path.relative_to(root)} carries no highlights (blank, or nothing but "
                "headings). Write one per line, each prefixed 'feature:' or 'fix:'."
            )
        texts.append(text)

    toml_path, lock_path = root / "Cargo.toml", root / "Cargo.lock"
    toml_text = toml_path.read_text(encoding="utf-8")
    lock_text = lock_path.read_text(encoding="utf-8")
    try:
        new = bump(package_version(toml_text), kind)
    except ValueError as err:
        raise ReleaseError(str(err)) from err
    notes_path = root / NOTES_DIR / f"{new}.md"
    if notes_path.exists():
        raise ReleaseError(
            f"{notes_path.relative_to(root)} already exists. It describes a release; "
            "a cut never overwrites one."
        )
    new_toml = set_package_version(toml_text, new)
    new_lock = set_lock_version(lock_text, new)

    notes_path.write_text("".join(t if t.endswith("\n") else t + "\n" for t in texts), encoding="utf-8")
    toml_path.write_text(new_toml, encoding="utf-8")
    lock_path.write_text(new_lock, encoding="utf-8")
    for path in fragments:
        path.unlink()
    return new


def release_notes(root, version: str, released: list[str]) -> str:
    """The GitHub release body for `version`.

    A GitHub release goes out every PUBLISH_EVERY versions
    (.github/workflows/publish-crate.yml), so one release has to carry the
    notes of every version since the previous one, not only its own. Those are
    the versions above the newest entry in `released` (tags such as `v0.2.28`;
    anything that is not one is ignored) up to `version`, newest first, each
    under its own heading. A version in that range with no notes file shipped
    nothing and is left out. With nothing released before it, or nothing in
    between, the body is `version`'s own notes, as it was when every version
    was released.
    """
    root = Path(root)
    own = root / NOTES_DIR / f"{version}.md"
    if not VERSION.fullmatch(version) or not own.is_file():
        raise ReleaseError(f"no {NOTES_DIR}/{version}.md: the tag does not match any version")
    top = parse(version)
    before = [
        parse(tag[1:])
        for tag in (line.strip() for line in released)
        if tag.startswith("v") and VERSION.fullmatch(tag[1:]) and parse(tag[1:]) < top
    ]
    floor = max(before, default=top)
    versions = sorted(
        (
            path.stem
            for path in (root / NOTES_DIR).glob("*.md")
            if VERSION.fullmatch(path.stem) and floor < parse(path.stem) <= top
        ),
        key=parse,
        reverse=True,
    )
    if len(versions) <= 1:
        return own.read_text(encoding="utf-8")
    sections = []
    for v in versions:
        text = (root / NOTES_DIR / f"{v}.md").read_text(encoding="utf-8")
        if not text.endswith("\n"):
            text += "\n"
        sections.append(f"## {v}\n\n{text}")
    return "\n".join(sections)


def setting_commit(version: str, cwd=None) -> str:
    """The commit that set Cargo.toml's version to `version`, which HEAD carries.

    That is the newest commit on HEAD's first-parent chain whose first parent
    carries another version: a later merge still carries the version but its
    code is not that version's yet. publish-crate.yml publishes and tags this
    commit, and tag.yml tags a push only when its commit and first parent
    differ in version, the same comparison, so the two never name different
    commits. Versions are compared rather than diff lines matched: `git log -G`
    also picks a later commit that only rewrites the version line, or a
    dependency's `version =` line that happens to hold the same number.
    """
    if not VERSION.fullmatch(version):
        raise ReleaseError(f"refusing malformed version {version!r}")
    chain = git("rev-list", "--first-parent", "HEAD", cwd=cwd).split()

    def version_at(commit: str) -> str:
        return package_version(git("show", f"{commit}:Cargo.toml", cwd=cwd))

    head = version_at(chain[0])
    if head != version:
        raise ReleaseError(f"HEAD carries version {head}, not {version}")
    for commit, parent in zip(chain, chain[1:]):
        if version_at(parent) != version:
            return commit
    # The walk reached a commit with no parent. In a shallow clone that is
    # only where the history was cut off, not where the version was set.
    if git("rev-parse", "--is-shallow-repository", cwd=cwd).strip() == "true":
        raise ReleaseError(
            f"the clone is shallow and every commit in it carries {version}; "
            "fetch the full history to find the commit that set it"
        )
    return chain[-1]


def check(base: str, head: str, cwd=None) -> list[str]:
    """The PR gate. Returns its errors, each in GitHub's `::error` form.

    A change that ships must add at least one fragment that says something.
    Whether or not anything ships, the version and every released version's
    notes stay as they are (`cut` writes both after merge), and a note
    pending on main stays, saying something.
    """
    changed = git("diff", "--no-renames", "--name-status", base, head, cwd=cwd).splitlines()
    status = {}
    for row in changed:
        code, path = row.split("\t", 1)
        status[path] = code[0]

    errors = []
    old = package_version(git("show", f"{base}:Cargo.toml", cwd=cwd))
    new = package_version(git("show", f"{head}:Cargo.toml", cwd=cwd))
    if old != new:
        errors.append(
            f"::error file=Cargo.toml::This PR changes the version from {old} to {new}. "
            "Leave it at the base's version: the version-bump workflow assigns the next "
            "one after merge, so two open PRs never claim the same number."
        )
    for path in sorted(p for p in status if RELEASED_NOTES.match(p)):
        errors.append(
            f"::error file={path}::This PR changes {path}. A version's notes are written "
            "after merge, from the pending fragments, and a released version's notes "
            "describe a binary already out there. Write this change's highlights to "
            f"{UNRELEASED_DIR}/<name>.md instead."
        )

    # Whether or not anything ships: a note pending on main is the next
    # release's, so a PR may edit it but not delete it, and an edit must
    # leave it saying something, or the cut after merge stops on it. A note
    # moved to another name, its text unchanged, is a rename, not a loss.
    def is_fragment(p: str) -> bool:
        return bool(FRAGMENT.match(p)) and Path(p).name != README

    added = [p for p, code in sorted(status.items()) if code == "A" and is_fragment(p)]
    added_texts = {git("show", f"{head}:{p}", cwd=cwd) for p in added}
    for path, code in sorted(status.items()):
        if not is_fragment(path):
            continue
        if code == "D" and git("show", f"{base}:{path}", cwd=cwd) not in added_texts:
            errors.append(
                f"::error file={path}::This PR deletes {path}, a note waiting for the next "
                "release, which would then leave it out. Keep it; to change what it says, "
                "edit it."
            )
        elif code == "M" and not has_highlight(git("show", f"{head}:{path}", cwd=cwd)):
            errors.append(
                f"::error file={path}::{path} carries no highlights (blank, or nothing but "
                "headings). Write one per line, each prefixed 'feature:' or 'fix:'."
            )

    shipped = []
    for path in sorted(status):
        if not SHIPPED.match(path) or NOT_SHIPPED.match(path):
            continue
        if ships_nothing(base, head, path, cwd=cwd):
            print(f"{path}: ships nothing.")
            continue
        shipped.append(path)
    if not shipped:
        print("No shipped files changed; no release notes required.")
        return errors
    print("Shipped files changed:")
    for path in shipped:
        print(f"  {path}")

    if not added:
        errors.append(
            f"::error::This PR changes shipped files but adds no release notes. Add "
            f"{UNRELEASED_DIR}/<name>.md, named for this change (for example its issue "
            "number and a word or two), with its highlights one per line, each prefixed "
            "'feature:' or 'fix:'. It is folded into the next version's notes after merge."
        )
    for path in added:
        if not has_highlight(git("show", f"{head}:{path}", cwd=cwd)):
            errors.append(
                f"::error file={path}::{path} carries no highlights (blank, or nothing but "
                "headings). Write one per line, each prefixed 'feature:' or 'fix:'."
            )
    return errors


def main(argv: list[str]) -> int:
    try:
        return run_command(argv)
    except subprocess.CalledProcessError as err:
        # A ref the clone lacks (a shallow checkout, a mistyped base): one
        # line CI shows as an error, not a traceback. On stderr, so a step
        # that captures stdout (`cut`'s version, `commit`'s hash) still shows it.
        command = " ".join(err.cmd)
        why = (err.stderr or "").strip().splitlines()
        print(f"::error::{command} failed: {why[0] if why else f'exit {err.returncode}'}", file=sys.stderr)
        return 1


def run_command(argv: list[str]) -> int:
    if len(argv) == 3 and argv[0] == "check":
        errors = check(argv[1], argv[2])
        for error in errors:
            print(error)
        return 1 if errors else 0
    if 1 <= len(argv) <= 2 and argv[0] == "cut":
        try:
            new = cut(Path.cwd(), argv[1] if len(argv) == 2 else "patch")
        except ReleaseError as err:
            print(f"::error::{err}", file=sys.stderr)
            return 1
        if new:
            print(new)
        return 0
    if len(argv) == 2 and argv[0] == "notes":
        try:
            body = release_notes(Path.cwd(), argv[1], sys.stdin.read().splitlines())
        except ReleaseError as err:
            print(f"::error::{err}", file=sys.stderr)
            return 1
        sys.stdout.write(body)
        return 0
    if len(argv) == 2 and argv[0] == "commit":
        try:
            commit = setting_commit(argv[1])
        except ReleaseError as err:
            print(f"::error::{err}", file=sys.stderr)
            return 1
        print(commit)
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
