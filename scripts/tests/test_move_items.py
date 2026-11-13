"""Tests for scripts/move_items.py: the tool that writes each stage-B move.

The tool's output is what scripts/check_pure_move.py then has to certify, so
the main cases run both on a real git repository: a move the tool writes must
pass the checker as it stands, with nothing fixed up by hand.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import check_pure_move as pm  # noqa: E402
import move_items as mi  # noqa: E402

MOD = """//! The app.

mod click;
mod hover;
use std::fmt;

pub struct App {
    n: u8,
}

/// The counter.
impl App {
    /// Bumps the counter.
    #[inline]
    fn bump(&mut self) -> u8 {
        self.n + 1
    }

    pub(super) fn shown(&self) -> u8 {
        self.n
    }
}

impl App {
    fn keep(&self) {}

    #[cfg(test)] fn probe(&self) {}
}

impl fmt::Display for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.n)
    }
}

// Adds one.
#[must_use]
fn inc(x: u8) -> u8 {
    x + 1
}

const LIMIT: u8 = 3;
"""

ARCH = """# Architecture

```
src/
├── app/
│   ├── mod.rs           The App struct.
│   ├── click.rs         Clicks.
│   ├── hover.rs         Hovers.
│   ├── tests/           Tests.
```
"""

# Where each item of MOD goes: name -> (pr, file). Everything else stays.
ONE_PR = {
    "bump": (1, "code_scanning"),
    "shown": (1, "code_scanning"),
    "probe": (1, "code_scanning"),
    "fmt": (1, "code_scanning"),
    "inc": (1, "code_scanning"),
    "LIMIT": (1, "code_scanning"),
}


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True)


def quiet(fn, *args):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        return fn(*args), out.getvalue() + err.getvalue()


def manifest(text, where):
    """The manifest for `text` that sends each item named in `where` to its
    (pr, file) and keeps the rest."""
    items, _ = pm.parse(text, mi.MOD)
    rows = ["# test manifest", "pr\tfile\tkind\timpl\tname"]
    for it in items:
        kind, impl, name = mi.item_key(it)
        pr, file = where.get(it.name, (0, ""))
        rows.append(f"{pr}\t{file}\t{kind}\t{impl}\t{name}")
    return "\n".join(rows) + "\n"


class Root:
    """A throwaway repository laid out like croft's, on branch `main`."""

    def __init__(self, path: Path, mod: str, where: dict):
        self.path = path
        for rel, text in {
            mi.MOD: mod,
            mi.ARCH: ARCH,
            mi.MANIFEST: manifest(mod, where),
            os.path.join(mi.NOTES, ".keep"): "",
        }.items():
            (path / rel).parent.mkdir(parents=True, exist_ok=True)
            (path / rel).write_text(text)
        git(path, "init", "-q", "-b", "main")
        git(path, "config", "user.email", "t@example.com")
        git(path, "config", "user.name", "t")
        self.commit()
        git(path, "checkout", "-q", "-b", "work")

    def commit(self):
        git(self.path, "add", "-A")
        git(self.path, "commit", "-q", "--allow-empty", "-m", "c")

    def move(self, pr):
        return quiet(mi.main, ["--pr", str(pr), "--root", str(self.path), "--no-fmt", "--no-build"])

    def read(self, rel):
        return (self.path / rel).read_text()

    def pure(self, base="main"):
        here = os.getcwd()
        os.chdir(self.path)
        try:
            return quiet(pm.main, [base, "HEAD"])
        finally:
            os.chdir(here)


class Moves(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)

    def test_a_move_the_tool_writes_passes_the_pure_move_check(self):
        root = Root(self.tmp, MOD, ONE_PR)
        code, out = root.move(1)
        self.assertEqual(code, 0, out)
        self.assertIn("moved 6 item(s) from src/app/mod.rs to src/app/code_scanning.rs, 4 widened", out)
        root.commit()
        code, out = root.pure()
        self.assertEqual(code, 0, out)
        self.assertIn("Pure move: every changed item moved byte for byte.", out)

    def test_the_new_file_has_its_doc_the_parent_scope_and_impl_headers(self):
        root = Root(self.tmp, MOD, ONE_PR)
        root.move(1)
        text = root.read("src/app/code_scanning.rs")
        self.assertTrue(text.startswith("//! SARIF results viewer"), text)
        self.assertIn("\n\nuse super::*;\n\n/// The counter.\nimpl App {\n    /// Bumps the counter.\n", text)
        self.assertEqual(text.count("impl App {"), 2, "methods of two blocks stay in two blocks")
        self.assertIn("impl fmt::Display for App {", text)
        self.assertIn("\n\n/// The counter.\nimpl App {\n", text, "an emptied block's docs go with it")

    def test_private_items_widen_to_the_parent_and_nothing_else_does(self):
        root = Root(self.tmp, MOD, ONE_PR)
        root.move(1)
        text = root.read("src/app/code_scanning.rs")
        self.assertIn("    #[inline]\n    pub(super) fn bump(&mut self)", text)
        self.assertIn("    #[cfg(test)] pub(super) fn probe(&self) {}", text)
        self.assertIn("// Adds one.\n#[must_use]\npub(super) fn inc(x: u8)", text)
        self.assertIn("pub(super) const LIMIT: u8 = 3;", text)
        self.assertIn("    pub(super) fn shown(&self)", text)
        self.assertNotIn("pub(super) pub(super)", text)
        self.assertIn("    fn fmt(&self, f: &mut fmt::Formatter<'_>)", text, "a trait method takes no visibility")

    def test_mod_rs_loses_emptied_blocks_and_gains_the_module_glue(self):
        root = Root(self.tmp, MOD, ONE_PR)
        root.move(1)
        text = root.read(mi.MOD)
        self.assertIn("mod hover;\nmod code_scanning;\nuse std::fmt;\nuse code_scanning::*;\n", text)
        self.assertNotIn("fn bump", text)
        self.assertNotIn("/// The counter.", text, "a block with nothing left goes, docs and all")
        self.assertNotIn("impl fmt::Display", text)
        self.assertIn("impl App {\n    fn keep(&self) {}\n}\n", text)
        self.assertNotIn("\n\n\n", text)

    def test_the_release_note_and_architecture_row_are_written(self):
        root = Root(self.tmp, MOD, ONE_PR)
        root.move(1)
        note = root.read(os.path.join(mi.NOTES, "2062-split-code-scanning.md"))
        self.assertTrue(note.startswith("refactor: Groundwork for the refactor program (#2062): the "), note)
        self.assertIn("src/app/code_scanning.rs. Nothing changes in use.", note)
        arch = root.read(mi.ARCH)
        self.assertRegex(arch, r"│   ├── mod\.rs .*\n│   ├── click\.rs .*\n│   ├── code_scanning\.rs ")

    def test_a_file_in_two_prs_is_written_then_appended_to(self):
        where = {"bump": (2, "codeql"), "inc": (3, "codeql")}
        root = Root(self.tmp, MOD, where)
        code, out = root.move(2)
        self.assertEqual(code, 0, out)
        mod = root.read(mi.MOD)
        self.assertIn("mod codeql;\n", mod)
        self.assertNotIn("use codeql::*;", mod, "only methods so far: nothing to import")
        root.commit()
        first = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.tmp, capture_output=True,
                               text=True, check=True).stdout.strip()
        code, out = root.move(3)
        self.assertEqual(code, 0, out)
        root.commit()
        text = root.read("src/app/codeql.rs")
        self.assertEqual(text.count("//! "), len(mi.FILES["codeql"][0].split("\n")), "one doc, at the top")
        self.assertLess(text.index("fn bump"), text.index("fn inc"))
        self.assertIn("mod codeql;\nuse std::fmt;\nuse codeql::*;\n", root.read(mi.MOD))
        for half, part in ((1, "the first part of the "), (2, "the rest of the ")):
            note = root.read(os.path.join(mi.NOTES, f"2062-split-codeql-{half}.md"))
            self.assertIn(part, note)
        self.assertEqual(root.read(mi.ARCH).count("codeql.rs"), 1)
        code, out = root.pure(first)
        self.assertEqual(code, 0, out)

    def test_attributes_over_several_lines_and_comments_between_them_are_passed(self):
        spread = (MOD + "\n#[cfg(any(\n    unix,\n    windows\n))]\n// Why it is allowed.\n"
                  "#[allow(dead_code)] #[inline]\nfn spread() {}\n")
        root = Root(self.tmp, spread, {"spread": (1, "code_scanning")})
        code, out = root.move(1)
        self.assertEqual(code, 0, out)
        text = root.read("src/app/code_scanning.rs")
        self.assertIn("// Why it is allowed.\n#[allow(dead_code)] #[inline]\npub(super) fn spread() {}", text)
        root.commit()
        code, out = root.pure()
        self.assertEqual(code, 0, out)

    def test_blank_lines_inside_a_string_that_stays_are_left_alone(self):
        kept = "const KEPT: &str = \"a\n\n\n}\";\n"
        text = MOD.replace("const LIMIT", kept + "\nconst LIMIT")
        root = Root(self.tmp, text, {"LIMIT": (1, "code_scanning"), "inc": (1, "code_scanning")})
        code, out = root.move(1)
        self.assertEqual(code, 0, out)
        self.assertIn(kept, root.read(mi.MOD))
        root.commit()
        code, out = root.pure()
        self.assertEqual(code, 0, out)


class Refusals(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)

    def test_an_item_the_manifest_does_not_place_stops_the_move(self):
        root = Root(self.tmp, MOD, ONE_PR)
        (self.tmp / mi.MOD).write_text(MOD + "\nfn added_since() {}\n")
        code, out = root.move(1)
        self.assertEqual(code, 1, out)
        self.assertIn("not in the manifest: fn added_since in -", out)
        self.assertEqual(root.read(mi.MOD), MOD + "\nfn added_since() {}\n", "nothing written")

    def test_a_row_with_no_item_stops_the_move(self):
        root = Root(self.tmp, MOD, ONE_PR)
        (self.tmp / mi.MOD).write_text(MOD.replace("const LIMIT: u8 = 3;\n", ""))
        code, out = root.move(1)
        self.assertEqual(code, 1, out)
        self.assertIn("not in mod.rs: const LIMIT in -", out)

    def test_twins_sent_to_different_places_stop_the_move(self):
        twins = MOD + "\n#[cfg(unix)]\nfn os() {}\n\n#[cfg(not(unix))]\nfn os() {}\n"
        root = Root(self.tmp, twins, {"os": (1, "code_scanning")})
        rows = root.read(mi.MANIFEST).split("\n")
        last = max(n for n, r in enumerate(rows) if r.endswith("\tos"))
        rows[last] = rows[last].replace("1\tcode_scanning", "0\t")
        (self.tmp / mi.MANIFEST).write_text("\n".join(rows))
        code, out = root.move(1)
        self.assertEqual(code, 1, out)
        self.assertIn("twins of fn os in - go to different places", out)

    def test_an_item_in_an_inline_module_is_not_moved(self):
        nested = MOD + "\nmod inner {\n    fn deep() {}\n}\n"
        root = Root(self.tmp, nested, {"deep": (1, "code_scanning")})
        code, out = root.move(1)
        self.assertEqual(code, 1, out)
        self.assertIn("deep sits in an inline module (inner)", out)

    def test_an_item_both_in_mod_rs_and_its_file_stops_the_move(self):
        root = Root(self.tmp, MOD, ONE_PR)
        (self.tmp / "src/app/code_scanning.rs").write_text("pub(super) const LIMIT: u8 = 3;\n")
        code, out = root.move(1)
        self.assertEqual(code, 1, out)
        self.assertIn("both in mod.rs and in code_scanning.rs: const LIMIT in -", out)

    def test_a_pr_with_no_rows_is_refused(self):
        root = Root(self.tmp, MOD, ONE_PR)
        code, out = root.move(7)
        self.assertEqual(code, 1, out)
        self.assertIn("no item in the manifest goes in PR 7", out)

    def test_a_pr_already_moved_is_refused(self):
        root = Root(self.tmp, MOD, ONE_PR)
        root.move(1)
        code, out = root.move(1)
        self.assertEqual(code, 1, out)
        self.assertIn("not in mod.rs", out)


class Helpers(unittest.TestCase):
    GLUE = ["mod commands;", "mod remote_sessions;", "mod settings;", "use commands::*;",
            "use remote_sessions::*;", "use settings::*;", "", "pub struct App;"]

    def test_an_item_used_outside_app_is_re_exported_at_its_own_visibility(self):
        errors = [("E0603", "function import `takeover_mode_seq` is private"),
                  ("E0603", "enum import `SideBarPosition` is private")]
        homes = {"takeover_mode_seq": ("remote_sessions", "pub(crate)"),
                 "SideBarPosition": ("settings", "pub")}
        lines, done = mi.fix_glue(self.GLUE, errors, errors, homes)
        self.assertEqual(lines[4:8], ["use remote_sessions::*;",
                                      "pub(crate) use remote_sessions::takeover_mode_seq;",
                                      "use settings::*;", "pub use settings::SideBarPosition;"])
        self.assertEqual(len(done), 2)

    def test_a_glob_only_the_tests_need_is_kept_for_the_tests_and_an_unneeded_one_goes(self):
        app = [("unused_imports", "unused import: `commands::*`"),
               ("unused_imports", "unused import: `settings::*`")]
        tests = [("unused_imports", "unused import: `settings::*`")]
        lines, done = mi.fix_glue(self.GLUE, app, tests, {})
        self.assertEqual(lines[3:6], ["#[cfg(test)]", "use commands::*;", "use remote_sessions::*;"])
        self.assertNotIn("use settings::*;", lines)
        self.assertIn("dropped use settings::*: nothing needs it", done)

    def test_a_guarded_glob_that_is_still_unused_goes_with_its_guard(self):
        guarded = ["mod commands;", "#[cfg(test)]", "use commands::*;", "pub struct App;"]
        lines, _ = mi.fix_glue(guarded, [], [("unused_imports", "unused import: `commands::*`")], {})
        self.assertEqual(lines, ["mod commands;", "pub struct App;"])

    def test_any_other_build_error_stops_the_tool(self):
        cases = [
            [("E0425", "cannot find value `x` in this scope")],
            [("unused_imports", "unused import: `std::fmt`")],
            [("E0603", "function import `not_moved` is private")],
        ]
        for errors in cases:
            with self.subTest(errors=errors), self.assertRaises(mi.BuildError):
                mi.fix_glue(self.GLUE, errors, [], {})

    def test_an_item_too_narrow_for_its_outside_users_stops_the_tool(self):
        errors = [("E0603", "function import `seq` is private")]
        with self.assertRaises(mi.BuildError) as e:
            mi.fix_glue(self.GLUE, errors, [], {"seq": ("commands", "pub(super)")})
        self.assertIn("seq is used outside app but is pub(super) in commands.rs", str(e.exception))

    @staticmethod
    def message(level, text, code=None):
        body = {"level": level, "message": text, "code": {"code": code} if code else None}
        return json.dumps({"reason": "compiler-message", "message": body})

    def test_build_errors_are_read_with_or_without_a_code(self):
        out = "\n".join([
            self.message("error", "unused import: `commands::*`", "unused_imports"),
            self.message("error", "something with no code"),
            self.message("error", "aborting due to 2 previous errors"),
            self.message("warning", "a warning that is not denied", "dead_code"),
            "not json",
        ])
        errors = mi.read_errors("--bins", 101, out, "")
        self.assertEqual(errors, [("", "something with no code"),
                                  ("unused_imports", "unused import: `commands::*`")])
        with self.assertRaises(mi.BuildError):
            mi.fix_glue(self.GLUE, errors, [], {})

    def test_a_failed_build_with_no_compiler_error_is_not_clean(self):
        with self.assertRaises(mi.BuildError) as e:
            mi.read_errors("--tests", 101, "", "error: failed to run custom build command for `x`\n")
        self.assertIn("failed to run custom build command", str(e.exception))
        self.assertEqual(mi.read_errors("--tests", 0, "", ""), [])

    def test_halves_numbers_only_files_that_take_two_prs(self):
        rows = [(0, "", "fn", "-", "a"), (1, "x", "fn", "-", "b"), (2, "y", "fn", "-", "c"),
                (3, "y", "fn", "-", "d")]
        self.assertEqual(mi.halves(rows), {1: ("x", 0), 2: ("y", 1), 3: ("y", 2)})

    def test_the_architecture_row_goes_in_name_order_once(self):
        once = mi.add_arch_row(ARCH, "diff_merge")
        self.assertRegex(once, r"click\.rs .*\n│   ├── diff_merge\.rs +\S.*\n│   ├── hover\.rs")
        self.assertEqual(mi.add_arch_row(once, "diff_merge"), once)
        last = mi.add_arch_row(ARCH, "workbench")
        self.assertRegex(last, r"hover\.rs .*\n│   ├── workbench\.rs +\S.*\n│   ├── tests/")

    def test_every_file_in_the_manifest_has_its_doc_note_and_row(self):
        root = Path(__file__).resolve().parents[2]
        rows = mi.read_manifest(root / mi.MANIFEST)
        files = {file for pr, file, *_ in rows if pr}
        self.assertEqual(files, set(mi.FILES))
        for file, (doc, phrase, row) in mi.FILES.items():
            with self.subTest(file=file):
                self.assertTrue(doc and phrase and row)
                self.assertTrue(all(len("//! " + ln) <= 100 for ln in doc.split("\n")))


if __name__ == "__main__":
    unittest.main()
