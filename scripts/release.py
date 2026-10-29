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

A change that moves dependency versions and nothing else, which is what a
Dependabot PR is, needs no fragment: Dependabot cannot write one. `cut`
compares Cargo.lock with what it held at the last release and adds a line for
the crates that moved, so their release says so whoever merged them.

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

# Cargo.toml's dependency tables, also under `[target.<cfg>]` or
# `[workspace]`: one listing dependencies by name, where a dependency's value
# is its requirement or an inline table holding one, and `[dependencies.<name>]`,
# a table for one dependency, where only `version` is its requirement.
DEPENDENCY_TABLE = re.compile(r"^\[(?:target\.[^\]]+\.|workspace\.)?(?:dev-|build-)?dependencies\]$")
DEPENDENCY_SUBTABLE = re.compile(r"^\[(?:target\.[^\]]+\.|workspace\.)?(?:dev-|build-)?dependencies\.[^\]]+\]$")
BARE_REQUIREMENT = re.compile(r'^(\s*[A-Za-z0-9_-]+\s*=\s*)"[^"]*"')
INLINE_REQUIREMENT = re.compile(r'(\{.*\bversion\s*=\s*)"[^"]*"')
TABLE_REQUIREMENT = re.compile(r'^(\s*version\s*=\s*)"[^"]*"')
# Past this many dependency updates a release's note counts them instead.
NAMED_UPDATES = 5


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


def locked(lock_text: str) -> set[tuple[str, str, str]]:
    """Every (name, version, source) Cargo.lock pins for croft's dependencies.

    A set of entries, not a map from name to version: cargo locks a crate at
    several versions at once (syn 1 and syn 2) when its dependents disagree.
    croft's own entry is the package being released, not a dependency.
    """
    entries = set()
    for stanza in lock_text.split("[[package]]")[1:]:
        fields = dict(re.findall(r'^(name|version|source) = "([^"]*)"$', stanza, re.M))
        if fields.get("name") != PACKAGE:
            entries.add((fields.get("name", ""), fields.get("version", ""), fields.get("source", "")))
    return entries


def dependency_updates(old_lock: str, new_lock: str) -> list[tuple[str, str]]:
    """(name, version) for each crate the new lockfile pins at a version the
    old one did not, where the old one pinned that crate at all. A crate new
    to the lockfile comes with a change to Cargo.toml, whose notes say why."""
    old = locked(old_lock)
    names = {name for name, _, _ in old}
    return sorted((name, version) for name, version, _ in locked(new_lock) - old if name in names)


def update_note(updates: list[tuple[str, str]]) -> str:
    """The highlight a release carries for its dependency updates. The
    welcome card shows the notes one line each, so a `cargo update` that
    moves dozens of crates is counted rather than named."""
    if len(updates) > NAMED_UPDATES:
        return f"fix: Updated {len(updates)} dependencies.\n"
    named = [f"{name} to {version}" for name, version in updates]
    listed = named[0] if len(named) == 1 else ", ".join(named[:-1]) + " and " + named[-1]
    return f"fix: Updated {listed}.\n"


def without_requirements(toml_text: str) -> str:
    """Cargo.toml with every dependency's version requirement blanked.

    Two manifests equal under this differ only in the versions they ask for,
    and Cargo.lock says which version each got. Everything else about a
    dependency stays: a feature, a `path`, or a `package` rename picks
    different code, so a change to it still reads as a change.
    """
    out, section = [], None
    for line in toml_text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped
        elif section and DEPENDENCY_TABLE.match(section):
            line = INLINE_REQUIREMENT.sub(r'\1""', BARE_REQUIREMENT.sub(r'\1""', line))
        elif section and DEPENDENCY_SUBTABLE.match(section):
            line = TABLE_REQUIREMENT.sub(r'\1""', line)
        out.append(line)
    return "\n".join(out)


def dependency_only(base: str, head: str, shipped: list[str], cwd=None) -> list[tuple[str, str]]:
    """The dependency updates this change is made of, when it is nothing else.

    What a Dependabot PR changes: Cargo.lock pins a crate at a new version,
    and Cargo.toml raises the requirement when the new version is outside
    it. Dependabot cannot write a fragment, and the change needs none: `cut`
    notes every update since the last release. Empty when anything else
    ships, or when no version moved for `cut` to note.
    """
    if not set(shipped) <= {"Cargo.toml", "Cargo.lock"}:
        return []
    if "Cargo.toml" in shipped:
        manifests = [without_requirements(git("show", f"{rev}:Cargo.toml", cwd=cwd)) for rev in (base, head)]
        if manifests[0] != manifests[1]:
            return []
    lock = [git("show", f"{rev}:Cargo.lock", cwd=cwd) for rev in (base, head)]
    return dependency_updates(*lock)


def lock_at_release(cwd=None) -> str:
    """Cargo.lock as the last release left it: at the commit that set HEAD's
    version, which is what `cut` compares the lockfile with."""
    version = package_version(git("show", "HEAD:Cargo.toml", cwd=cwd))
    return git("show", f"{setting_commit(version, cwd=cwd)}:Cargo.lock", cwd=cwd)


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


def cut(root, kind: str = "patch", released_lock: str | None = None) -> str | None:
    """Fold the pending fragments into the next version's notes and bump.

    `released_lock` is Cargo.lock as the last release left it: a dependency
    the lockfile now pins at another version gets a line after the
    fragments' highlights, since a dependency bump merges with no fragment.
    Returns the new version, or None when neither is pending: a merge that
    shipped nothing leaves neither, and an empty release would paint an empty
    card. Every check runs before the first write, so a refused cut leaves
    the tree as it found it.
    """
    root = Path(root)
    fragments = fragment_paths(root)
    toml_path, lock_path = root / "Cargo.toml", root / "Cargo.lock"
    lock_text = lock_path.read_text(encoding="utf-8")
    updates = [] if released_lock is None else dependency_updates(released_lock, lock_text)
    if not fragments and not updates:
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
    if updates:
        texts.append(update_note(updates))

    toml_text = toml_path.read_text(encoding="utf-8")
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

    A change that ships must add at least one fragment that says something,
    unless all it ships is dependency versions, which `cut` notes itself.
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

    updates = dependency_only(base, head, shipped, cwd=cwd)
    if updates:
        listed = ", ".join(f"{name} {version}" for name, version in updates)
        print(
            f"Only dependency versions changed ({listed}). The release after merge "
            "notes them, so no fragment is needed."
        )
    elif not added:
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
            new = cut(Path.cwd(), argv[1] if len(argv) == 2 else "patch", lock_at_release())
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
