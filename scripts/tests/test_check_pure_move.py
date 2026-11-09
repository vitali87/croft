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
                self.assertIn(f"CHANGED  fn a.rs::{pm.collapse([old[:-1]])}::f", out)
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
