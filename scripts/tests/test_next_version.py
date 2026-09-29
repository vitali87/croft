"""Tests for scripts/next_version.py and the release gate that enforces it.

croft's versions count like an odometer, the same rule code-graph-rag's
version-bump workflow uses: MINOR and PATCH stay below 1000, and a bump that
reaches 1000 carries into the next component. The rollover table mirrors
code-graph-rag's `test_version_bump_rollover.py`, plus croft's own starting
point (0.1.1244, already past the cap when the rule arrived).

A rule nothing enforces is a suggestion, so the last class runs the gate step
itself, extracted from ci.yml, against a throwaway repo, and asserts it
refuses a bump past the cap and accepts the carried version.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import next_version as nv  # noqa: E402

CLI = ROOT / "scripts" / "next_version.py"
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
GATE_STEP = "- name: Shipped changes must bump the version and replace the notes"


def cli(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(CLI), *args],
        capture_output=True,
        text=True,
        check=False,
    )


class Rollover(unittest.TestCase):
    def test_the_cap_is_one_thousand(self):
        self.assertEqual(nv.CAP, 1000)

    def test_components_roll_over_at_the_cap(self):
        cases = [
            ("0.0.5", "patch", "0.0.6"),
            ("0.0.998", "patch", "0.0.999"),
            ("0.0.999", "patch", "0.1.0"),
            ("0.0.1006", "patch", "0.1.0"),
            ("0.1.0", "patch", "0.1.1"),
            ("0.999.999", "patch", "1.0.0"),
            ("0.1500.3", "patch", "1.0.0"),
            ("0.998.7", "minor", "0.999.0"),
            ("0.999.7", "minor", "1.0.0"),
            ("0.999.7", "major", "1.0.0"),
            ("0.0.1006", "minor", "0.1.0"),
            # croft's own starting point: already past the cap.
            ("0.1.1244", "patch", "0.2.0"),
            ("0.1.1244", "minor", "0.2.0"),
            ("0.2.0", "patch", "0.2.1"),
        ]
        for current, kind, expected in cases:
            with self.subTest(current=current, kind=kind):
                self.assertEqual(nv.bump(current, kind), expected)

    def test_patch_is_the_default_bump(self):
        self.assertEqual(nv.bump("0.2.0"), "0.2.1")

    def test_within_cap(self):
        self.assertTrue(nv.within_cap("0.2.0"))
        self.assertTrue(nv.within_cap("0.999.999"))
        self.assertTrue(nv.within_cap("1000.0.0"), "MAJOR has no cap")
        self.assertFalse(nv.within_cap("0.1.1000"))
        self.assertFalse(nv.within_cap("0.1.1244"))
        self.assertFalse(nv.within_cap("0.1000.0"))

    def test_malformed_versions_are_refused(self):
        for bad in ["0.0", "0.0.1a", "0.0.1\n0.0.2", "0.0.1\n", "v0.1.0", "", "0.0.$(touch x)", "0.0.١"]:
            with self.subTest(bad=bad):
                with self.assertRaisesRegex(ValueError, "refusing malformed version"):
                    nv.bump(bad)
                with self.assertRaisesRegex(ValueError, "refusing malformed version"):
                    nv.within_cap(bad)

    def test_an_unknown_bump_type_is_refused(self):
        with self.assertRaisesRegex(ValueError, "unknown bump type"):
            nv.bump("0.0.1", "patch; touch /tmp/x")


class Cli(unittest.TestCase):
    def test_prints_the_next_version(self):
        out = cli("0.1.1244")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.strip(), "0.2.0")
        self.assertEqual(cli("0.2.5", "minor").stdout.strip(), "0.3.0")

    def test_check_reports_through_its_exit_status(self):
        self.assertEqual(cli("--check", "0.2.0").returncode, 0)
        refused = cli("--check", "0.1.1245")
        self.assertEqual(refused.returncode, 1)
        self.assertIn("at or above 1000", refused.stderr)

    def test_refused_input_exits_2(self):
        self.assertEqual(cli("--check", "0.1").returncode, 2)
        self.assertEqual(cli("0.1.0", "sideways").returncode, 2)
        self.assertEqual(cli().returncode, 2)
        self.assertEqual(cli("--bogus").returncode, 2)


def gate_script() -> str:
    """The release gate's `run:` block, exactly as ci.yml has it."""
    lines = WORKFLOW.read_text().splitlines()
    start = next(i for i, line in enumerate(lines) if line.strip() == GATE_STEP)
    run_at = next(i for i in range(start, len(lines)) if lines[i].strip() == "run: |")
    run_indent = len(lines[run_at]) - len(lines[run_at].lstrip())
    body = []
    for line in lines[run_at + 1 :]:
        if line.strip() and len(line) - len(line.lstrip()) <= run_indent:
            break
        body.append(line)
    body_indent = min(len(l) - len(l.lstrip()) for l in body if l.strip())
    script = "\n".join(l[body_indent:] for l in body)
    assert "next_version.py --check" in script, "the gate no longer runs the cap check"
    return script


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


@unittest.skipUnless(shutil.which("bash") and shutil.which("git"), "needs bash and git")
class ReleaseGate(unittest.TestCase):
    """The gate, run on a repo whose PR bumps `base` to `head`."""

    def run_gate(self, base: str, head: str) -> subprocess.CompletedProcess[str]:
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        git(tmp, "init", "-q", "-b", "main")
        git(tmp, "config", "user.email", "t@example.com")
        git(tmp, "config", "user.name", "t")
        (tmp / "scripts").mkdir()
        for name in ["ships_nothing.py", "check_doc_ownership.py", "next_version.py"]:
            shutil.copy(ROOT / "scripts" / name, tmp / "scripts" / name)
        (tmp / "src" / "release_notes").mkdir(parents=True)

        def manifest(version: str) -> str:
            return f'[package]\nname = "croft-software"\nversion = "{version}"\n'

        (tmp / "Cargo.toml").write_text(manifest(base))
        (tmp / "src" / "lib.rs").write_text("pub fn f() -> u8 {\n    1\n}\n")
        git(tmp, "add", "-A")
        git(tmp, "commit", "-q", "-m", "base")
        git(tmp, "update-ref", "refs/remotes/origin/main", "HEAD")

        # A shipped change, the bump, and this version's notes.
        (tmp / "src" / "lib.rs").write_text("pub fn f() -> u8 {\n    2\n}\n")
        (tmp / "Cargo.toml").write_text(manifest(head))
        (tmp / "src" / "release_notes" / f"{head}.md").write_text("fix: f returns 2.\n")
        git(tmp, "add", "-A")
        git(tmp, "commit", "-q", "-m", "head")

        env = {**os.environ, "GITHUB_BASE_REF": "main"}
        return subprocess.run(
            ["bash", "-c", gate_script()],
            cwd=tmp,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_a_patch_bump_past_the_cap_is_refused(self):
        out = self.run_gate("0.1.1244", "0.1.1245")
        self.assertNotEqual(out.returncode, 0, out.stdout)
        self.assertIn("Version 0.1.1245 has a component at or above 1000", out.stdout)
        self.assertIn("the next version after 0.1.1244 is 0.2.0", out.stdout)

    def test_the_carried_version_is_accepted(self):
        out = self.run_gate("0.1.1244", "0.2.0")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)

    def test_a_minor_at_the_cap_is_refused(self):
        out = self.run_gate("0.999.5", "0.1000.0")
        self.assertNotEqual(out.returncode, 0, out.stdout)
        self.assertIn("at or above 1000", out.stdout)

    def test_an_ordinary_bump_is_accepted(self):
        out = self.run_gate("0.2.0", "0.2.1")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)


if __name__ == "__main__":
    unittest.main()
