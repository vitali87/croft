"""Tests for scripts/ships_nothing.py: the release gate's test-only filter.

The `release notes` job says in its own comment that
"test-only changes ship nothing and are exempt", and then encodes test-only
as a path list. croft keeps most unit tests in a `#[cfg(test)] mod tests`
beside the code they cover, so the ordinary shape of a test-only fix here is
a diff inside a shipped file (#461).

The direction of failure matters more than the coverage: waiving notes that
were needed ships a change no release covers, so two different binaries can
share a version, while asking for notes that were not needed only costs a line
of notes. Anything the parse is unsure about therefore counts as shipped, and
the tests below pin that as much as they pin the exemption.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ships_nothing as filt  # noqa: E402


SHIPPED = '''pub fn width(cols: u16) -> u16 {
    cols / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(width(4), 2);
    }
}
'''


def run(cwd, *args):
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True).stdout


class Repo:
    """A throwaway repo with one shipped file and a branch off main."""

    def __init__(self, tmp: Path):
        self.path = tmp
        run(tmp, "git", "init", "-q", "-b", "main")
        run(tmp, "git", "config", "user.email", "t@example.com")
        run(tmp, "git", "config", "user.name", "t")

    def commit(self, name: str, text: str):
        (self.path / name).write_text(text)
        run(self.path, "git", "add", "-A")
        run(self.path, "git", "commit", "-q", "-m", "c")

    def verdict(self, path="a.rs", base="main", head="HEAD"):
        return filt.ships_nothing(base, head, path, cwd=self.path)


class Ranges(unittest.TestCase):
    def test_a_cfg_test_module_is_found(self):
        self.assertEqual(filt.cfg_test_ranges(SHIPPED), [(5, 13)])

    def test_a_brace_in_a_string_does_not_end_the_block_early(self):
        """The block tracker has to read Rust, not count characters: a
        `format!("{}")` inside a test would otherwise close the module and
        leave every line after it looking like shipped code."""
        text = (
            "fn f() {}\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            '    const S: &str = "}";\n'
            "    fn g() {}\n"
            "}\n"
        )
        self.assertEqual(filt.cfg_test_ranges(text), [(2, 6)])

    def test_a_cfg_test_item_that_is_not_a_module_is_not_a_range(self):
        """Deliberately narrow. `#[cfg(test)]` on a free function compiles
        out too, but reading its extent is a second parser, and the cost of
        being wrong here is a waived bump."""
        text = "fn f() {}\n#[cfg(test)]\nfn helper() -> u8 {\n    1\n}\n"
        self.assertEqual(filt.cfg_test_ranges(text), [])

    def test_an_attribute_inside_a_block_comment_is_not_a_range(self):
        """#2217 review: `#[cfg(test)]` inside `/* */` is comment text and
        guards nothing; the `mod helper;` below it ships."""
        text = "/*\n#[cfg(test)]\n*/\nmod helper;\nfn main() {}\n"
        self.assertEqual(filt.cfg_test_ranges(text), [])

    def test_a_comment_line_that_ends_in_code_yields_no_ranges(self):
        """#2217 review: `/* comment */ }` closes the module after its
        comment; a reading that skips the line as a comment never counts the
        brace, and the span runs on over shipped code."""
        text = (
            "mod outer {\n    #[cfg(test)]\n    mod tests {\n    /* comment */ }\n"
            '    pub const SRC: &str = include_str!("helper.rs");\n}\n'
        )
        self.assertEqual(filt.cfg_test_ranges(text), [])

    def test_lines_are_counted_as_git_counts_them(self):
        """A U+2028 inside a string is one character to Rust and git, not a
        line break, so the span after it keeps git's line numbers."""
        text = 'const S: &str = "a\u2028b";\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n'
        self.assertEqual(filt.cfg_test_ranges(text), [(2, 5)])

    def test_a_file_with_no_tests_has_no_ranges(self):
        self.assertEqual(filt.cfg_test_ranges("fn f() {}\n"), [])


class Verdicts(unittest.TestCase):
    def test_a_change_inside_the_test_module_ships_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            repo.commit("b.rs", "fn b() {}\n")
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", SHIPPED.replace("width(4), 2", "width(6), 3"))
            self.assertTrue(repo.verdict())

    def test_a_change_outside_the_test_module_ships(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", SHIPPED.replace("cols / 2", "cols / 3"))
            self.assertFalse(repo.verdict())

    def test_a_change_to_both_ships(self):
        """The one that must not be read as test-only because most of it is."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit(
                "a.rs",
                SHIPPED.replace("cols / 2", "cols / 3").replace("width(4), 2", "width(6), 2"),
            )
            self.assertFalse(repo.verdict())

    def test_deleting_a_test_ships_nothing(self):
        """A removal has no line at the head to inspect, so the base side of
        the diff has to be read as well. Checking only the head would waive
        a deletion of shipped code just as readily."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", SHIPPED.replace("        assert_eq!(width(4), 2);\n", ""))
            self.assertTrue(repo.verdict())

    def test_deleting_shipped_code_ships(self):
        """The other half of reading the base side, and the half that has to
        be a PURE deletion: an edited line leaves a replacement at the head
        that a head-only check would catch anyway, so it proves nothing."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            deleted = "pub fn legacy() -> u16 {\n    1\n}\n\n"
            repo.commit("a.rs", deleted + SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", SHIPPED)
            self.assertFalse(repo.verdict())

    def test_a_test_module_that_grows_ships_nothing(self):
        """Added lines land at the head, and the range they have to fall in
        is the one at the head, not the one at the base."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit(
                "a.rs",
                SHIPPED.replace(
                    "    #[test]\n",
                    "    #[test]\n    fn also() {\n        assert_eq!(width(2), 1);\n    }\n\n    #[test]\n",
                    1,
                ),
            )
            self.assertTrue(repo.verdict())

    def test_a_file_that_is_not_rust_ships(self):
        """A baked-in asset can quote Rust - a doc page with a fenced test
        module is the ordinary case - and a Rust parse turned loose on it
        would waive an edit to shipped content because the surrounding prose
        happens to look like a test module."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            page = (
                "Testing conventions\n\n```rust\n#[cfg(test)]\nmod tests {\n"
                "    #[test]\n    fn shown_to_the_reader() {}\n}\n```\n"
            )
            repo.commit("a.rs", SHIPPED)
            repo.commit("guide.md", page)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("guide.md", page.replace("shown_to_the_reader", "renamed"))
            self.assertFalse(repo.verdict(path="guide.md"))

    def test_an_added_file_that_is_only_tests_ships_nothing(self):
        """No base version at all, so every line is an addition and the head
        ranges decide it on their own."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit(
                "c.rs",
                "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n",
            )
            self.assertTrue(repo.verdict(path="c.rs"))

    def test_adding_the_cfg_test_attribute_to_a_shipped_module_ships(self):
        """The line that decides what compiles. Adding `#[cfg(test)]` above a
        module that ships today REMOVES it from the binary, and the diff is a
        single added line which falls inside the span the attribute opens - so
        a span that swallows its own attribute waives the one change the job
        exists to catch."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            shipped = "pub fn f() -> u8 {\n    1\n}\n\nmod helpers {\n    pub fn g() -> u8 {\n        2\n    }\n}\n"
            repo.commit("a.rs", shipped)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", shipped.replace("\nmod helpers {", "\n#[cfg(test)]\nmod helpers {"))
            self.assertFalse(repo.verdict())

    def test_removing_the_cfg_test_attribute_ships(self):
        """The same line, the other way: the module and everything in it
        ENTERS the binary."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            gated = "pub fn f() -> u8 {\n    1\n}\n\n#[cfg(test)]\nmod helpers {\n    pub fn g() -> u8 {\n        2\n    }\n}\n"
            repo.commit("a.rs", gated)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", gated.replace("#[cfg(test)]\n", ""))
            self.assertFalse(repo.verdict())

    def test_a_whole_test_module_added_at_once_still_ships_nothing(self):
        """The control for the two above, and the PR's main use case: when
        the WHOLE module arrives in one diff, every line of the span is new
        and nothing leaves or enters the binary."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "pub fn f() -> u8 {\n    1\n}\n")
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit(
                "a.rs",
                "pub fn f() -> u8 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n"
                "    #[test]\n    fn t() {\n        assert_eq!(super::f(), 1);\n    }\n}\n",
            )
            self.assertTrue(repo.verdict())

    def test_a_module_left_open_at_end_of_file_is_not_a_range(self):
        """A truncated file or a miscount. Treating the rest of the file as
        test code would exempt everything below the attribute."""
        text = "fn f() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n"
        self.assertEqual(filt.cfg_test_ranges(text), [])

    def test_a_module_declaration_without_a_body_spans_only_itself(self):
        """`#[cfg(test)] mod tests;` puts the body in another file and opens
        no braces: the span is the attribute and the declaration, and must
        not run on over the shipped code below it."""
        text = "#[cfg(test)]\nmod tests;\npub fn shipped() -> u8 {\n    1\n}\n"
        self.assertEqual(filt.cfg_test_ranges(text), [(1, 2)])

    def test_a_glob_in_a_string_is_not_a_block_comment(self):
        """`/*` inside a string opens no comment. Reading the raw line made
        any file holding a glob (`"src/**/*.rs"`) ship whatever changed."""
        text = (
            'pub fn glob() -> &\'static str {\n    "src/**/*.rs"\n}\n'
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n"
        )
        self.assertEqual(filt.cfg_test_ranges(text), [(4, 8)])

    def test_a_doc_comment_between_the_attribute_and_its_module_is_read(self):
        """`classify` labels `///` as DOC, not SKIP, and a documented test
        module is an ordinary shape. Missing it loses the exemption rather
        than waiving wrongly, but it loses it silently."""
        text = (
            "fn f() {}\n#[cfg(test)]\n/// Unit tests for the parser.\n"
            "mod tests {\n    #[test]\n    fn t() {}\n}\n"
        )
        self.assertEqual(filt.cfg_test_ranges(text), [(2, 7)])

    def test_a_path_the_pathspec_cannot_resolve_ships(self):
        """An empty diff means "nothing changed" for a path that exists at
        both ends, and "I could not read that" otherwise. Only the first is a
        reason to waive a bump."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", SHIPPED.replace("width(4), 2", "width(6), 3"))
            self.assertFalse(repo.verdict(path="does_not_exist.rs"))

    def test_a_blank_line_inside_a_string_literal_ships(self):
        """A blank line is only whitespace when it is CODE. Inside a raw
        string it is content: croft's model prompt and its tree-sitter
        queries are multi-line literals, and a blank line added or removed
        there changes what the binary does. There are 42 such lines in `src/`
        today, so this is not hypothetical."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            before = 'pub const P: &str = r#"\ncroft\n\nv1\n"#;\n'
            after = 'pub const P: &str = r#"\ncroft\nv1\n"#;\n'
            repo.commit("a.rs", before)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", after)
            self.assertFalse(repo.verdict(), "a blank line left a string literal")

    def test_a_blank_line_added_to_a_string_literal_ships(self):
        """The other direction, and the one that pins the removed-side half
        of the blank filter: with only the added-side half tested, deleting
        the removed-side line leaves the suite green."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            before = 'pub const P: &str = r#"\ncroft\nv1\n"#;\n'
            after = 'pub const P: &str = r#"\ncroft\n\nv1\n"#;\n'
            repo.commit("a.rs", before)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", after)
            self.assertFalse(repo.verdict(), "a blank line joined a string literal")

    def test_a_whole_test_module_in_its_real_shape_ships_nothing(self):
        """Every one of the 159 `#[cfg(test)]` modules in `src/` has a blank
        line in it. Stripping blank lines from the touched set before asking
        whether the whole span moved made the exemption's main case
        unreachable for every module shaped like the ones already here."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "pub fn f() -> u8 {\n    1\n}\n")
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit(
                "a.rs",
                "pub fn f() -> u8 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n"
                "    use super::*;\n\n    #[test]\n    fn t() {\n"
                "        assert_eq!(f(), 1);\n    }\n}\n",
            )
            self.assertTrue(repo.verdict(), "the module arrived in one piece")

    def test_deleting_a_whole_test_module_in_its_real_shape_ships_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            whole = (
                "pub fn f() -> u8 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n"
                "    use super::*;\n\n    #[test]\n    fn t() {\n"
                "        assert_eq!(f(), 1);\n    }\n}\n"
            )
            repo = Repo(Path(tmp))
            repo.commit("a.rs", whole)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", "pub fn f() -> u8 {\n    1\n}\n")
            self.assertTrue(repo.verdict(), "and the module leaving in one piece")

    def test_a_diff_git_reports_as_binary_ships(self):
        """`git diff -U0` on a file git calls binary prints no `@@` header at
        all, so an inferred "nothing changed" is wrong in the waiving
        direction."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            (Path(tmp) / "a.rs").write_bytes(b"pub fn f() -> u8 {\n    1\n}\n\x00\n")
            run(repo.path, "git", "add", "-A")
            run(repo.path, "git", "commit", "-q", "-m", "c")
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            (Path(tmp) / "a.rs").write_bytes(b"pub fn f() -> u8 {\n    9\n}\n\x00\n")
            run(repo.path, "git", "add", "-A")
            run(repo.path, "git", "commit", "-q", "-m", "c2")
            self.assertFalse(repo.verdict())

    def test_a_mid_line_block_comment_yields_no_ranges(self):
        """The brace tracker strips strings and `//`, not `/* */`, so a brace
        inside a block comment is counted as structure and the span can
        swallow shipped code below it. No range at all is the safe answer."""
        text = (
            "#[cfg(test)]\nmod tests {\n    fn t() {\n        let _x = 0; /* { */\n"
            "    }\n}\n\npub fn g() -> u8 {\n    let _ = 0; /* } */\n    1\n}\n"
        )
        self.assertEqual(filt.cfg_test_ranges(text), [])

    def test_a_block_comment_right_after_a_string_yields_no_ranges(self):
        """#2217 review: with the string removed, `"x" /* { */` leaves the
        comment at the start of the line's code, where a mid-line pattern
        missed it and the braces inside it were counted."""
        text = (
            "#[cfg(test)]\nmod tests {\n    fn t() -> &'static str {\n"
            '        "x" /* { */\n    }\n}\n'
            "pub fn g() -> &'static str {\n    \"y\" /* } */\n}\n"
        )
        self.assertEqual(filt.cfg_test_ranges(text), [])

    def test_an_attribute_and_module_inside_a_string_are_not_a_range(self):
        """#2217 review: lines inside a multi-line raw string are its
        content, not items, however much they look like a test module."""
        text = (
            'const PROMPT: &str = r#"\n#[cfg(test)]\nmod old;\n"#;\n'
            'fn main() { print!("{PROMPT}"); }\n'
        )
        self.assertEqual(filt.cfg_test_ranges(text), [])

    def test_editing_a_string_that_looks_like_a_test_module_ships(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            before = 'const PROMPT: &str = r#"\n#[cfg(test)]\nmod old;\n"#;\n'
            repo.commit("a.rs", before)
            run(repo.path, "git", "checkout", "-q", "-b", "change")
            repo.commit("a.rs", before.replace("mod old;", "mod new;"))
            self.assertFalse(repo.verdict())

    def test_a_declaration_slipped_under_an_existing_attribute_ships(self):
        """#2217 review: `mod tests;` inserted between `#[cfg(test)]` and the
        function it guarded takes the attribute off that function, which then
        ships, though every touched line sits inside the new span."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "#[cfg(test)]\npub fn helper() {}\n")
            run(repo.path, "git", "checkout", "-q", "-b", "change")
            repo.commit("a.rs", "#[cfg(test)]\nmod tests;\npub fn helper() {}\n")
            self.assertFalse(repo.verdict())

    def test_a_module_slipped_under_an_existing_attribute_ships(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "#[cfg(test)]\npub fn helper() {}\n")
            run(repo.path, "git", "checkout", "-q", "-b", "change")
            repo.commit(
                "a.rs",
                "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\npub fn helper() {}\n",
            )
            self.assertFalse(repo.verdict())

    def test_removing_a_declaration_from_under_its_attribute_ships(self):
        """The reverse: the attribute now guards the function below, which
        leaves the release build."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "#[cfg(test)]\nmod tests;\npub fn helper() {}\n")
            run(repo.path, "git", "checkout", "-q", "-b", "change")
            repo.commit("a.rs", "#[cfg(test)]\npub fn helper() {}\n")
            self.assertFalse(repo.verdict())

    def test_a_declaration_slipped_above_a_test_module_ships(self):
        """#2217 review: `mod added;` between an existing `#[cfg(test)]` and
        the module it guarded takes the attribute off that module, which
        then ships, though the attribute opens a span on both sides."""
        module = "mod existing {\n    fn t() {}\n}\n"
        for base, head in (
            ("#[cfg(test)]\n" + module, "#[cfg(test)]\nmod added;\n" + module),
            ("#[cfg(test)]\nmod added;\n" + module, "#[cfg(test)]\n" + module),
        ):
            with self.subTest(base=base, head=head), tempfile.TemporaryDirectory() as tmp:
                repo = Repo(Path(tmp))
                repo.commit("a.rs", base)
                run(repo.path, "git", "checkout", "-q", "-b", "change")
                repo.commit("a.rs", head)
                self.assertFalse(repo.verdict())

    def test_a_test_module_that_grows_over_shipped_code_ships(self):
        """A brace added inside a test module can move its end below shipped
        code that did not change: that code leaves the release build."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", "#[cfg(test)]\nmod t {\n}\nfn shipped() {}\n")
            run(repo.path, "git", "checkout", "-q", "-b", "change")
            repo.commit(
                "a.rs", "#[cfg(test)]\nmod t {\nfn helper() {\n}\nfn shipped() {}\n}\n"
            )
            self.assertFalse(repo.verdict())

    def test_edits_in_two_test_modules_that_shift_each_other_ship_nothing(self):
        """The attribute check follows each span to its line on the other
        side, past the lines an earlier hunk added."""
        two = (
            "pub fn a() {}\n#[cfg(test)]\nmod one {\n    fn t() {}\n}\n"
            "pub fn b() {}\n#[cfg(test)]\nmod two {\n    fn t() {}\n}\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", two)
            run(repo.path, "git", "checkout", "-q", "-b", "change")
            repo.commit(
                "a.rs",
                two.replace("    fn t() {}\n", "    fn t() {}\n    fn u() {}\n    fn v() {}\n"),
            )
            self.assertTrue(repo.verdict())

    def test_the_cli_reports_the_verdict_as_its_exit_status(self):
        """The gate is shell, so the answer has to arrive as a status."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = Repo(Path(tmp))
            repo.commit("a.rs", SHIPPED)
            run(repo.path, "git", "checkout", "-q", "-b", "work")
            repo.commit("a.rs", SHIPPED.replace("width(4), 2", "width(6), 3"))
            script = str(Path(__file__).resolve().parents[1] / "ships_nothing.py")
            done = subprocess.run(
                [sys.executable, script, "main", "HEAD", "a.rs"],
                cwd=repo.path,
                capture_output=True,
                text=True,
            )
            self.assertEqual(done.returncode, 0, done.stderr)
            # And the direction that matters, in the same test: a CLI that
            # always exits 0 waives every bump, and the assertion above alone
            # cannot tell that apart from a working filter.
            repo.commit("a.rs", SHIPPED.replace("cols / 2", "cols / 3"))
            ships = subprocess.run(
                [sys.executable, script, "main", "HEAD", "a.rs"],
                cwd=repo.path,
                capture_output=True,
                text=True,
            )
            self.assertEqual(ships.returncode, 1, ships.stderr)



class TestOnlyModules(unittest.TestCase):
    """#2210 review: a module only tests compile ships nothing, declared as
    `#[cfg(test)] mod name;` in its parent, and so does the declaration."""

    def repo(self, tmp, files):
        repo = Repo(Path(tmp))
        for name, text in files.items():
            (repo.path / name).parent.mkdir(parents=True, exist_ok=True)
            (repo.path / name).write_text(text)
        run(repo.path, "git", "add", "-A")
        run(repo.path, "git", "commit", "-q", "-m", "base")
        run(repo.path, "git", "checkout", "-q", "-b", "change")
        return repo

    def change(self, repo, files):
        for name, text in files.items():
            (repo.path / name).parent.mkdir(parents=True, exist_ok=True)
            (repo.path / name).write_text(text)
        run(repo.path, "git", "add", "-A")
        run(repo.path, "git", "commit", "-q", "-m", "change")

    def test_adding_a_test_only_module_and_its_declaration_ships_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(tmp, {"src/main.rs": "mod a;\nfn main() {}\n"})
            self.change(
                repo,
                {
                    "src/main.rs": "mod a;\n#[cfg(test)]\nmod helper;\nfn main() {}\n",
                    "src/helper.rs": "pub fn fixture() -> u8 {\n    1\n}\n",
                },
            )
            self.assertTrue(repo.verdict("src/main.rs"))
            self.assertTrue(repo.verdict("src/helper.rs"))

    def test_a_module_declared_for_the_release_build_ships(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(tmp, {"src/main.rs": "fn main() {}\n"})
            self.change(
                repo,
                {
                    "src/main.rs": "mod helper;\nfn main() {}\n",
                    "src/helper.rs": "pub fn fixture() -> u8 {\n    1\n}\n",
                },
            )
            self.assertFalse(repo.verdict("src/main.rs"))
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_a_module_one_crate_root_ships_ships(self):
        """Declared for tests in main.rs but for real in lib.rs: it ships."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": "#[cfg(test)]\nmod helper;\nfn main() {}\n",
                    "src/lib.rs": "pub mod helper;\n",
                    "src/helper.rs": "pub fn fixture() -> u8 {\n    1\n}\n",
                },
            )
            self.change(repo, {"src/helper.rs": "pub fn fixture() -> u8 {\n    2\n}\n"})
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_a_nested_test_only_module_ships_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": "mod widgets;\nfn main() {}\n",
                    "src/widgets.rs": "pub mod editor;\n#[cfg(test)]\nmod fixtures;\n",
                    "src/widgets/fixtures/mod.rs": "pub fn f() -> u8 {\n    1\n}\n",
                },
            )
            self.change(
                repo, {"src/widgets/fixtures/mod.rs": "pub fn f() -> u8 {\n    2\n}\n"}
            )
            self.assertTrue(repo.verdict("src/widgets/fixtures/mod.rs"))

    def test_a_release_declaration_with_a_trailing_comment_ships(self):
        """#2217 review: `pub mod helper; // shared` declares the module for
        the release build as surely as the bare form."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": "#[cfg(test)]\nmod helper;\nfn main() {}\n",
                    "src/lib.rs": "pub mod helper; // shared\n",
                    "src/helper.rs": "pub fn f() -> u8 {\n    1\n}\n",
                },
            )
            self.change(repo, {"src/helper.rs": "pub fn f() -> u8 {\n    2\n}\n"})
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_a_test_only_declaration_with_a_trailing_comment_ships_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": "#[cfg(test)]\nmod helper; // fixtures\nfn main() {}\n",
                    "src/helper.rs": "pub fn f() -> u8 {\n    1\n}\n",
                },
            )
            self.change(repo, {"src/helper.rs": "pub fn f() -> u8 {\n    2\n}\n"})
            self.assertTrue(repo.verdict("src/helper.rs"))

    def test_a_file_a_path_attribute_can_load_ships(self):
        """#2217 review: `#[path = "helper.rs"] mod production;` loads the
        file into the release build under another name, which a reading by
        name cannot see."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": (
                        "#[cfg(test)]\nmod helper;\n"
                        '#[path = "helper.rs"]\nmod production;\nfn main() {}\n'
                    ),
                    "src/helper.rs": "pub fn f() -> u8 {\n    1\n}\n",
                },
            )
            self.change(repo, {"src/helper.rs": "pub fn f() -> u8 {\n    2\n}\n"})
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_a_path_attribute_split_over_lines_still_ships(self):
        """#2217 review: Rust lets an attribute run over several lines, and
        rustfmt wraps a long `cfg_attr` that way."""
        for attribute in (
            '#[path\n    = "helper.rs"]\nmod production;\n',
            '#[cfg_attr(\n    not(test),\n    path = "helper.rs"\n)]\nmod production;\n',
        ):
            with self.subTest(attribute=attribute), tempfile.TemporaryDirectory() as tmp:
                repo = self.repo(
                    tmp,
                    {
                        "src/main.rs": "#[cfg(test)]\nmod helper;\n" + attribute + "fn main() {}\n",
                        "src/helper.rs": "pub fn f() -> u8 {\n    1\n}\n",
                    },
                )
                self.change(repo, {"src/helper.rs": "pub fn f() -> u8 {\n    2\n}\n"})
                self.assertFalse(repo.verdict("src/helper.rs"))

    def test_a_path_attribute_naming_another_file_leaves_the_exemption(self):
        """croft's own build.rs loads `src/release_notes/select.rs` by
        `#[path]`; that cannot load `src/helper.rs`, and a `format!` argument
        named `path` is no attribute at all."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(
                tmp,
                {
                    "build.rs": '#[path = "src/select.rs"]\nmod select;\nfn main() {}\n',
                    "src/select.rs": "pub fn baked() -> u8 {\n    1\n}\n",
                    "src/main.rs": (
                        "#[cfg(test)]\nmod helper;\n"
                        'fn main() {\n    let p = "x";\n    print!("{path}", path = p);\n}\n'
                    ),
                    "src/helper.rs": "pub fn f() -> u8 {\n    1\n}\n",
                },
            )
            self.change(repo, {"src/helper.rs": "pub fn f() -> u8 {\n    2\n}\n"})
            self.assertTrue(repo.verdict("src/helper.rs"))

    def shipped_by(self, files, path="src/helper.rs"):
        """The verdict on an edit to `path` in a repo of `files`."""
        with tempfile.TemporaryDirectory() as tmp:
            files = {path: "pub fn f() -> u8 {\n    1\n}\n", **files}
            repo = self.repo(tmp, files)
            self.change(repo, {path: "pub fn f() -> u8 {\n    2\n}\n"})
            return repo.verdict(path)

    def test_a_release_declaration_hidden_from_a_line_reading_ships(self):
        """#2217 review: Rust lets a declaration break after `mod`, carry a
        comment between its words, or spell the name as `r#helper`; each
        still declares `helper` for the release build."""
        for release in (
            "pub mod\n    helper;\n",
            "pub mod /* shared */ helper;\n",
            "pub mod r#helper;\n",
        ):
            with self.subTest(release=release):
                self.assertFalse(
                    self.shipped_by(
                        {
                            "src/main.rs": "#[cfg(test)]\nmod helper;\nfn main() {}\n",
                            "src/lib.rs": release,
                        }
                    )
                )

    def test_a_test_only_declaration_over_two_lines_ships_nothing(self):
        self.assertTrue(
            self.shipped_by({"src/main.rs": "#[cfg(test)]\nmod\n    helper;\nfn main() {}\n"})
        )

    def test_an_attribute_in_a_block_comment_does_not_make_a_module_test_only(self):
        """#2217 review: the `#[cfg(test)]` is comment text, so `mod helper;`
        compiles for the release build."""
        self.assertFalse(
            self.shipped_by({"src/main.rs": "/*\n#[cfg(test)]\n*/\nmod helper;\nfn main() {}\n"})
        )

    def test_a_path_attribute_with_an_escape_ships(self):
        """#2217 review: `"hel\\x70er.rs"` is `"helper.rs"` to Rust; a value
        this does not decode counts as naming any file."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        "#[cfg(test)]\nmod helper;\n"
                        '#[path = "hel\\x70er.rs"]\nmod production;\nfn main() {}\n'
                    )
                }
            )
        )

    def test_a_path_attribute_on_an_inline_module_ships_what_it_can_reach(self):
        """#2217 review: `#[path = "widgets"]` on an inline module makes its
        children load from `src/widgets/`, so `production::helper` loads
        `src/widgets/helper.rs` for the release build."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        "mod widgets;\n"
                        '#[path = "widgets"]\nmod production {\n    pub mod helper;\n}\n'
                        "fn main() {}\n"
                    ),
                    "src/widgets.rs": "#[cfg(test)]\npub mod helper;\n",
                },
                path="src/widgets/helper.rs",
            )
        )

    def test_a_file_a_path_attribute_loads_declares_its_neighbours(self):
        """A file loaded by `#[path]` finds its own children beside it, as a
        crate root does: `src/select.rs` declaring `pub mod helper;` loads
        `src/helper.rs` into the build script."""
        self.assertFalse(
            self.shipped_by(
                {
                    "build.rs": '#[path = "src/select.rs"]\nmod select;\nfn main() {}\n',
                    "src/select.rs": "pub mod helper;\n",
                    "src/main.rs": "#[cfg(test)]\nmod helper;\nfn main() {}\n",
                }
            )
        )

    def test_an_inline_module_further_up_ships_the_file(self):
        """`pub mod a { pub mod helper; }` in lib.rs loads `src/a/helper.rs`
        though `src/a.rs` declares it only for tests."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": "mod a;\nfn main() {}\n",
                    "src/a.rs": "#[cfg(test)]\nmod helper;\n",
                    "src/lib.rs": "pub mod a {\n    pub mod helper;\n}\n",
                },
                path="src/a/helper.rs",
            )
        )

    def test_a_file_an_include_macro_reads_ships(self):
        """`include_str!` puts the file's text in the binary, and `include!`
        its code, whatever the module tree says. A name the macro's
        argument does not pin down counts as any name."""
        for reader in (
            'const SRC: &str = include_str!("helper.rs");\n',
            'include!(concat!(env!("OUT_DIR"), "/", env!("NAME")));\n',
        ):
            with self.subTest(reader=reader):
                self.assertFalse(
                    self.shipped_by(
                        {"src/main.rs": "#[cfg(test)]\nmod helper;\n" + reader + "fn main() {}\n"}
                    )
                )

    def test_a_comment_inside_a_loader_still_ships(self):
        """#2217 review: Rust reads a comment between tokens as whitespace,
        so `path /* shared */ =` is a `#[path]` and `include_str /* x */ !`
        an `include_str!`."""
        for loader in (
            '#[path /* shared */ = "helper.rs"]\nmod production;\n',
            'const SRC: &str = include_str /* shared */ !("helper.rs");\n',
        ):
            with self.subTest(loader=loader):
                self.assertFalse(
                    self.shipped_by(
                        {"src/main.rs": "#[cfg(test)]\nmod helper;\n" + loader + "fn main() {}\n"}
                    )
                )

    def test_a_module_file_of_any_name_is_read_for_loaders(self):
        """#2217 review: `#[path]` loads a file whatever its name, so
        `production.inc` is a module whose `include_str!("helper.rs")` puts
        the file in the binary."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        '#[cfg(test)]\nmod helper;\n#[path = "production.inc"]\nmod production;\n'
                        'fn main() {\n    print!("{}", production::SRC);\n}\n'
                    ),
                    "src/production.inc": 'pub const SRC: &str = include_str!("helper.rs");\n',
                }
            )
        )

    def test_a_path_attribute_naming_a_missing_file_ships(self):
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        '#[cfg(test)]\nmod helper;\n#[path = "gone.inc"]\nmod production;\n'
                        "fn main() {}\n"
                    )
                }
            )
        )

    def test_an_edit_inside_the_tests_of_an_embedded_file_ships(self):
        """#2217 review: `include_str!("helper.rs")` puts the whole file in
        the binary, test module and all, so an edit inside that test module
        changes what ships."""
        with tempfile.TemporaryDirectory() as tmp:
            body = "pub fn f() -> u8 {{\n    1\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        assert!({});\n    }}\n}}\n"
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": (
                        "mod helper;\n"
                        'const SRC: &str = include_str!("helper.rs");\nfn main() {}\n'
                    ),
                    "src/helper.rs": body.format("true"),
                },
            )
            self.change(repo, {"src/helper.rs": body.format("false")})
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_an_edit_inside_the_tests_of_a_file_a_generated_module_can_embed_ships(self):
        """#2217 review: a `#[path]` module missing from the tree is
        written by the build, and can `include_str!` any file; an edit in
        that file's test module may then change the binary."""
        with tempfile.TemporaryDirectory() as tmp:
            body = "pub fn f() -> u8 {{\n    1\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        assert!({});\n    }}\n}}\n"
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": (
                        'mod helper;\n#[path = "production.inc"]\nmod production;\nfn main() {}\n'
                    ),
                    "src/helper.rs": body.format("true"),
                },
            )
            self.change(repo, {"src/helper.rs": body.format("false")})
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_an_include_after_a_comment_that_closes_a_test_module_ships(self):
        """#2217 review: the `include_str!` sits outside the test module,
        whose closing brace follows a comment on the same line."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        "mod outer {\n    #[cfg(test)]\n    mod tests {\n    /* comment */ }\n"
                        '    pub const SRC: &str = include_str!("helper.rs");\n}\n'
                        "#[cfg(test)]\nmod helper;\nfn main() {}\n"
                    )
                }
            )
        )

    def test_an_edit_inside_the_tests_of_a_file_an_escaped_path_module_can_embed_ships(self):
        """#2217 review: `"product\\x69on.inc"` is `production.inc` to Rust,
        a module this never reads, which can embed any file."""
        with tempfile.TemporaryDirectory() as tmp:
            body = "pub fn f() -> u8 {{\n    1\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        assert!({});\n    }}\n}}\n"
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": (
                        'mod helper;\n#[path = "product\\x69on.inc"]\nmod production;\nfn main() {}\n'
                    ),
                    "src/production.inc": 'pub const SRC: &str = include_str!("helper.rs");\n',
                    "src/helper.rs": body.format("true"),
                },
            )
            self.change(repo, {"src/helper.rs": body.format("false")})
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_an_edit_inside_the_tests_of_a_file_a_nested_path_module_can_embed_ships(self):
        """#2217 review: `#[path]` on a module inside `mod outer { .. }` loads
        `src/outer/production.inc`, which this does not read, and which
        embeds `helper.rs`."""
        with tempfile.TemporaryDirectory() as tmp:
            body = "pub fn f() -> u8 {{\n    1\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        assert!({});\n    }}\n}}\n"
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": (
                        'mod helper;\nmod outer {\n    #[path = "production.inc"]\n    pub mod production;\n}\n'
                        "fn main() {}\n"
                    ),
                    "src/outer/production.inc": 'pub const SRC: &str = include_str!("../helper.rs");\n',
                    "src/helper.rs": body.format("true"),
                },
            )
            self.change(repo, {"src/helper.rs": body.format("false")})
            self.assertFalse(repo.verdict("src/helper.rs"))

    def test_an_include_inside_a_test_module_does_not_count(self):
        """croft's port detector has a test that reads its own source with
        `include_str!`; inside `#[cfg(test)]` that compiles only for tests,
        so the file's test-only edits still ship nothing."""
        with tempfile.TemporaryDirectory() as tmp:
            body = (
                "pub fn f() -> u8 {{\n    1\n}}\n\n#[cfg(test)]\nmod tests {{\n"
                '    #[test]\n    fn t() {{\n        let src = include_str!("helper.rs");\n'
                "        assert!({});\n    }}\n}}\n"
            )
            repo = self.repo(
                tmp,
                {"src/main.rs": "mod helper;\nfn main() {}\n", "src/helper.rs": body.format("true")},
            )
            self.change(repo, {"src/helper.rs": body.format("!src.is_empty()")})
            self.assertTrue(repo.verdict("src/helper.rs"))

    def test_an_include_on_a_test_modules_closing_line_ships(self):
        """#2217 review: the `include_str!` follows the brace that closes the
        test module on the same line, so it is outside the module and
        compiles into the release build."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        "#[cfg(test)]\nmod helper;\n#[cfg(test)]\nmod tests {\n    fn t() {}\n"
                        '} pub const SRC: &str = include_str!("helper.rs");\n'
                        'fn main() {\n    print!("{SRC}");\n}\n'
                    )
                }
            )
        )

    def test_an_include_before_a_test_modules_attribute_on_one_line_ships(self):
        """The mirror case: the `include_str!` comes before `#[cfg(test)]`
        on the line the test module starts on."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        "#[cfg(test)]\nmod helper;\n"
                        'pub const SRC: &str = include_str!("helper.rs"); #[cfg(test)] mod tests {\n'
                        '    fn t() {}\n}\nfn main() {\n    print!("{SRC}");\n}\n'
                    )
                }
            )
        )

    def test_test_module_spans_run_from_the_attribute_to_the_closing_brace(self):
        toks = filt.rust_tokens(
            'a!(); #[cfg(test)] #[allow(x)] pub(crate) mod t { b!(); } c!();\n'
            "#[cfg(test)]\nmod d;\n#[cfg(not(test))]\nmod e {}\n"
        )
        spans = filt._test_module_spans(toks)
        self.assertEqual(len(spans), 1, "only the module with a body and exact cfg(test)")
        first, last = spans[0]
        self.assertEqual(toks[first], ("punct", "#"))
        self.assertEqual(toks[last], ("punct", "}"))
        inside = [w for k, w in toks[first : last + 1] if k == "ident"]
        self.assertIn("b", inside)
        self.assertNotIn("a", inside)
        self.assertNotIn("c", inside)

    def test_code_pasted_in_by_include_ships(self):
        """#2217 review: `include!` pastes a file in as code, and that code
        can declare modules or read files itself (here `production.inc`
        reads `helper.rs`); nothing follows it, so it counts as unknown."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        '#[cfg(test)]\nmod helper;\ninclude!("production.inc");\n'
                        'fn main() {\n    print!("{SRC}");\n}\n'
                    ),
                    "src/production.inc": 'const SRC: &str = include_str!("helper.rs");\n',
                }
            )
        )

    def test_a_renamed_include_macro_ships(self):
        """After `use std::include_str as text;`, `text!("helper.rs")` reads
        the file under a name this does not know."""
        self.assertFalse(
            self.shipped_by(
                {
                    "src/main.rs": (
                        "#[cfg(test)]\nmod helper;\nuse std::include_str as text;\n"
                        'const SRC: &str = text!("helper.rs");\nfn main() {}\n'
                    )
                }
            )
        )

    def test_an_include_of_another_file_leaves_the_exemption(self):
        """croft's own shape: `concat!(env!("OUT_DIR"), "/notes.md")` reads a
        `notes.md`, which is not `src/helper.rs`."""
        self.assertTrue(
            self.shipped_by(
                {
                    "src/main.rs": (
                        "#[cfg(test)]\nmod helper;\n"
                        'const N: &str = include_str!(concat!(env!("OUT_DIR"), "/notes.md"));\n'
                        "fn main() {}\n"
                    )
                }
            )
        )

    def test_a_crate_root_cargo_names_declares_its_neighbours(self):
        """A `[lib] path` in Cargo.toml is a crate root like `src/lib.rs`."""
        self.assertFalse(
            self.shipped_by(
                {
                    "Cargo.toml": '[package]\nname = "x"\n\n[lib]\npath = "src/core.rs"\n',
                    "src/core.rs": "pub mod helper;\n",
                    "src/main.rs": "#[cfg(test)]\nmod helper;\nfn main() {}\n",
                }
            )
        )

    def test_a_binary_in_src_bin_declares_its_neighbours(self):
        self.assertFalse(
            self.shipped_by(
                {
                    "src/bin/tool.rs": "pub mod helper;\nfn main() {}\n",
                    "src/bin/check.rs": "#[cfg(test)]\nmod helper;\nfn main() {}\n",
                },
                path="src/bin/helper.rs",
            )
        )

    def test_making_a_shipped_module_test_only_ships(self):
        """Adding `#[cfg(test)]` above a declaration takes a module out of
        the release build: the binary changes."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = self.repo(
                tmp,
                {
                    "src/main.rs": "mod helper;\nfn main() {}\n",
                    "src/helper.rs": "pub fn f() -> u8 {\n    1\n}\n",
                },
            )
            self.change(repo, {"src/main.rs": "#[cfg(test)]\nmod helper;\nfn main() {}\n"})
            self.assertFalse(repo.verdict("src/main.rs"))

if __name__ == "__main__":
    unittest.main()
