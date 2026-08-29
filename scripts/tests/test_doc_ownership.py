"""Tests for scripts/check_doc_ownership.py: the #314 doc-capture gate.

The gate exists because inserting an item directly above another item's `///`
block silently hands that prose to the newcomer, and nothing in the compiler
or the test suite notices. These tests build real git repositories, because
the two things the gate gets wrong are both about git history rather than
about parsing: what a merge base can see, and what it cannot.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import check_doc_ownership as gate  # noqa: E402


def run(cwd, *args):
    subprocess.run(
        args, cwd=cwd, check=True, capture_output=True, text=True
    )


class Repo:
    """A throwaway git repo with a `main` and a branch off it."""

    def __init__(self, tmp: Path):
        self.path = tmp
        run(tmp, "git", "init", "-q", "-b", "main")
        run(tmp, "git", "config", "user.email", "t@example.com")
        run(tmp, "git", "config", "user.name", "t")

    def commit(self, name: str, text: str, message: str = "c"):
        (self.path / name).write_text(text)
        run(self.path, "git", "add", "-A")
        run(self.path, "git", "commit", "-q", "-m", message)

    def branch(self, name: str):
        run(self.path, "git", "checkout", "-q", "-b", name)

    def exit_code(self, base="main", head="HEAD"):
        """The gate's exit status: 1 when it found a capture, 0 when clean."""
        import os

        cwd = os.getcwd()
        argv = sys.argv
        os.chdir(self.path)
        sys.argv = ["check_doc_ownership.py", base, head]
        try:
            return gate.main()
        finally:
            sys.argv = argv
            os.chdir(cwd)


DOCUMENTED_CONST = '''/// The scopes each role uses.
const SYNTAX_SCOPES: &[&str] = &["a"];
'''

CAPTURED_CONST = '''/// The scopes each role uses.
const SYNTAX_SEMANTIC: &[&str] = &["b"];

const SYNTAX_SCOPES: &[&str] = &["a"];
'''


class ItemKinds(unittest.TestCase):
    """`fn` was the original scope, and it let two captures through in a day."""

    def test_every_item_kind_is_recognised(self):
        for decl, name in [
            ("fn a() {}", "a"),
            ("pub fn a() {}", "a"),
            ("const A: u8 = 1;", "A"),
            ("pub const A: &str = \"x\";", "A"),
            ("static A: u8 = 1;", "A"),
            ("pub static mut A: u8 = 1;", "A"),
            ("struct A;", "A"),
            ("pub enum A { B }", "A"),
            ("union A { b: u8 }", "A"),
            ("pub trait A {}", "A"),
            ("type A = u8;", "A"),
            ("macro_rules! a {}", "a"),
        ]:
            state = gate.documented(f"/// doc\n{decl}\n")
            self.assertEqual(
                state.get(name),
                True,
                f"{decl!r} should be seen as documented item {name!r}",
            )

    def test_declaration_shapes_the_regex_must_not_miss(self):
        """Shapes that look like edge cases and are ordinary Rust.

        Each was probed rather than assumed: the same-line attribute was
        genuinely missed before this test existed, which made the item
        invisible to the gate rather than merely undocumented.
        """
        for label, text in [
            ("attribute on its own line", "/// doc\n#[cfg(test)]\nconst A: u8 = 1;\n"),
            ("attribute on the same line", "/// doc\n#[cfg(test)] const A: u8 = 1;\n"),
            ("generic type alias", "/// doc\npub type A<T> = Vec<T>;\n"),
            ("const fn", "/// doc\npub const fn a() -> u8 { 1 }\n"),
            ("indented impl method", "impl X {\n    /// doc\n    pub fn a(&self) {}\n}\n"),
        ]:
            state = gate.documented(text)
            self.assertTrue(
                any(state.values()),
                f"{label}: nothing was recognised, so the item is invisible "
                f"to the gate rather than merely undocumented",
            )

    def test_a_declaration_inside_a_string_is_not_an_item(self):
        """The regex is line-anchored, so a declaration quoted mid-line is
        not mistaken for one."""
        state = gate.documented('/// doc\nconst A: &str = "const B: u8 = 1;";\n')
        self.assertEqual(state, {"A": True}, "B is inside a string, not an item")

    def test_a_rename_is_not_a_lost_doc(self):
        """A documented item renamed within a branch must not read as a loss.

        The check requires the name to still EXIST at head, so a rename drops
        out rather than reporting the old name as undocumented. Pinned because
        it is the obvious false positive for a name-keyed comparison.
        """
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "/// doc\nfn a() {}\n")
            repo.branch("work")
            repo.commit("a.rs", "/// doc\nfn b() {}\n")
            self.assertEqual(
                repo.exit_code(), 0, "a rename is not a captured doc comment"
            )

    def test_a_const_that_loses_its_doc_is_reported(self):
        """The #395 shape: a const inserted above another const's doc."""
        before = gate.documented(DOCUMENTED_CONST)
        after = gate.documented(CAPTURED_CONST)
        self.assertTrue(before["SYNTAX_SCOPES"])
        self.assertFalse(after["SYNTAX_SCOPES"], "its doc now sits on the newcomer")
        self.assertTrue(after["SYNTAX_SEMANTIC"], "which is how the capture hides")


class GitShapes(unittest.TestCase):
    def test_a_capture_in_an_existing_file_is_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", DOCUMENTED_CONST)
            repo.branch("work")
            repo.commit("a.rs", CAPTURED_CONST)
            self.assertEqual(
                repo.exit_code(), 1, "the gate must fail on a captured doc"
            )

    def test_a_capture_inside_a_file_the_branch_added_is_reported(self):
        """The #391 shape, which the merge-base comparison cannot see.

        The file does not exist at the base, so `documented(before)` is empty
        and no loss is representable. The branch's own commits are the only
        place that history exists.
        """
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("seed.rs", "fn seed() {}\n")
            repo.branch("work")
            repo.commit("new.rs", DOCUMENTED_CONST)
            repo.commit("new.rs", CAPTURED_CONST)
            self.assertEqual(
                repo.exit_code(), 1, "the gate must fail on a captured doc"
            )

    def test_a_branch_that_captures_nothing_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", DOCUMENTED_CONST)
            repo.branch("work")
            repo.commit("a.rs", DOCUMENTED_CONST + "\n/// Another.\nconst B: u8 = 2;\n")
            self.assertEqual(repo.exit_code(), 0, "an honest addition must pass")

    def test_a_declared_removal_is_exempt(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", DOCUMENTED_CONST)
            repo.branch("work")
            repo.commit(
                "a.rs",
                "const SYNTAX_SCOPES: &[&str] = &[\"a\"];\n",
                "drop it\n\ndoc-removal: a.rs::SYNTAX_SCOPES",
            )
            self.assertEqual(
                repo.exit_code(),
                0,
                "a declared removal is the documented escape hatch",
            )


if __name__ == "__main__":
    unittest.main()
