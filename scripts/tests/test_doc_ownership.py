"""Tests for scripts/check_doc_ownership.py: the #314 doc-capture gate.

The gate exists because inserting an item directly above another item's `///`
block silently hands that prose to the newcomer, and nothing in the compiler
or the test suite notices. These tests build real git repositories, because
the two things the gate gets wrong are both about git history rather than
about parsing: what a merge base can see, and what it cannot.
"""

from __future__ import annotations

import contextlib
import io
import os
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

    def test_methods_are_keyed_by_their_enclosing_impl(self):
        """Two `fn new` in different impl blocks are two items (#405).

        Keyed by bare name, "any documented" merged them and a capture on
        one was invisible while the other kept its prose. The enclosing
        `impl` header is the occurrence-unique key that still lines up
        across revisions: an edit above the block moves it, but does not
        change what it is an impl of.
        """
        both_documented = (
            "impl A {\n    /// a\n    fn new() {}\n}\n\n"
            "impl B {\n    /// b\n    fn new() {}\n}\n"
        )
        one_captured = (
            "impl A {\n    /// a\n    fn other() {}\n    fn new() {}\n}\n\n"
            "impl B {\n    /// b\n    fn new() {}\n}\n"
        )
        self.assertEqual(
            gate.documented(both_documented), {"A::new": True, "B::new": True}
        )
        self.assertEqual(
            gate.documented(one_captured),
            {"A::other": True, "A::new": False, "B::new": True},
            "the capture on A::new is visible even though B::new kept its doc",
        )

    def test_trait_impls_for_the_same_type_are_distinct_blocks(self):
        """`impl Display for X` and `impl Debug for X` both define `fmt`;
        the type alone is not a unique key, so the whole header is."""
        text = (
            "impl fmt::Display for X {\n    /// d\n    fn fmt(&self) {}\n}\n"
            "impl fmt::Debug for X {\n    fn fmt(&self) {}\n}\n"
        )
        self.assertEqual(
            gate.documented(text),
            {"fmt::Display for X::fmt": True, "fmt::Debug for X::fmt": False},
        )

    def test_impl_header_shapes(self):
        """Generics, lifetimes, a `where` clause with the brace on its own
        line, and a trait impl: each still yields the same key, so a branch
        that reformats the header does not move the item to a new key."""
        for label, header in [
            ("plain", "impl Foo {"),
            ("generic", "impl<'a, T: Clone> Foo<'a, T> {"),
            ("where clause, brace on the next line", "impl<T> Foo<T>\nwhere\n    T: Clone,\n{"),
            ("unsafe impl", "unsafe impl Foo {"),
        ]:
            state = gate.documented(f"{header}\n    /// doc\n    fn new() {{}}\n}}\n")
            self.assertEqual(state, {"Foo::new": True}, label)
        state = gate.documented(
            "impl<T> Iterator for Foo<T> {\n    /// doc\n    fn next(&mut self) {}\n}\n"
        )
        self.assertEqual(state, {"Iterator for Foo::next": True})

    def test_braces_in_strings_and_chars_do_not_break_block_tracking(self):
        """Format strings are full of braces. Counting them would leave the
        scanner inside a block that has closed, and key the next top-level
        item to an impl it is not in."""
        text = (
            "impl Foo {\n"
            "    /// doc\n"
            '    fn new() { let _ = format!("{{ {x} }}"); let c = \'{\'; }\n'
            "}\n"
            "/// top\n"
            "fn top() {}\n"
        )
        self.assertEqual(gate.documented(text), {"Foo::new": True, "top": True})

    def test_items_outside_any_impl_keep_the_bare_name(self):
        """Top-level items, and items inside `mod` or `fn` bodies, are keyed
        by name as before: the impl header is the only qualifier, so every
        existing key for a free item survives unchanged."""
        text = (
            "/// a\nfn a() {}\n"
            "mod m {\n    /// b\n    pub fn b() {}\n}\n"
            "impl X {\n    /// c\n    fn c() {}\n}\n"
            "/// d\nfn d() {}\n"
        )
        self.assertEqual(
            gate.documented(text), {"a": True, "b": True, "X::c": True, "d": True}
        )

    def test_a_bodyless_impl_does_not_qualify_the_next_block(self):
        """`impl Eq for X {}` opens and closes on one line. Its header must
        not survive onto the next block, or a `mod` after a marker impl has
        every item keyed to that impl, and a capture inside it is filed under
        a key the base revision never had: a missed report, on the exact
        shape (`src/widgets/file_finder.rs`) the repo contains."""
        text = "impl Eq for X {}\nmod util {\n    /// doc\n    pub fn go() {}\n}\n"
        self.assertEqual(gate.documented(text), {"go": True})
        # And the base-vs-head comparison still reports the capture.
        captured = (
            "impl Eq for X {}\nmod util {\n    /// doc\n    pub fn newcomer() {}\n"
            "    pub fn go() {}\n}\n"
        )
        after = gate.documented(captured)
        self.assertEqual(after.get("go"), False, f"go lost its doc: {after}")

    def test_raw_strings_do_not_break_block_tracking(self):
        """`r#"{"#` is not a `"..."`, and a multi-line raw string of JSON is
        braces all the way down. Both left the counter inside a block that
        had closed, on seven real files under src/."""
        single = (
            'impl Foo {\n    /// doc\n    fn m() {\n'
            '        let j = r#"[{"key": "click"}]"#;\n    }\n}\n'
            "/// top\nfn top() {}\n"
        )
        self.assertEqual(gate.documented(single), {"Foo::m": True, "top": True})
        multi = (
            'impl Foo {\n    /// doc\n    fn m() {\n'
            '        let j = r#"{ "tasks": [\n'
            '            { "label": "x" }\n'
            '        ] }"#;\n    }\n}\n'
            "/// top\nfn top() {}\n"
        )
        self.assertEqual(gate.documented(multi), {"Foo::m": True, "top": True})
        plain_multi = (
            'impl Foo {\n    /// doc\n    fn m() {\n'
            '        let s = "{ opens here\n'
            '            and closes here }";\n    }\n}\n'
            "/// top\nfn top() {}\n"
        )
        self.assertEqual(gate.documented(plain_multi), {"Foo::m": True, "top": True})

    def test_a_header_wrapped_by_rustfmt_keeps_its_key(self):
        """`impl<T>` alone on the first line strips to nothing; the key must
        come from the whole header, or a rewrap moves every method to a
        bare key and a capture in that PR goes unreported."""
        text = "impl<T>\n    Trait for X<T>\n{\n    /// doc\n    fn m() {}\n}\n"
        self.assertEqual(gate.documented(text), {"Trait for X::m": True})

    def test_trait_methods_are_keyed_by_their_trait(self):
        """Two traits declaring `id` are the same shape as two impls
        defining `new`."""
        text = (
            "trait A {\n    /// a\n    fn id(&self);\n}\n"
            "trait B: Clone {\n    /// b\n    fn id(&self);\n}\n"
        )
        self.assertEqual(
            gate.documented(text),
            {"A": False, "trait A::id": True, "B": False, "trait B::id": True},
            "the traits are items in their own scope; their methods are in theirs",
        )

    def test_the_block_tracker_is_balanced_at_eof_on_every_real_file(self):
        """The counter is line-based, so the only proof it kept up with a
        file is that it ends where it started: depth zero, no open block.
        Run over every Rust file in the repo, so a new literal shape that
        desyncs it fails here before it mis-keys anything."""
        src = Path(__file__).resolve().parents[2] / "src"
        files = sorted(src.rglob("*.rs"))
        self.assertGreater(len(files), 50, "the control corpus is missing")
        unbalanced = []
        for f in files:
            lines = f.read_text().splitlines()
            kinds = gate.classify(lines)
            t = gate.BlockTracker()
            for line, kind in zip(lines, kinds):
                if kind == gate.CODE:
                    t.feed(line)
            if t.depth != 0 or t.blocks or t.open_string is not None:
                unbalanced.append((str(f.relative_to(src)), t.depth, t.blocks, t.open_string))
        self.assertEqual(unbalanced, [], "the tracker lost count in these files")

    def test_same_named_items_in_the_same_scope_are_still_merged(self):
        """Two definitions under ONE key (a `cfg`-gated pair, say) keep the
        conservative "any documented" reading: the gate under-reports there
        rather than accusing the wrong twin."""
        text = "/// a\nfn new() {}\n\n/// b\nfn new() {}\n"
        one_captured = "/// a\nfn other() {}\nfn new() {}\n\n/// b\nfn new() {}\n"
        self.assertEqual(gate.documented(text), {"new": True})
        self.assertEqual(gate.documented(one_captured), {"other": True, "new": True})

    def test_a_capture_on_one_impl_of_a_shared_method_name_is_reported(self):
        """End to end: the #405 shape blocks the merge."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit(
                "a.rs",
                "impl A {\n    /// a\n    fn new() {}\n}\n"
                "impl B {\n    /// b\n    fn new() {}\n}\n",
            )
            repo.branch("work")
            repo.commit(
                "a.rs",
                "impl A {\n    /// a\n    fn other() {}\n    fn new() {}\n}\n"
                "impl B {\n    /// b\n    fn new() {}\n}\n",
            )
            self.assertEqual(repo.exit_code(), 1, "A::new lost its doc")
            # The declared-removal escape hatch takes the qualified key.
            repo.commit("a.rs", "// nudge\n" + (repo.path / "a.rs").read_text(), "ok\n\ndoc-removal: a.rs::A::new")
            self.assertEqual(repo.exit_code(), 0, "a declared A::new removal is exempt")

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

    def test_a_file_present_at_the_base_is_not_treated_as_added(self):
        """`git cat-file -e` prints nothing on success, so a probe that read
        its stdout took every changed file for a branch-added one: each
        went through the pairwise commit walk (the slow path, minutes on a
        real branch) and a capture in an existing file was reported twice,
        once per loop. The probe must read the exit status."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", DOCUMENTED_CONST)
            repo.branch("work")
            repo.commit("a.rs", CAPTURED_CONST)
            repo.commit("new.rs", "/// n\nfn n() {}\n")
            cwd = os.getcwd()
            os.chdir(repo.path)
            try:
                self.assertTrue(gate.exists_at("main", "a.rs"))
                self.assertFalse(gate.exists_at("main", "new.rs"))
                self.assertEqual(gate.added_files("main", ["a.rs", "new.rs"]), ["new.rs"])
            finally:
                os.chdir(cwd)

    def test_a_capture_in_an_existing_file_is_reported_exactly_once(self):
        """Two commits on the branch, so the pairwise walk has a pair to
        compare: with every file mistaken for branch-added, the capture
        was reported by that walk AND by the base comparison."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", DOCUMENTED_CONST)
            repo.branch("work")
            repo.commit("a.rs", DOCUMENTED_CONST + "\n/// Extra.\nconst EXTRA: u8 = 1;\n")
            repo.commit("a.rs", CAPTURED_CONST + "\n/// Extra.\nconst EXTRA: u8 = 1;\n")
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
            self.assertEqual(
                out.getvalue().count("::error"),
                1,
                f"one loss, one annotation: {out.getvalue()}",
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


class HeadOnlyOrphanTests(unittest.TestCase):
    """The head-only pass: captures the DIFF cannot see (#427, #436).

    The diff check reports an item that HAD a doc and HAS none. Two real
    captures are invisible to it: one whose victim is new on the branch (so
    it had no doc at the merge base to lose, #427), and one whose capturing
    item is of a kind `ITEM` does not model, such as a `use` (#436). Both
    leave the same fingerprint at HEAD — a doc block with no item under it —
    which needs no base revision to see.
    """

    def test_a_use_between_a_doc_and_its_fn_is_caught(self):
        """#436, reduced. This exact shape shipped and the gate passed."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "fn existing() {}\n")
            repo.branch("feat")
            # The victim is added by the BRANCH, so the diff check has
            # nothing at the base to compare it against.
            repo.commit(
                "a.rs",
                "fn existing() {}\n\n/// Documents beta.\nuse std::fmt;\n\nfn beta() {}\n",
            )
            self.assertEqual(repo.exit_code(), 1)

    def test_a_capture_whose_victim_is_new_on_the_branch_is_caught(self):
        """#427: both items added by the branch, so the merge-base
        comparison sees no loss however many commits it spans."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "fn existing() {}\n")
            repo.branch("feat")
            repo.commit("a.rs", "fn existing() {}\n\n/// Documents beta.\nfn beta() {}\n")
            # A later commit inserts a documented item above it, taking the
            # first doc and stranding its own.
            # Two `///` lines in a row are ONE block, which is legal; a
            # real insertion leaves a blank line between the stranded doc
            # and the newcomer's own, which is what rustfmt produces.
            repo.commit(
                "a.rs",
                "fn existing() {}\n\n/// Documents beta.\n\n"
                "/// Documents gamma.\nfn gamma() {}\n\nfn beta() {}\n",
            )
            self.assertEqual(repo.exit_code(), 1)

    def test_ordinary_docs_are_not_reported(self):
        """The control. A gate that cries wolf stops being read, so the
        shapes a real file is full of must stay silent: attributes and
        blank lines between a doc and its item, a `//` note under a doc,
        and a doc above an item kind the regex does not model."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "fn existing() {}\n")
            repo.branch("feat")
            repo.commit(
                "a.rs",
                "fn existing() {}\n\n"
                # An attribute between doc and item.
                "/// Documented, with an attribute under the doc.\n"
                "#[derive(Clone, Debug)]\n"
                "struct A;\n\n"
                # A plain comment under a doc: does NOT break attachment.
                "/// Documented, with an implementation note.\n"
                "// not a doc comment\n"
                "fn b() {}\n\n"
                # A doc above an item kind `ITEM` does not model.
                "/// Documented, above an impl.\n"
                "impl A {}\n\n"
                # A doc above a module.
                "/// Documented, above a mod.\n"
                "mod inner {}\n",
            )
            self.assertEqual(repo.exit_code(), 0)

    def test_a_doc_block_at_end_of_file_is_reported(self):
        """The third shape the pass reports, and the one with no test until
        now: a doc block with no item after it at all."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "fn existing() {}\n")
            repo.branch("feat")
            repo.commit("a.rs", "fn existing() {}\n\n/// Documents nothing at all.\n")
            self.assertEqual(repo.exit_code(), 1)

    def test_a_capture_by_a_documented_newcomer_is_caught(self):
        """Was the residue #427 kept, and is now caught by the diff pass
        (#455). When the inserted item carries its OWN doc nothing is
        stranded, so the head-only pass still cannot see it; what gives it
        away is that `/// Documents alpha.` sat above `fn alpha()` earlier on
        the branch and sits above a newly inserted item now, leaving `alpha`
        bare. Inverted rather than deleted, as the earlier version asked."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "fn existing() {}\n")
            repo.branch("feat")
            repo.commit("a.rs", "fn existing() {}\n\n/// Documents alpha.\nfn alpha() {}\n")
            repo.commit(
                "a.rs",
                "fn existing() {}\n\n/// Documents alpha.\n"
                "/// ...but now sits above the newcomer.\n"
                "struct Inserted;\n\nfn alpha() {}\n",
            )
            self.assertEqual(repo.exit_code(), 1)

    def test_a_documented_re_export_is_not_reported(self):
        """`pub use` CAN legitimately carry a doc — rustdoc renders it — so
        flagging one would fail a correct PR. A private `use` cannot appear
        in the docs at all, which is why it is the only form treated as
        unable to hold prose."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "fn existing() {}\n")
            repo.branch("feat")
            repo.commit(
                "a.rs",
                "fn existing() {}\n\n"
                "/// Re-exported for convenience.\n"
                "pub use crate::x::Y;\n\n"
                "/// Also re-exported.\n"
                "pub(crate) use crate::x::Z;\n",
            )
            self.assertEqual(repo.exit_code(), 0)

    def test_a_use_that_documents_nothing_untouched_is_not_reported(self):
        """Only CHANGED files are swept, so a pre-existing orphan elsewhere
        does not fail an unrelated PR."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "/// Stranded.\nuse std::fmt;\n\nfn a() {}\n")
            repo.commit("b.rs", "fn b() {}\n")
            repo.branch("feat")
            repo.commit("b.rs", "fn b() {}\n\nfn c() {}\n")
            self.assertEqual(repo.exit_code(), 0)



class ReassignedDocTests(unittest.TestCase):
    """The third pass (#455): a doc line that changed the item it sits above.

    The head-only pass sees a capture only through the prose it STRANDS, and
    a capture whose newcomer brings its own doc strands nothing: rustfmt
    leaves the two `///` lines contiguous, they read as one ordinary block,
    and the item below them is the thief. What separates that from a genuine
    two-line doc block is not in the snapshot at all - it is in the diff,
    where the same line used to sit above a different item.
    """

    def test_a_contiguous_capture_of_a_variants_doc_is_caught(self):
        """#455, reduced from the instance that shipped in #454: the victim
        is an enum variant, which `ITEM` does not model, and the stolen doc
        is contiguous with the thief's, which the head-only pass reads as
        one block. Both existing passes are blind to it."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit(
                "a.rs",
                "enum E {\n"
                "    /// Doc for A, an existing item.\n"
                "    A,\n"
                "}\n",
            )
            repo.branch("feat")
            repo.commit(
                "a.rs",
                "enum E {\n"
                "    /// Doc for A, an existing item.\n"
                "    /// Doc for B, inserted above it.\n"
                "    B,\n"
                "    A,\n"
                "}\n",
            )
            self.assertEqual(repo.exit_code(), 1)

    def test_the_error_names_the_line_and_both_items(self):
        """A gate that says only "something moved" sends the reader hunting.
        The message has to carry the prose, what it used to describe, and
        what it describes now."""
        before = "enum E {\n    /// Doc for A.\n    A,\n}\n"
        after = "enum E {\n    /// Doc for A.\n    /// Doc for B.\n    B,\n    A,\n}\n"
        found = gate.reassigned_docs(before, after)
        self.assertEqual(len(found), 1, found)
        _line, doc, old, new = found[0]
        self.assertEqual(doc, "/// Doc for A.")
        self.assertEqual(old, "A,")
        self.assertEqual(new, "B,")

    def test_moving_a_doc_back_onto_its_own_item_is_not_a_capture(self):
        """The corrective PR. Repairing a misattribution moves the prose the
        other way, and the item it lands on is one that already existed -
        which is exactly what an INSERTED thief is not."""
        before = "enum E {\n    /// Doc for A.\n    B,\n    A,\n}\n"
        after = "enum E {\n    B,\n    /// Doc for A.\n    A,\n}\n"
        self.assertEqual(gate.reassigned_docs(before, after), [])

    def test_reordering_documented_items_is_not_a_capture(self):
        """Each doc travels with its own item, so no line changes owner."""
        before = "enum E {\n    /// Doc A.\n    A,\n    /// Doc B.\n    B,\n}\n"
        after = "enum E {\n    /// Doc B.\n    B,\n    /// Doc A.\n    A,\n}\n"
        self.assertEqual(gate.reassigned_docs(before, after), [])

    def test_a_renamed_item_is_not_a_capture(self):
        """The old subject line is gone at head, so nothing was stranded."""
        before = "/// Doc for alpha.\nfn alpha() {}\n"
        after = "/// Doc for alpha.\nfn renamed() {}\n"
        self.assertEqual(gate.reassigned_docs(before, after), [])

    def test_a_victim_that_keeps_a_doc_of_its_own_is_not_reported(self):
        """The swap that fixes a capture leaves both items documented. Only
        prose that leaves an item BARE is a loss worth failing a PR over."""
        # `Inserted` is new, and A keeps a doc line, so nothing is bare.
        before = "enum E {\n    /// Doc A.\n    A,\n    /// Doc B.\n    B,\n}\n"
        after = (
            "enum E {\n    /// Doc A.\n    Inserted,\n"
            "    /// Doc A, still here.\n    A,\n    /// Doc B.\n    B,\n}\n"
        )
        self.assertEqual(gate.reassigned_docs(before, after), [])
