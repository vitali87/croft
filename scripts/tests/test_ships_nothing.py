"""Tests for scripts/ships_nothing.py: the release gate's test-only filter.

The `version bump + release notes` job says in its own comment that
"test-only changes ship nothing and are exempt", and then encodes test-only
as a path list. croft keeps most unit tests in a `#[cfg(test)] mod tests`
beside the code they cover, so the ordinary shape of a test-only fix here is
a diff inside a shipped file (#461).

The direction of failure matters more than the coverage: waiving a bump that
was needed puts two different binaries on one version, while asking for a
bump that was not needed only costs a version number. Anything the parse is
unsure about therefore counts as shipped, and the tests below pin that as
much as they pin the exemption.
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

    def test_a_module_declaration_without_a_body_is_not_a_range(self):
        """`mod tests;` puts the body in another file and opens no braces, so
        the span would run to the end of THIS file and cover shipped code."""
        text = "#[cfg(test)]\nmod tests;\npub fn shipped() -> u8 {\n    1\n}\n"
        self.assertEqual(filt.cfg_test_ranges(text), [])

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


if __name__ == "__main__":
    unittest.main()
