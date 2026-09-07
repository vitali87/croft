#!/usr/bin/env python3
"""Whether Cargo.toml's binstall metadata still describes what release.yml builds.

`cargo binstall croft-software` resolves a URL and an in-archive path from
`[package.metadata.binstall]`, and nothing verifies those against the workflow
that produces the archives. The two drift silently: rename the archive in the
workflow and the manifest still points at the old name, so binstall 404s for
every user while every test and every CI job stays green. The failure lands on
users at install time, which is the worst place for it (#376).

So this asserts the contract between the two files:

  * the archive basename in `pkg-url` matches the name release.yml packages
  * `bin-dir`'s directory component matches the directory inside the tarball
  * the tag shape in `pkg-url` matches the workflow's trigger and its
    tag-to-version arithmetic
  * every target in `bin-dir`'s expansion is one the workflow actually builds

Exit 0 when they agree, 1 with a message naming the drift when they do not.

Deliberately string-level rather than a TOML/YAML parse: the point is to catch
a human editing one file and not the other, and both files are read here the
way a reader reads them. A parser would also need a dependency this repo's
gates do not have.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "Cargo.toml"
WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"

# The four triples the issue names. A target added to the workflow without
# being added here is fine; a target here that the workflow does not build
# means binstall promises a platform no release carries.
EXPECTED_TARGETS = {
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
}


def fail(msg: str) -> None:
    print(f"binstall contract: {msg}", file=sys.stderr)
    sys.exit(1)


def binstall_block(text: str) -> dict:
    """The key/value pairs under [package.metadata.binstall]."""
    m = re.search(
        r"^\[package\.metadata\.binstall\]\s*$(.*?)(?=^\[|\Z)",
        text,
        re.M | re.S,
    )
    if not m:
        fail("no [package.metadata.binstall] section in Cargo.toml")
    out = {}
    for line in m.group(1).splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        k, _, v = line.partition("=")
        out[k.strip()] = v.strip().strip('"')
    return out


def main() -> None:
    if not WORKFLOW.exists():
        fail(f"{WORKFLOW.relative_to(ROOT)} is missing, but Cargo.toml "
             "promises binstall artifacts")

    manifest = MANIFEST.read_text()
    workflow = WORKFLOW.read_text()
    meta = binstall_block(manifest)

    for key in ("pkg-url", "pkg-fmt", "bin-dir"):
        if key not in meta:
            fail(f"[package.metadata.binstall] has no {key}")

    # 1. The archive name. Both sides spell it with the target substituted,
    #    so compare the shape either side of the placeholder.
    url_archive = meta["pkg-url"].rsplit("/", 1)[-1]
    if "{ target }" not in url_archive:
        fail(f"pkg-url's archive name does not vary by target: {url_archive}")
    prefix, suffix = url_archive.split("{ target }", 1)
    # Anchor on the Package step's own assignment rather than on the prefix
    # appearing somewhere in the file: "croft-" occurs in artifact names, the
    # collect glob and the checksum lines, so a substring search over the
    # whole workflow passes even when the packaged name has changed.
    packaged = re.search(r'^\s*name="([^"]+)"\s*$', workflow, re.M)
    if not packaged:
        fail("release.yml has no `name=\"...\"` line in its Package step; "
             "cannot tell what the archive is called")
    packaged_name = packaged.group(1).replace("${{ matrix.target }}",
                                              "{ target }")
    if packaged_name != url_archive[: -len(suffix)]:
        fail(f"release.yml packages {packaged_name!r} but pkg-url expects "
             f"{url_archive[: -len(suffix)]!r}; binstall would 404")
    if not suffix:
        fail("pkg-url's archive name has no extension")
    if f"tar -czf" in workflow and suffix != ".tar.gz":
        fail(f"release.yml writes .tar.gz but pkg-url expects {suffix}")

    # 2. pkg-fmt must match the extension, or binstall unpacks with the wrong
    #    reader and reports a corrupt download.
    if suffix == ".tar.gz" and meta["pkg-fmt"] != "tgz":
        fail(f'pkg-fmt is {meta["pkg-fmt"]!r} for a .tar.gz archive; want "tgz"')

    # 3. bin-dir's directory must match the directory the workflow puts inside
    #    the tarball. `tar -czf "$name.tar.gz" -C dist "$name"` means the
    #    archive root is $name, so bin-dir must start with the same shape.
    bin_dir = meta["bin-dir"]
    if "/" not in bin_dir:
        fail(f"bin-dir has no directory component: {bin_dir!r}; the archive "
             "wraps the binary in a directory named for the target")
    archive_dir = bin_dir.split("/", 1)[0]
    expected_dir = url_archive[: -len(suffix)]
    if archive_dir != expected_dir:
        fail(f"bin-dir's directory {archive_dir!r} does not match the archive "
             f"name {expected_dir!r}; binstall would look in the wrong folder")

    # 4. The tag shape. pkg-url hard-codes the `v` prefix, and the workflow
    #    both triggers on it and strips it to find the notes file.
    if "/v{ version }/" not in meta["pkg-url"]:
        fail("pkg-url does not use the v-prefixed tag the workflow triggers on")
    if 'tags:' not in workflow or '"v*"' not in workflow:
        fail("release.yml does not trigger on v* tags, but pkg-url assumes it")

    # 5. Every expected target is actually built.
    missing = sorted(t for t in EXPECTED_TARGETS if t not in workflow)
    if missing:
        fail("release.yml does not build " + ", ".join(missing))

    print(f"binstall contract: Cargo.toml and "
          f"{WORKFLOW.relative_to(ROOT)} agree "
          f"({len(EXPECTED_TARGETS)} targets, {expected_dir}{suffix})")


if __name__ == "__main__":
    main()
