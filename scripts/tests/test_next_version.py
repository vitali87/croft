"""Tests for scripts/next_version.py.

croft's versions count like an odometer, the same rule code-graph-rag's
version-bump workflow uses: MINOR and PATCH stay below 1000, and a bump that
reaches 1000 carries into the next component. The rollover table mirrors
code-graph-rag's `test_version_bump_rollover.py`, plus croft's own starting
point (0.1.1244, already past the cap when the rule arrived).

Pull requests no longer pick a version, so the rule is enforced where the
version is chosen: `scripts/release.py cut` takes every number from `bump`,
and scripts/tests/test_release.py runs the version-bump workflow's step
itself against a throwaway repo.
"""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import next_version as nv  # noqa: E402

CLI = ROOT / "scripts" / "next_version.py"


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


if __name__ == "__main__":
    unittest.main()
