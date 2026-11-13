"""Tests for scripts/check_pure_move.py: the proof that a split only moves code.

Each case builds a real git repository, because what the check certifies is a
range of commits: a review that trusts its "pure move" verdict reads none of
the moved lines, so every way an edit could hide inside a move is pinned here.
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

import check_pure_move as pm  # noqa: E402


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True)


def run_main(cwd, *args):
    out, here = io.StringIO(), os.getcwd()
    os.chdir(cwd)
    try:
        with contextlib.redirect_stdout(out):
            return pm.main(list(args)), out.getvalue()
    finally:
        os.chdir(here)


class Repo:
    """A throwaway repo: `main` holds the base files, `work` the change."""

    def __init__(self, path: Path, base: dict):
        self.path = path
        git(path, "init", "-q", "-b", "main")
        git(path, "config", "user.email", "t@example.com")
        git(path, "config", "user.name", "t")
        self.commit(base)
        git(path, "checkout", "-q", "-b", "work")

    def commit(self, files: dict, message: str = "c"):
        for name, text in files.items():
            (self.path / name).write_text(text)
        git(self.path, "add", "-A")
        git(self.path, "commit", "-q", "--allow-empty", "-m", message)

    def check(self, *flags):
        return run_main(self.path, *flags, "main", "HEAD")


BUMP = """    /// Bumps the counter.
    #[inline]
    fn bump(&mut self) -> u8 {
        self.n + 1
    }
"""
APP = "pub struct App {\n    n: u8,\n}\n\nimpl App {\n" + BUMP + "\n    fn keep(&self) {}\n}\n"
APP_WITHOUT_BUMP = APP.replace(BUMP + "\n", "")
VIEW = "impl App {\n    fn view(&self) {}\n}\n"
VIEW_WITH_BUMP = "use super::App;\n\nimpl App {\n    fn view(&self) {}\n\n" + BUMP + "}\n"

INC = """/// Adds one.
#[must_use]
fn inc(x: u8) -> u8 {
    x + 1
}
"""


class PureMoves(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)

    def repo(self, base):
        return Repo(self.tmp, base)

    def test_a_method_moved_between_impl_blocks_in_two_files_passes(self):
        repo = self.repo({"app.rs": APP, "view.rs": VIEW})
        repo.commit({"app.rs": APP_WITHOUT_BUMP, "view.rs": VIEW_WITH_BUMP})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("moved    fn app.rs::impl App::bump -> view.rs::impl App::bump", out)
        self.assertIn("glue +  view.rs:1: use super::App;", out)

    def test_a_moved_fn_whose_body_changed_by_one_character_fails_with_a_diff(self):
        repo = self.repo({"app.rs": APP, "view.rs": VIEW})
        repo.commit({"app.rs": APP_WITHOUT_BUMP, "view.rs": VIEW_WITH_BUMP.replace("+ 1", "+ 2")})
        code, out = repo.check()
        self.assertEqual(code, 1, out)
        self.assertIn("CHANGED  fn app.rs::impl App::bump -> view.rs::impl App::bump", out)
        self.assertIn("-    self.n + 1", out)
        self.assertIn("+    self.n + 2", out)

    def test_widened_visibility_and_new_indentation_still_match(self):
        repo = self.repo({"a.rs": INC + "\nfn other() {}\n"})
        moved = "".join("    " + ln if ln else ln for ln in INC.splitlines(True))
        moved = moved.replace("    fn inc", "    pub(in crate::a) fn inc")
        repo.commit({"a.rs": "fn other() {}\n\npub mod util {\n" + moved + "}\n"})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("moved    fn a.rs::inc -> a.rs::util::inc", out)
        self.assertIn(", private -> pub(in crate::a))", out)
        self.assertIn("1 visibility widened", out)

    def test_a_visibility_widened_in_place_is_listed(self):
        repo = self.repo({"a.rs": INC})
        repo.commit({"a.rs": INC.replace("fn inc", "pub(crate) fn inc")})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("  widened  fn a.rs::inc (a.rs:1-5 -> a.rs:1-5, private -> pub(crate))", out)
        self.assertIn("0 unchanged, 0 moved, 0 changed, 0 added, 0 removed; 1 visibility widened", out)

    def test_a_widened_signature_that_rustfmt_wraps_still_matches(self):
        """Widening can push a signature past the line limit; rustfmt then puts
        each parameter on a line of its own with a trailing comma, which is
        layout, not a change."""
        before = "fn place(&self, at: u8) -> Option<(Vec<u8>, bool)> {\n    None\n}\n"
        after = (
            "pub(super) fn place(\n    &self,\n    at: u8,\n) -> Option<(Vec<u8>, bool)> {\n"
            "    None\n}\n"
        )
        repo = self.repo({"a.rs": before})
        repo.commit({"a.rs": "", "b.rs": after})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("moved    fn a.rs::place -> b.rs::place (a.rs:1-3 -> b.rs:1-6, private -> "
                      "pub(super), laid out anew by rustfmt)", out)
        self.assertIn("1 laid out anew by rustfmt", out)

    def test_a_one_item_tuple_is_not_its_item(self):
        """`(u8,)` is a tuple and `(u8)` is a `u8`: the comma is the type, so
        no layout rule may drop it, wrapped or not."""
        cases = [
            ("type Value = (u8,\n);\n", "pub(super) type Value = (u8);\n", "a wrapped alias"),
            ("type Value = (u8,);\n", "pub(super) type Value = (u8);\n", "an alias"),
            ("fn f(_: (u8,)) {}\n", "pub(super) fn f(\n    _: (u8),\n) {}\n", "a wrapped parameter"),
            ("fn f() -> (u8) {\n    0\n}\n", "pub(super) fn f() -> (\n    u8,\n) {\n    0\n}\n",
             "a wrapped return type"),
            ("const X: u8 = { let (a,) = (1,); a };\n",
             "pub(super) const X: u8 = {\n    let (a) = (1,);\n    a\n};\n", "a pattern"),
        ]
        for before, after, why in cases:
            with self.subTest(why=why), tempfile.TemporaryDirectory() as tmp:
                repo = Repo(Path(tmp), {"a.rs": before})
                repo.commit({"a.rs": "", "b.rs": after})
                code, out = repo.check()
                self.assertEqual(code, 1, out)
                self.assertIn("CHANGED", out)

    def test_a_widened_item_rustfmt_rewraps_in_place_is_a_widening(self):
        before = "impl A {\n    fn place(&self, at: u8) -> Option<(Vec<u8>, bool, String, String, String)> {\n        None\n    }\n}\n"
        after = ("impl A {\n    pub(super) fn place(\n        &self,\n        at: u8,\n"
                 "    ) -> Option<(Vec<u8>, bool, String, String, String)> {\n        None\n    }\n}\n")
        repo = self.repo({"a.rs": before})
        repo.commit({"a.rs": after})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("widened  fn a.rs::impl A::place", out)
        self.assertIn("private -> pub(super), laid out anew by rustfmt", out)

    def test_text_rustfmt_cannot_read_never_matches(self):
        self.assertIsNone(pm.rustfmt("fn (", "2021"))
        broken = pm.Item("a.rs", "fn", "f", [], "", 0, 0, "fn (", "")
        self.assertFalse(pm.laid_out_alike(broken, broken, "2021"))

    def test_a_change_inside_a_wrapped_signature_still_fails(self):
        before = "fn place(&self, at: u8) -> bool {\n    true\n}\n"
        cases = [
            ("pub(super) fn place(\n    &self,\n    at: u16,\n) -> bool {\n", "a parameter's type"),
            ("pub(super) fn place(\n    &mut self,\n    at: u8,\n) -> bool {\n", "the receiver"),
            ("pub(super) fn place(\n    &self,\n) -> bool {\n", "a parameter dropped"),
            ("pub(super) fn place(\n    &self, // at: u8,\n) -> bool {\n", "a parameter commented out"),
        ]
        for head, why in cases:
            with self.subTest(why=why), tempfile.TemporaryDirectory() as tmp:
                repo = Repo(Path(tmp), {"a.rs": before})
                repo.commit({"a.rs": "", "b.rs": head + "    true\n}\n"})
                code, out = repo.check()
                self.assertEqual(code, 1, out)
                self.assertIn("CHANGED  fn a.rs::place -> b.rs::place", out)

    def test_a_trailing_comma_inside_a_macro_call_is_a_change(self):
        """`m!(1,)` and `m!(1)` are different inputs: a macro can match them
        apart. A trailing comma rustfmt adds to a wrapped list is layout; one
        in a macro call is not, so it is compared as written."""
        cases = [
            ("choose!(1)", "choose!(\n    1,\n)", "a wrapped call"),
            ("outer!(inner!(1))", "outer!(inner!(\n    1,\n))", "a nested call"),
            ("[1, choose!(2)]", "[\n    1,\n    choose!(\n        2,\n    ),\n]", "a call in a list"),
        ]
        for value, wrapped, why in cases:
            before = f"const X: u8 = {value};\n"
            after = f"pub(super) const X: u8 = {wrapped};\n"
            with self.subTest(why=why), tempfile.TemporaryDirectory() as tmp:
                repo = Repo(Path(tmp), {"a.rs": before})
                repo.commit({"a.rs": "", "b.rs": after})
                code, out = repo.check()
                self.assertEqual(code, 1, out)
                self.assertIn("CHANGED  const a.rs::X -> b.rs::X", out)

    def test_a_wrapped_array_and_a_wrapped_macro_call_still_match(self):
        before = 'const A: [u8; 2] = [1, 2];\n\nconst B: &str = concat!("a", "b");\n'
        after = ('pub(super) const A: [u8; 2] = [\n    1,\n    2,\n];\n\n'
                 'pub(super) const B: &str = concat!(\n    "a",\n    "b"\n);\n')
        repo = self.repo({"a.rs": before})
        repo.commit({"a.rs": "", "b.rs": after})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("2 visibility widened", out)

    def test_code_after_a_comment_in_a_signature_is_not_the_comment(self):
        """A `//` comment ends at its line: a parameter on the next line is
        code, and the same words on the comment's own line are not."""
        live = "fn place(\n    // at the start\n    at: u8,\n) -> bool {\n    true\n}\n"
        dead = "fn place(\n    // at the start at: u8\n) -> bool {\n    true\n}\n"
        repo = self.repo({"a.rs": live})
        repo.commit({"a.rs": "", "b.rs": dead})
        code, out = repo.check()
        self.assertEqual(code, 1, out)
        self.assertIn("CHANGED  fn a.rs::place -> b.rs::place", out)

    def test_literals_in_a_wrapped_declaration_are_compared_as_written(self):
        before = 'const NAME: &str = "a\n  b";\n'
        after = 'pub(super) const NAME: &str =\n    "a\n b";\n'
        repo = self.repo({"a.rs": before})
        repo.commit({"a.rs": "", "b.rs": after})
        code, out = repo.check()
        self.assertEqual(code, 1, out)
        self.assertIn("CHANGED  const a.rs::NAME -> b.rs::NAME", out)

    def test_narrowed_or_incomparable_visibility_fails(self):
        """Only a wider reach is a pure move: a narrower one can hide what a
        caller used, in place or while moving, and two scopes that do not
        compare cannot be shown to be wider."""
        cases = [
            ("pub fn inc", "fn inc", "pub -> private"),
            ("pub(crate) fn inc", "pub(in crate::a) fn inc", "pub(crate) -> pub(in crate::a)"),
            ("pub(super) fn inc", "pub(in crate::a) fn inc", "pub(super) -> pub(in crate::a)"),
        ]
        for before, after, why in cases:
            for moved in (False, True):
                with self.subTest(why=why, moved=moved), tempfile.TemporaryDirectory() as tmp:
                    repo = Repo(Path(tmp), {"a.rs": INC.replace("fn inc", before)})
                    target = "b.rs" if moved else "a.rs"
                    files = {target: INC.replace("fn inc", after)}
                    if moved:
                        files["a.rs"] = ""
                    repo.commit(files)
                    code, out = repo.check()
                    self.assertEqual(code, 1, out)
                    self.assertIn(f"CHANGED  fn a.rs::inc -> {target}::inc", out)
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp), {"a.rs": INC.replace("fn inc", "pub(in crate::a) fn inc")})
            repo.commit({"a.rs": "", "b.rs": INC.replace("fn inc", "pub(crate) fn inc")})
            code, out = repo.check()
            self.assertEqual(code, 0, out)

    def test_identical_items_moving_together_pair_by_visibility(self):
        """A private helper and an identical `pub(crate)` one, moved to two
        new files in the other order, is a pure move: neither narrows."""
        helper = "fn helper() -> u8 {\n    1\n}\n"
        repo = self.repo({"a.rs": helper, "b.rs": "pub(crate) " + helper})
        repo.commit({"a.rs": "", "b.rs": "", "c.rs": "pub(crate) " + helper, "d.rs": helper})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("moved    fn a.rs::helper -> d.rs::helper", out)
        self.assertIn("moved    fn b.rs::helper -> c.rs::helper", out)

    def test_an_inner_attribute_of_an_impl_belongs_to_its_methods(self):
        """`#![cfg(test)]` inside an impl guards every method in it, so a
        method moved out of the block without it is not the same method."""
        old = "impl App {\n    #![cfg(test)]\n\n    fn a() {}\n\n    fn b() {}\n}\n"
        repo = self.repo({"a.rs": old})
        repo.commit({
            "a.rs": "impl App {\n    #![cfg(test)]\n\n    fn a() {}\n}\n",
            "b.rs": "impl App {\n    fn b() {}\n}\n",
        })
        code, out = repo.check()
        self.assertEqual(code, 1, out)
        self.assertIn("CHANGED  fn a.rs::impl App #![cfg(test)]::b -> b.rs::impl App::b", out)

    def test_an_empty_impl_block_is_an_item(self):
        """With no method to carry its header, an added or dropped empty
        impl (`unsafe impl Send`) would otherwise pass as glue."""
        repo = self.repo({"a.rs": "struct X(*const u8);\n"})
        repo.commit({"a.rs": "struct X(*const u8);\n\nunsafe impl Send for X {\n}\n"})
        code, out = repo.check()
        self.assertEqual(code, 1, out)
        self.assertIn("ADDED    impl a.rs::unsafe impl Send for X", out)

    def test_glue_is_told_apart_by_the_module_it_sits_in(self):
        """Swapping two `use` lines between inline modules changes what each
        module's code calls, so the report lists it."""
        one, two = "use crate::one::f;", "use crate::two::f;"
        repo = self.repo({"a.rs": f"mod x {{\n    {one}\n}}\n\nmod y {{\n    {two}\n}}\n"})
        repo.commit({"a.rs": f"mod x {{\n    {two}\n}}\n\nmod y {{\n    {one}\n}}\n"})
        _, out = repo.check()
        self.assertIn(f"glue -  a.rs:2 in x: {one}", out)
        self.assertIn(f"glue +  a.rs:2 in x: {two}", out)

    def test_docs_and_attributes_left_behind_fail(self):
        """Rust hands a stranded doc or attribute to the next item, so an
        item that moved without them changed two items, not zero."""
        for left in ("/// Adds one.\n", "#[must_use]\n"):
            with self.subTest(left=left), tempfile.TemporaryDirectory() as tmp:
                repo = Repo(Path(tmp), {"a.rs": INC + "\nfn other() {}\n"})
                repo.commit({"a.rs": left + "\nfn other() {}\n", "b.rs": INC.replace(left, "")})
                code, out = repo.check()
                self.assertEqual(code, 1, out)
                self.assertIn("CHANGED  fn a.rs::inc -> b.rs::inc", out)
                self.assertIn("CHANGED  fn a.rs::other -> a.rs::other", out)

    def test_a_method_under_another_impl_header_is_not_a_move(self):
        """The header's code, attributes and `where` clause included, is part
        of a method's identity: each of these moves changes what it means."""
        headers = [
            ("impl Display for X {", "impl Debug for X {"),
            ("#[cfg(test)]\nimpl App {", "impl App {"),
            ("impl<T> W<T>\nwhere\n    T: Copy,\n{", "impl<T> W<T> {"),
        ]
        for old, new in headers:
            with self.subTest(old=old), tempfile.TemporaryDirectory() as tmp:
                repo = Repo(Path(tmp), {"a.rs": old + "\n    fn f(&self) {}\n}\n"})
                repo.commit({"a.rs": new + "\n    fn f(&self) {}\n}\n"})
                code, out = repo.check()
                self.assertEqual(code, 1, out)
                header = " ".join(old[:-1].split())
                self.assertIn(f"CHANGED  fn a.rs::{header}::f", out)
                self.assertIn("same text, under a different impl header", out)

    def test_an_edited_doc_or_comment_on_an_impl_header_is_listed(self):
        repo = self.repo({"a.rs": "/// Old.\nimpl App { // here\n    fn f() {}\n} // App\n"})
        repo.commit({"a.rs": "/// New.\nimpl App { // there\n    fn f() {}\n} // end\n"})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("glue -  a.rs:1: /// Old.\n          impl App { // here", out)
        self.assertIn("glue +  a.rs:1: /// New.\n          impl App { // there", out)
        self.assertIn("glue -  a.rs:4: } // App", out)

    def test_reindenting_a_multi_line_string_changes_it(self):
        repo = self.repo({"a.rs": 'const S: &str = "a\nb";\n'})
        repo.commit({"a.rs": 'mod m {\n    const S: &str = "a\n    b";\n}\n'})
        self.assertEqual(repo.check()[0], 1)

    def test_a_renamed_file_is_a_pure_move(self):
        repo = self.repo({"a.rs": APP})
        git(repo.path, "mv", "a.rs", "b.rs")
        repo.commit({})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("moved    struct a.rs::App -> b.rs::App", out)

    def test_a_file_with_a_non_ascii_name_is_checked(self):
        repo = self.repo({"é.rs": INC})
        repo.commit({"é.rs": INC.replace("+ 1", "+ 2")})
        code, out = repo.check()
        self.assertEqual(code, 1, out)
        self.assertIn("CHANGED  fn é.rs::inc", out)

    def test_glue_is_listed_in_file_order(self):
        repo = self.repo({"a.rs": "//! Zebra.\n//! Apple.\n\nfn f() {}\n"})
        repo.commit({"a.rs": "fn f() {}\n"})
        code, out = repo.check()
        self.assertEqual(code, 0, out)
        self.assertIn("glue -  a.rs:1: //! Zebra.\n  glue -  a.rs:2: //! Apple.", out)

    def test_if_declared_only_enforces_with_the_trailer(self):
        repo = self.repo({"a.rs": INC})
        repo.commit({"a.rs": INC.replace("+ 1", "+ 2")})
        code, out = repo.check("--if-declared")
        self.assertEqual(code, 0, out)
        self.assertIn("CHANGED  fn a.rs::inc", out)
        self.assertNotIn("+    x + 2", out, "an undeclared range leaves the diffs out")
        self.assertIn("Not enforced", out)
        self.assertEqual(repo.check()[0], 1, "without the flag it always enforces")
        repo.commit({}, "Split the file\n\npure-move: true\n")
        code, out = repo.check("--if-declared")
        self.assertEqual(code, 1, out)
        self.assertIn("+    x + 2", out)


TRICKY = r"""fn braces<'a>(s: &'a str) -> char {
    let _ = "}{";
    let _ = r#"}"{"#;
    let _ = br##"}"#}"##;
    let _ = b"}\"{";
    let _ = ['{', '\'', b'}', '\u{7d}', '"'];
    let r#type = 1; // }
    /* { /* } */ { */
    'outer: loop {
        break 'outer;
    }
    let _ = "multi
}line";
    '}'
}

/// After it.
fn after() {}
"""


class Parser(unittest.TestCase):
    def test_braces_in_literals_comments_and_lifetimes_are_not_structure(self):
        code = pm.lex(TRICKY)[0]
        # Only the three real blocks survive, no quote of any literal does,
        # and the four `'` left are the lifetime and the label.
        self.assertEqual([code.count(c) for c in "{}\"'"], [3, 3, 0, 4], code)
        items, _ = pm.parse(TRICKY)
        self.assertEqual(
            [(it.name, it.start + 1, it.end + 1) for it in items],
            [("braces", 1, 15), ("after", 17, 18)],
        )

    def test_an_impl_header_keeps_its_docs_and_comments_in_the_glue(self):
        """Only the header's code names its methods, but every line of it
        is glue, so an edited doc or comment there is still listed."""
        text = "/// Doc.\n#[cfg(test)]\nimpl App { // note\n    fn f() {}\n} // end\n"
        items, glue = pm.parse(text)
        self.assertEqual([(it.name, it.ctx) for it in items], [("f", "#[cfg(test)] impl App")])
        self.assertEqual(
            [g.text for g in glue if g.kind in ("header", "close")],
            ["/// Doc.\n#[cfg(test)]\nimpl App { // note", "} // end"],
        )

    def test_every_line_of_a_block_inner_doc_is_glue(self):
        items, glue = pm.parse("/*! Crate doc,\n    on two lines.\n*/\n\nfn a() {}\n")
        self.assertEqual([it.name for it in items], ["a"])
        self.assertEqual([g.kind for g in glue], ["doc", "doc", "doc", "blank", "blank"])

    def test_the_braces_of_a_const_generic_argument_are_not_an_impl_body(self):
        text = "impl Tr for Foo<{ 1 }> {\n    fn a(&self) {}\n}\n\nfn f() -> Vec<u8> {\n    vec![]\n}\n"
        items, _ = pm.parse(text)
        self.assertEqual(
            [(it.name, it.ctx) for it in items], [("a", "impl Tr for Foo<{ 1 }>"), ("f", "")]
        )

    def test_a_second_item_on_the_line_where_one_ends_is_refused(self):
        """Read as one item, the second would be hidden inside the first."""
        for text in ("fn a() {} fn b() {}\n", "const A: u8 = 1; const B: u8 = 2;\n"):
            with self.subTest(text=text), self.assertRaises(pm.ParseError):
                pm.parse(text)
        items, _ = pm.parse("fn a() {} // b\nconst A: u8 = 1; /* c */\n")
        self.assertEqual([it.name for it in items], ["a", "A"])

    def test_a_shebang_line_is_glue_and_an_inner_attribute_is_not(self):
        items, glue = pm.parse("#!/usr/bin/env rust-script\nfn main() {}\n")
        self.assertEqual([it.name for it in items], ["main"])
        self.assertEqual(glue[0].kind, "shebang")
        items, _ = pm.parse("#![allow(dead_code)]\nfn main() {}\n")
        self.assertEqual([it.kind for it in items], ["attr", "fn"])

    def test_whitespace_inside_a_string_in_glue_or_a_header_is_kept(self):
        """`#[path = "a  b.rs"]` loads another file than `"a b.rs"`."""
        one = pm.parse('#[path = "a b.rs"]\nmod child;\n')[1]
        two = pm.parse('#[path = "a  b.rs"]\nmod child;\n')[1]
        self.assertNotEqual([g.text for g in one], [g.text for g in two])
        spaced = pm.parse('#[path   =   "a b.rs"]\nmod   child;\n')[1]
        self.assertEqual([g.text for g in one], [g.text for g in spaced])
        items, _ = pm.parse('#[doc = "x  y"]\nimpl   A {\n    fn f() {}\n}\n')
        self.assertEqual(items[0].ctx, '#[doc = "x  y"] impl A')

    def test_list_prints_each_item_with_its_line_range(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        path = Path(tmp.name) / "a.rs"
        path.write_text(
            "const X: u8 = 1;\n\nimpl Foo {\n    /// Doc.\n    #[inline]\n"
            '    fn a(&self) {\n        let _ = "}";\n    }\n}\n'
        )
        code, out = run_main(".", "list", str(path))
        self.assertEqual(code, 0)
        self.assertEqual(out, "const\tX\t-\t1-1\t1\nfn\ta\timpl Foo\t4-8\t5\n")

    def test_every_real_file_parses_and_every_line_is_accounted_for(self):
        """The parser fails closed on anything it cannot balance, so the
        proof it keeps up with the real tree is that every file under src/
        parses and its items and glue cover each line exactly once."""
        src = Path(__file__).resolve().parents[2] / "src"
        files = sorted(src.rglob("*.rs"))
        self.assertGreater(len(files), 50, "the control corpus is missing")
        bad = []
        for f in files:
            text = f.read_text()
            try:
                items, glue = pm.parse(text)
            except pm.ParseError as e:
                bad.append((str(f.relative_to(src)), str(e)))
                continue
            spans = sorted([(i.start, i.end) for i in items] + [(g.start, g.end) for g in glue])
            covered = [n for s, e in spans for n in range(s, e + 1)]
            if covered != list(range(len(text.split("\n")))):
                bad.append((str(f.relative_to(src)), "lines not covered exactly once"))
        self.assertEqual(bad, [])


if __name__ == "__main__":
    unittest.main()
