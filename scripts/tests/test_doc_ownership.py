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
            # Rust's qualifier order is default, const, async, unsafe, extern.
            # Consuming `const` after the others made these match NOTHING, so
            # the function was invisible rather than undocumented.
            ("const unsafe fn", "/// doc\npub const unsafe fn a() {}\n"),
            ("const extern fn", '/// doc\nconst unsafe extern "C" fn a() {}\n'),
            ("every qualifier", "/// doc\npub default const async unsafe fn a() {}\n"),
        ]:
            state = gate.documented(text)
            self.assertTrue(
                any(state.values()),
                f"{label}: nothing was recognised, so the item is invisible "
                f"to the gate rather than merely undocumented",
            )

    def test_multi_line_attributes_and_block_docs_are_read_correctly(self):
        """Both shapes made a DOCUMENTED item read as undocumented.

        That is a false accusation, not a miss: the gate would report a loss
        that never happened, on a branch that had merely reformatted an
        attribute or used `/** */`. For a gate that is the worse direction,
        because one that cries wolf stops being read.

        The inner form `/*!` and the ordinary `/*` and `/***` comments must
        NOT count, or the gate invents documentation instead.
        """
        documented_shapes = [
            ("multi-line attribute", "/// doc\n#[cfg(all(\n    feature = \"x\",\n    unix\n))]\nconst A: u8 = 1;\n"),
            ("one-line block doc", "/** doc */\nconst A: u8 = 1;\n"),
            ("multi-line block doc", "/**\n * doc\n */\nconst A: u8 = 1;\n"),
            ("block doc then attribute", "/** doc */\n#[cfg(test)]\nconst A: u8 = 1;\n"),
        ]
        for label, text in documented_shapes:
            self.assertEqual(
                gate.documented(text).get("A"), True, f"{label} documents A"
            )

        undocumented_shapes = [
            ("inner block doc documents the MODULE", "/*! module */\nconst A: u8 = 1;\n"),
            ("ordinary block comment", "/* a note */\nconst A: u8 = 1;\n"),
            ("triple-star rule", "/*** rule ***/\nconst A: u8 = 1;\n"),
            ("four slashes is a rule", "//// rule\nconst A: u8 = 1;\n"),
        ]
        for label, text in undocumented_shapes:
            self.assertEqual(
                gate.documented(text).get("A"),
                False,
                f"{label} must not count as documentation",
            )

    def test_a_declaration_inside_a_string_is_not_an_item(self):
        """The regex is line-anchored, so a declaration quoted mid-line is
        not mistaken for one."""
        state = gate.documented('/// doc\nconst A: &str = "const B: u8 = 1;";\n')
        self.assertEqual(state, {"A": True}, "B is inside a string, not an item")

    def test_same_named_items_are_merged_which_under_reports(self):
        """A KNOWN limitation, pinned so it is a decision rather than a bug.

        Two `fn new` in different impl blocks share one key, and "any
        documented" wins, so a capture on one is invisible while the other
        keeps its prose. The alternative is an occurrence-unique key, and
        every candidate (position, index, enclosing type) has to line up
        across two revisions that may have moved the item; a key that
        mis-aligns turns a silent miss into a false accusation, which is
        worse for a gate that blocks merges.

        So the gate under-reports on duplicate names, deliberately. Tracked
        for a proper fix; pinned here so a future change that alters this
        behaviour has to look at this test and say which way it went.
        """
        both_documented = "/// a\nfn new() {}\n\n/// b\nfn new() {}\n"
        one_captured = "/// a\nfn other() {}\nfn new() {}\n\n/// b\nfn new() {}\n"
        self.assertEqual(gate.documented(both_documented), {"new": True})
        self.assertEqual(
            gate.documented(one_captured),
            {"other": True, "new": True},
            "the surviving doc on the second `new` keeps the key True, so the "
            "capture on the first is not visible: under-reporting, not a "
            "false alarm",
        )

    def test_the_error_names_the_revision_the_doc_was_last_seen_at(self):
        """For a file the branch ADDED, `base` is the wrong revision to cite.

        The file does not exist there, so a reader sent to it finds nothing.
        The loss carries the commit it was last documented at.
        """
        import contextlib
        import io
        import os

        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("seed.rs", "fn seed() {}\n")
            repo.branch("work")
            repo.commit("new.rs", DOCUMENTED_CONST)
            documented_at = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=repo.path,
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
            repo.commit("new.rs", CAPTURED_CONST)

            cwd, argv = os.getcwd(), sys.argv
            os.chdir(repo.path)
            sys.argv = ["check_doc_ownership.py", "main", "HEAD"]
            out = io.StringIO()
            try:
                with contextlib.redirect_stdout(out):
                    code = gate.main()
            finally:
                sys.argv = argv
                os.chdir(cwd)

            self.assertEqual(code, 1)
            printed = out.getvalue()
            self.assertIn(
                documented_at[:12],
                printed,
                f"the error must name the commit the doc was last seen at, said: {printed}",
            )

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

    def test_a_doc_captured_and_then_restored_within_a_branch_passes(self):
        """A branch whose HEAD is correct must not be blocked by a state it
        passed through.

        The pairwise pass over branch-added files walks every adjacent pair,
        so a doc captured in one commit and restored in a later one shows up
        as a loss between two intermediate revisions. Reporting that blocks a
        PR that is fine, which is the false accusation that makes a gate
        untrustworthy: the first thing a maintainer does with a gate that
        cries wolf is stop reading it.

        Found within an hour of shipping the pairwise pass, on a real PR
        whose head was correct.
        """
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("seed.rs", "fn seed() {}\n")
            repo.branch("work")
            repo.commit("new.rs", DOCUMENTED_CONST)
            repo.commit("new.rs", CAPTURED_CONST)
            # ... and put it back.
            repo.commit(
                "new.rs",
                "/// Its own doc.\nconst SYNTAX_SEMANTIC: &[&str] = &[\"b\"];\n\n"
                + DOCUMENTED_CONST,
            )
            self.assertEqual(
                repo.exit_code(),
                0,
                "a doc restored before the branch tip is not a loss",
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
