"""Tests for the parser in scripts/check_pure_move.py.

A proof that a split only moves code is only as good as the parser under it:
a brace it miscounts, or a line it drops, is a hole in that proof.
"""

from __future__ import annotations

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import check_pure_move as pm  # noqa: E402


def run_main(cwd, *args):
    out, here = io.StringIO(), os.getcwd()
    os.chdir(cwd)
    try:
        with contextlib.redirect_stdout(out):
            return pm.main(list(args)), out.getvalue()
    finally:
        os.chdir(here)


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
