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
        self.assertIn(
            "moved 6 item(s) from src/app/mod.rs to src/app/code_scanning.rs, "
            "4 private -> pub(super), 1 pub(super) -> pub(crate)",
            out,
        )
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

    def test_each_moved_item_keeps_exactly_its_reach(self):
        """A private item of mod.rs reached all of `app`, which is `pub(super)`
        from a file under src/app/; a `pub(super)` one reached the whole crate,
        which from there is `pub(crate)`: kept as `pub(super)` it would reach
        only `app`, narrower than before."""
        root = Root(self.tmp, MOD, ONE_PR)
        root.move(1)
        text = root.read("src/app/code_scanning.rs")
        self.assertIn("    #[inline]\n    pub(super) fn bump(&mut self)", text)
        self.assertIn("    #[cfg(test)] pub(super) fn probe(&self) {}", text)
        self.assertIn("// Adds one.\n#[must_use]\npub(super) fn inc(x: u8)", text)
        self.assertIn("pub(super) const LIMIT: u8 = 3;", text)
        self.assertIn("    pub(crate) fn shown(&self)", text)
        self.assertNotIn("pub(super) pub(super)", text)
        self.assertNotIn("pub(crate) pub(super)", text)
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

    def test_appending_to_a_file_whose_glob_was_dropped_puts_it_back(self):
        where = {"bump": (2, "codeql"), "inc": (3, "codeql")}
        root = Root(self.tmp, MOD, where)
        code, out = root.move(2)
        self.assertEqual(code, 0, out)
        path = self.tmp / "src/app/codeql.rs"
        text = path.read_text()
        self.assertIn("\nuse super::*;\n\n", text)
        path.write_text(text.replace("\nuse super::*;\n\n", "\n", 1))
        root.commit()
        code, out = root.move(3)
        self.assertEqual(code, 0, out)
        text = root.read("src/app/codeql.rs")
        self.assertEqual(text.count("use super::*;"), 1)
        self.assertLess(text.index("use super::*;"), text.index("fn bump"))

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

    def test_a_visibility_is_respelled_to_reach_what_it_reached(self):
        """Moving from src/app/codeql.rs into src/app/codeql/: a private item
        reached all of `codeql`, now `pub(super)`; a `pub(super)` one reached
        all of `app`, now `pub(in crate::app)`; wider ones stay. Without the
        modules, only a private item changes, to `pub(super)`."""
        src, dst = ("crate", "app", "codeql"), ("crate", "app", "codeql", "run")
        for vis, want in [("", "pub(super)"), ("pub(super)", "pub(incrate::app)"),
                          ("pub(crate)", "pub(crate)"), ("pub", "pub")]:
            self.assertEqual(mi.respell(vis, src, dst), want, vis)
        self.assertEqual(mi.respell(""), "pub(super)")
        self.assertEqual(mi.respell("pub(super)"), "pub(super)")

    def test_a_moved_pub_super_method_names_the_module_it_reached(self):
        text = "impl App {\n    /// Runs.\n    pub(super) fn run(&self) {}\n\n    fn helper(&self) {}\n}\n"
        code, starts_in, _ = pm.lex(text)
        lines, clines = text.split("\n"), code.split("\n")
        kinds = [pm.line_kind(ln, c, starts_in.get(i)) for i, (ln, c) in enumerate(zip(lines, clines))]
        run, helper = pm.parse(text, "src/app/codeql.rs")[0]
        src, dst = ("crate", "app", "codeql"), ("crate", "app", "codeql", "run")
        self.assertEqual(mi.widen(lines, run, kinds, clines, "impl App", src, dst),
                         ["    /// Runs.", "    pub(in crate::app) fn run(&self) {}"])
        self.assertEqual(mi.widen(lines, helper, kinds, clines, "impl App", src, dst),
                         ["    pub(super) fn helper(&self) {}"])

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
        self.assertTrue(all(d.endswith("used outside app") for d in done), done)
        _, done = mi.fix_glue(self.GLUE, errors, errors, homes, "editor")
        self.assertTrue(all(d.endswith("used outside editor") for d in done), done)

    def test_a_new_mod_joins_the_top_block_not_the_tests_at_the_bottom(self):
        """A split file shrinks until its `mod tests;`, below its last struct,
        is near the top of the file. A later file's `mod` (and its glob) still
        go with the other files' at the top: pair.rs's `turns` once landed
        after `mod tests;`, where rustfmt kept it."""
        lines = ["//! The pilot.", "", "mod apply;", "mod fence;", "use apply::*;",
                 "use fence::*;", "", "use std::sync::Mutex;", "",
                 "pub(crate) struct PairState {", "    n: u8,", "}", "",
                 "#[cfg(test)]", "mod tests;", ""]
        out = mi.add_glue(list(lines), "turns", True)
        self.assertEqual(out[2:5], ["mod apply;", "mod fence;", "mod turns;"])
        self.assertLess(out.index("use turns::*;"), out.index("pub(crate) struct PairState {"))
        self.assertEqual(out[-3:], ["#[cfg(test)]", "mod tests;", ""])

    def test_a_test_only_item_is_re_exported_for_the_tests_alone(self):
        """An item that only exists under `#[cfg(test)]` (pair.rs's
        `inject_note`, which the App's navigator tests call by path) gets a
        re-export guarded the same way: an unguarded one names, in the app
        build, an item that is not there (E0432)."""
        errors = [("E0603", "function import `inject_note` is private")]
        homes = {"inject_note": ("settings", "pub(crate)", ["#[cfg(test)]"])}
        lines, done = mi.fix_glue(self.GLUE, [], errors, homes)
        self.assertEqual(lines[5:8], ["use settings::*;", "#[cfg(test)]",
                                      "pub(crate) use settings::inject_note;"])
        self.assertEqual(done, ["re-exported settings::inject_note (pub(crate)) only under "
                                "cfg(test): used outside app"])

    def test_an_items_cfg_lines_are_read_from_above_its_declaration(self):
        text = ("/// Test-only: anchors a seed note.\n#[cfg(test)]\n#[allow(dead_code)]\n"
                "pub(crate) fn inject_note() {\n    #[cfg(unix)]\n    let x = 1;\n}")
        self.assertEqual(mi.item_cfgs(text), ["#[cfg(test)]"])
        self.assertEqual(mi.item_cfgs("pub fn plain() {}"), [])

    def test_a_glob_only_the_tests_need_is_kept_for_the_tests_and_an_unneeded_one_goes(self):
        app = [("unused_imports", "unused import: `commands::*`"),
               ("unused_imports", "unused import: `settings::*`")]
        tests = [("unused_imports", "unused import: `settings::*`")]
        lines, done = mi.fix_glue(self.GLUE, app, tests, {})
        self.assertEqual(lines[3:6], ["#[cfg(test)]", "use commands::*;", "use remote_sessions::*;"])
        self.assertNotIn("use settings::*;", lines)
        self.assertIn("dropped use settings::*: nothing needs it", done)

    def test_a_re_export_only_the_tests_need_is_kept_for_the_tests(self):
        """An item a test outside the module names by path (an editor helper
        a line_edit test calls) is re-exported, and the app build then finds
        the re-export unused: it gets `#[cfg(test)]`, as a glob would."""
        glue = ["mod navigation;", "pub(crate) use navigation::word_char_at;",
                "use navigation::*;", "pub struct Editor;"]
        app = [("unused_imports", "unused import: `navigation::word_char_at`")]
        lines, done = mi.fix_glue(glue, app, [], {})
        self.assertEqual(lines[1:3], ["#[cfg(test)]", "pub(crate) use navigation::word_char_at;"])
        self.assertEqual(done, ["re-export of navigation::word_char_at only under cfg(test)"])
        lines, done = mi.fix_glue(glue, app, app, {})
        self.assertNotIn("pub(crate) use navigation::word_char_at;", lines)
        self.assertEqual(done, ["dropped the re-export of navigation::word_char_at: nothing needs it"])

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

    def test_an_error_that_follows_from_a_private_import_waits_for_the_re_export(self):
        """A struct reached through a private glob import is not resolved, so
        rustc also reports what it then could not infer about the struct's
        fields (E0277 on a `let Some(x) = status.branch.clone()`). The
        re-export goes in rather than the tool stopping; the next build says
        whether the other error was only a consequence."""
        errors = [("E0603", "struct import `GitStatus` is private"),
                  ("E0277", "the size for values of type `str` cannot be known at compilation time")]
        homes = {"GitStatus": ("settings", "pub")}
        lines, done = mi.fix_glue(self.GLUE, errors, errors, homes, "git")
        self.assertIn("pub use settings::GitStatus;", lines)
        self.assertEqual(done, ["re-exported settings::GitStatus (pub): used outside git"])

    def test_a_new_file_that_needs_nothing_from_its_parent_loses_the_glob(self):
        """A moved file of plain text handling (git.rs's one-line summaries
        of git's output) uses nothing from git.rs, and `use super::*;` is
        then an unused import that fails the build: it goes, with the blank
        line after it."""
        lines = ["//! What git said, in one line.", "", "use super::*;", "",
                 "pub fn headline(output: &str) -> &str {", "    output", "}", ""]
        self.assertEqual(mi.without_super_glob(lines),
                         ["//! What git said, in one line.", "",
                          "pub fn headline(output: &str) -> &str {", "    output", "}", ""])

    def test_a_later_move_puts_the_parent_glob_back_before_appending(self):
        """An earlier move may have dropped `use super::*;` from a file that
        needed nothing from its parent; items a later move appends may need
        it. The glob goes back after the `//!` doc (the build step drops it
        again if it is still unused), and a file that has it is untouched."""
        bare = ["//! What git said, in one line.", "",
                "pub fn headline(output: &str) -> &str {", "    output", "}", ""]
        full = ["//! What git said, in one line.", "", "use super::*;", "",
                "pub fn headline(output: &str) -> &str {", "    output", "}", ""]
        self.assertEqual(mi.with_super_glob(bare), full)
        self.assertEqual(mi.with_super_glob(full), full)
        self.assertEqual(mi.with_super_glob(mi.without_super_glob(full)), full)

    def test_an_item_too_narrow_for_its_outside_users_stops_the_tool(self):
        errors = [("E0603", "function import `seq` is private")]
        with self.assertRaises(mi.BuildError) as e:
            mi.fix_glue(self.GLUE, errors, [], {"seq": ("commands", "pub(super)")})
        self.assertIn("seq is used outside app but is pub(super) in commands.rs", str(e.exception))

    NESTED = ("src/app/debugger.rs", "src/app/debugger")
    DEBUGGER_GLUE = ["//! Run and Debug.", "", "use super::*;", "", "mod adapters;",
                     "use adapters::*;", "", "pub(super) const GRACE: u64 = 250;", ""]

    def test_a_name_a_nested_split_hides_is_re_exported_at_the_same_reach(self):
        """src/app/mod.rs has `use debugger::*;`, so the app's tests called
        debugger.rs's `pub(super)` free functions by name. Moved a level down
        into debugger/adapters.rs, they are found only through debugger.rs's
        own `use adapters::*;`, which is private to debugger, so the tests
        no longer find them (E0425), bare or as `super::name`. debugger.rs
        re-exports each by name,
        spelled to reach exactly what the item reaches: `pub(super)` there is
        the `pub(in crate::app)` the move gave it, and a `pub(crate)` item
        stays `pub(crate)`."""
        tests = [("E0425", "cannot find function `lldb_dap_rank` in this scope"),
                 ("E0412", "cannot find type `Probe` in this scope"),
                 ("E0433", "failed to resolve: use of undeclared type `Probe`"),
                 # A test in app/tests/debug.rs calling `super::debuggee_python`.
                 ("E0425", "cannot find function `debuggee_python` in module `super`")]
        homes = {"lldb_dap_rank": ("adapters", "pub(in crate::app)"),
                 "Probe": ("adapters", "pub(crate)"),
                 "debuggee_python": ("adapters", "pub(in crate::app)")}
        lines, done = mi.fix_glue(self.DEBUGGER_GLUE, [], tests, homes, "debugger", self.NESTED)
        self.assertEqual(lines[5:9], ["use adapters::*;", "pub(super) use adapters::lldb_dap_rank;",
                                      "pub(super) use adapters::debuggee_python;",
                                      "pub(crate) use adapters::Probe;"])
        self.assertEqual(done, [
            "re-exported adapters::Probe (pub(crate)): used outside debugger",
            "re-exported adapters::debuggee_python (pub(super)): used outside debugger",
            "re-exported adapters::lldb_dap_rank (pub(super)): used outside debugger",
        ])

    def test_a_hidden_name_that_reaches_only_its_split_file_stops_the_tool(self):
        """`pub(super)` in debugger/adapters.rs reaches only debugger: a
        re-export cannot give users outside it more than the item has."""
        tests = [("E0425", "cannot find function `probe` in this scope")]
        with self.assertRaises(mi.BuildError) as e:
            mi.fix_glue(self.DEBUGGER_GLUE, [], tests, {"probe": ("adapters", "pub(super)")},
                        "debugger", self.NESTED)
        self.assertIn("probe is used outside debugger but is pub(super) in adapters.rs",
                      str(e.exception))

    def test_a_re_export_of_any_spelling_is_found_again(self):
        """The re-exports a nested split writes (`pub(super)`, `pub(in ...)`)
        are found again when the app build then finds one unused, so it gets
        `#[cfg(test)]` like any other."""
        for vis in ("pub(super)", "pub(in crate::app)", "pub(crate)", "pub"):
            glue = ["mod adapters;", "use adapters::*;", f"{vis} use adapters::lldb_dap_rank;"]
            with self.subTest(vis=vis):
                self.assertEqual(mi.reexport_line(glue, "adapters", "lldb_dap_rank"), 2)
                app = [("unused_imports", "unused import: `adapters::lldb_dap_rank`")]
                lines, _ = mi.fix_glue(glue, app, [], {}, "debugger", self.NESTED)
                self.assertEqual(lines[2:], ["#[cfg(test)]", f"{vis} use adapters::lldb_dap_rank;"])

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
        for t in mi.TARGETS.values():
            rows = mi.read_manifest(root / t.manifest)
            files = {file for pr, file, *_ in rows if pr}
            self.assertEqual(files, set(t.files), t.name)
            for file, (doc, phrase, row) in t.files.items():
                with self.subTest(target=t.name, file=file):
                    self.assertTrue(doc and phrase and row)
                    self.assertTrue(all(len("//! " + ln) <= 100 for ln in doc.split("\n")))


# A file split into a folder beside it (the editor), its tests inline.
EDITOR = """use std::fmt;

pub struct Editor {
    n: u8,
}

impl Editor {
    fn bump(&mut self) -> u8 {
        self.n + 1
    }

    pub fn shown(&self) -> u8 {
        self.n
    }
}

/// The editor's tests.
#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "a
    b";

    #[test]
    fn bumps() {
        let mut e = Editor { n: 1 };
        assert_eq!(e.bump(), 2);
        let s = r#"
    kept as written
"#;
        assert!(s.contains("kept") && TEXT.ends_with("    b"));
    }
}
"""

WIDGETS = """# Architecture

```
src/
└── widgets/
    ├── mod.rs
    ├── editor.rs             The editor.
    ├── editor_find.rs        Find.
```
"""

# A target like the editor's, with the files a test moves items to.
SPLIT = mi.EDITOR._replace(files={
    "undo": ("Undo and redo.", "editor undo history", "undo and redo"),
    "brackets": ("Bracket matching.", "editor bracket", "bracket matching"),
})


class OtherTargets(unittest.TestCase):
    """The editor target: a file split into a folder beside it, and its inline
    tests moved to their own file first."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        for rel, text in {
            SPLIT.source: EDITOR,
            mi.ARCH: WIDGETS,
            SPLIT.manifest: manifest(EDITOR, {"bump": (1, "undo")}),
            os.path.join(mi.NOTES, ".keep"): "",
        }.items():
            (self.tmp / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.tmp / rel).write_text(text)
        git(self.tmp, "init", "-q", "-b", "main")
        git(self.tmp, "config", "user.email", "t@example.com")
        git(self.tmp, "config", "user.name", "t")
        self.root = Root.__new__(Root)
        self.root.path = self.tmp
        self.root.commit()
        git(self.tmp, "checkout", "-q", "-b", "work")

    def tests_out(self):
        return quiet(mi.main, ["--target", "editor", "--tests-out", "--root", str(self.tmp), "--no-fmt"])

    def test_the_inline_tests_move_to_their_own_file_and_pass_the_pure_move_check(self):
        code, out = self.tests_out()
        self.assertEqual(code, 0, out)
        self.assertIn("from src/widgets/editor.rs to src/widgets/editor/tests.rs", out)
        self.assertTrue(self.root.read(SPLIT.source).endswith(
            "/// The editor's tests.\n#[cfg(test)]\nmod tests;\n"))
        tests = self.root.read("src/widgets/editor/tests.rs")
        self.assertTrue(tests.startswith(
            "//! The editor's own tests, moved out of editor.rs (#2062).\n\nuse super::*;\n"))
        self.assertIn("\n#[test]\nfn bumps() {\n    let mut e", tests, "dedented one level")
        self.root.commit()
        code, report = self.root.pure()
        self.assertEqual(code, 0, report)
        self.assertIn("Pure move", report)

    def test_lines_inside_a_string_keep_their_indentation(self):
        self.tests_out()
        tests = self.root.read("src/widgets/editor/tests.rs")
        self.assertIn('const TEXT: &str = "a\n    b";', tests, "the string's value is unchanged")
        self.assertIn('let s = r#"\n    kept as written\n"#;', tests)

    def test_the_tests_get_a_release_note_and_a_row_under_the_new_folder(self):
        self.tests_out()
        note = self.root.read(os.path.join(mi.NOTES, "2062-split-editor-tests.md"))
        self.assertIn("the editor's tests moved to their own file, src/widgets/editor/tests.rs.", note)
        arch = self.root.read(mi.ARCH)
        self.assertRegex(arch, r"editor\.rs +The editor\.\n    ├── editor/ +the editor's feature "
                               r"files.*\n    │   ├── tests\.rs +the editor's own tests\n    ├── editor_find")

    def test_another_named_test_module_moves_to_a_file_of_its_name(self):
        """A file with a second inline test module (remote.rs's
        `offer_tests`) moves it to <folder>/<name>.rs: same module name and
        place, so its `use super::*;` still reaches the split file."""
        second = EDITOR + ("\n/// The offer's tests.\n#[cfg(test)]\nmod offer_tests {\n"
                           "    use super::*;\n\n    #[test]\n    fn shows() {\n"
                           "        assert_eq!(Editor { n: 3 }.shown(), 3);\n    }\n}\n")
        (self.tmp / SPLIT.source).write_text(second)
        self.root.commit()
        before = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.tmp, capture_output=True,
                                text=True, check=True).stdout.strip()
        target = SPLIT._replace(more_tests={
            "offer_tests": ("The offer's tests.", "the offer's tests", "the offer's tests")})
        moved, out = quiet(mi.tests_out, str(self.tmp), target, False, "offer_tests")
        self.assertEqual(moved, 6, out)
        self.assertIn("to src/widgets/editor/offer_tests.rs", out)
        self.assertTrue(self.root.read(SPLIT.source).endswith(
            "/// The offer's tests.\n#[cfg(test)]\nmod offer_tests;\n"))
        self.assertIn("mod tests {", self.root.read(SPLIT.source), "the other module stays")
        offer = self.root.read("src/widgets/editor/offer_tests.rs")
        self.assertTrue(offer.startswith("//! The offer's tests.\n\nuse super::*;\n\n#[test]\nfn shows() {\n"))
        note = self.root.read(os.path.join(mi.NOTES, "2062-split-editor-offer-tests.md"))
        self.assertIn("the offer's tests moved to their own file, src/widgets/editor/offer_tests.rs.", note)
        self.assertRegex(self.root.read(mi.ARCH), r"│   ├── offer_tests\.rs +the offer's tests\n")
        self.root.commit()
        code, report = self.root.pure(before)
        self.assertEqual(code, 0, report)

    def test_a_test_module_with_a_visibility_moves_and_keeps_it(self):
        """dap/session.rs's tests are `pub(crate) mod tests`, for the app's
        tests that borrow its fake adapters: the module moves all the same,
        and its declaration keeps the visibility, so those paths still
        reach it."""
        shared = EDITOR.replace("\nmod tests {", "\npub(crate) mod tests {")
        self.assertNotEqual(shared, EDITOR)
        (self.tmp / SPLIT.source).write_text(shared)
        self.root.commit()
        before = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.tmp, capture_output=True,
                                text=True, check=True).stdout.strip()
        code, out = self.tests_out()
        self.assertEqual(code, 0, out)
        self.assertTrue(self.root.read(SPLIT.source).endswith(
            "/// The editor's tests.\n#[cfg(test)]\npub(crate) mod tests;\n"))
        self.assertIn("\n#[test]\nfn bumps() {\n", self.root.read("src/widgets/editor/tests.rs"))
        self.root.commit()
        code, report = self.root.pure(before)
        self.assertEqual(code, 0, report)

    def test_a_test_module_the_target_has_no_words_for_is_refused(self):
        with self.assertRaises(mi.ManifestError) as e:
            mi.tests_out(str(self.tmp), SPLIT, False, "offer_tests")
        self.assertIn("no words for its offer_tests module", str(e.exception))

    def test_a_file_without_one_tests_block_is_refused(self):
        (self.tmp / SPLIT.source).write_text(EDITOR.split("/// The editor's tests.")[0])
        code, out = self.tests_out()
        self.assertEqual(code, 1)
        self.assertIn("0 top-level `mod tests {` blocks", out)

    def test_a_docs_tree_without_the_files_row_stops_tests_out_before_anything_is_written(self):
        """The tests' docs row goes under the split file's row: with no such
        row the tool must stop before it writes anything, not after it has
        moved the tests and emptied the docs."""
        no_row = WIDGETS.replace("    ├── editor.rs             The editor.\n", "")
        (self.tmp / mi.ARCH).write_text(no_row)
        code, out = self.tests_out()
        self.assertEqual(code, 1)
        self.assertIn("has no row starting '├── editor.rs'", out)
        self.assertEqual(self.root.read(mi.ARCH), no_row)
        self.assertEqual(self.root.read(SPLIT.source), EDITOR)
        self.assertFalse((self.tmp / "src/widgets/editor/tests.rs").exists())
        self.assertFalse((self.tmp / mi.NOTES / "2062-split-editor-tests.md").exists())

    def test_a_docs_tree_without_the_files_row_stops_a_move_before_anything_is_written(self):
        no_row = WIDGETS.replace("    ├── editor.rs             The editor.\n", "")
        (self.tmp / mi.ARCH).write_text(no_row)
        with self.assertRaises(mi.ManifestError):
            quiet(mi.move, str(self.tmp), 1, False, False, SPLIT)
        self.assertEqual(self.root.read(mi.ARCH), no_row)
        self.assertEqual(self.root.read(SPLIT.source), EDITOR)
        self.assertFalse((self.tmp / "src/widgets/editor").exists())

    def test_a_move_goes_to_a_file_in_the_folder_beside_the_split_file(self):
        quiet(mi.move, str(self.tmp), 1, False, False, SPLIT)
        undo = self.root.read("src/widgets/editor/undo.rs")
        self.assertTrue(undo.startswith("//! Undo and redo.\n\nuse super::*;\n\nimpl Editor {\n"))
        self.assertIn("    pub(super) fn bump(&mut self) -> u8 {", undo)
        editor = self.root.read(SPLIT.source)
        self.assertTrue(editor.startswith("use std::fmt;\n\nmod undo;\n\npub struct Editor"),
                        "the first `mod` goes after the `use` lines at the top")
        self.assertNotIn("fn bump(", editor)
        note = self.root.read(os.path.join(mi.NOTES, "2062-split-editor-undo.md"))
        self.assertIn("the editor undo history code moved to its own file, "
                      "src/widgets/editor/undo.rs.", note)
        self.root.commit()
        code, report = self.root.pure()
        self.assertEqual(code, 0, report)

    def test_rows_under_a_folder_go_in_name_order_and_the_folder_row_once(self):
        once = mi.add_arch_row(WIDGETS, "undo", SPLIT)
        twice = mi.add_arch_row(once, "brackets", SPLIT)
        self.assertEqual(twice.count("├── editor/"), 1)
        self.assertRegex(twice, r"├── editor/ .*\n    │   ├── brackets\.rs +bracket matching\n"
                                r"    │   ├── undo\.rs +undo and redo\n    ├── editor_find")
        self.assertEqual(mi.add_arch_row(twice, "undo", SPLIT), twice)

    def test_a_row_another_folder_has_does_not_stop_this_folder_getting_it(self):
        # editor/ and terminal/ list their files with the same indentation:
        # editor/ having an undo.rs row must not keep terminal/ from one.
        widgets = WIDGETS.replace("    ├── editor_find.rs        Find.\n",
                                  "    ├── editor_find.rs        Find.\n"
                                  "    ├── terminal.rs           The terminal.\n")
        term = mi.TERMINAL._replace(files={"undo": ("Undo.", "terminal undo", "terminal undo")})
        both = mi.add_arch_row(mi.add_arch_row(widgets, "undo", SPLIT), "undo", term)
        self.assertRegex(both, r"├── editor/ .*\n    │   ├── undo\.rs +undo and redo\n"
                               r"    ├── editor_find")
        self.assertRegex(both, r"├── terminal\.rs .*\n    ├── terminal/ .*\n"
                               r"    │   ├── undo\.rs +terminal undo\n```")
        self.assertEqual(mi.add_arch_row(both, "undo", term), both)

    def test_every_target_names_its_own_manifest_file_and_folder(self):
        self.assertEqual(
            sorted(mi.TARGETS),
            ["app", "cli", "codeql", "dap_configs", "dap_session", "debugger", "editor", "file_tree",
             "git", "language_servers", "lsp", "pair", "remote", "search", "session_host",
             "source_control", "terminal", "test_worker"],
        )
        for t in mi.TARGETS.values():
            with self.subTest(target=t.name):
                self.assertTrue(t.source.endswith(".rs") and t.manifest.endswith(".tsv"))
                self.assertFalse(t.dir.endswith("/mod"), "never foo/mod.rs (#2062 guardrail 7)")

    def test_no_file_is_named_after_a_crate_or_a_keyword(self):
        # A module named after a crate shadows it in the file it splits, so the
        # moved code's `trash::delete` would look in the new module and fail to
        # build; a keyword (`loop`, `move`) cannot name a module at all.
        import tomllib

        cargo = tomllib.loads((Path(__file__).resolve().parents[2] / "Cargo.toml").read_text())
        tables = [cargo, *cargo.get("target", {}).values()]
        crates = {name.replace("-", "_") for t in tables
                  for kind in ("dependencies", "dev-dependencies", "build-dependencies")
                  for name in t.get(kind, {})}
        self.assertIn("trash", crates)
        keywords = set(
            "as async await break const continue crate dyn else enum extern false fn for gen if "
            "impl in let loop match mod move mut pub ref return self Self static struct super "
            "trait true try type unsafe use where while abstract become box do final macro "
            "override priv typeof unsized virtual yield".split())
        for t in mi.TARGETS.values():
            for name in [*t.files, *t.more_tests]:
                with self.subTest(target=t.name, file=name):
                    self.assertFalse(name in crates, f"`{name}` shadows the crate of that name")
                    self.assertFalse(name in keywords, f"`{name}` is a keyword")


if __name__ == "__main__":
    unittest.main()
