"""Tests for scripts/publish_cadence.py and the workflow that runs it.

croft bumps its version on every shipped merge, so "publish every N bumps" is
a count of the versions main has carried since the newest one crates.io
already holds. The count comes from main's first-parent history and the
crates.io index, never from a counter stored in the repo, so a run can be
repeated and always reaches the same answer.

The workflow that runs the script is checked by test_publish_crate_workflow.py.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import publish_cadence as pc  # noqa: E402

CLI = ROOT / "scripts" / "publish_cadence.py"


def index(*versions: str, yanked: tuple[str, ...] = ()) -> str:
    """The crates.io sparse index for a crate: one JSON object per line."""
    return "".join(
        json.dumps({"name": "croft-software", "vers": v, "yanked": v in yanked})
        + "\n"
        for v in versions
    )


class ParseHistory(unittest.TestCase):
    def test_keeps_order_and_drops_repeats(self):
        text = "0.2.3\n0.2.2\n0.2.3\n0.1.1251\n\n"
        self.assertEqual(pc.parse_history(text), ["0.2.3", "0.2.2", "0.1.1251"])

    def test_refuses_a_malformed_version(self):
        with self.assertRaisesRegex(pc.CadenceError, "malformed"):
            pc.parse_history("0.2.3\n0.2.x\n")

    def test_refuses_an_empty_history(self):
        with self.assertRaisesRegex(pc.CadenceError, "empty"):
            pc.parse_history("\n")


class ParseIndex(unittest.TestCase):
    def test_reads_every_version_including_yanked(self):
        # A yanked version was still a publish event, so it still anchors
        # the count: the cadence measures publishes, not live versions.
        got = pc.parse_index(index("0.1.942", "0.2.3", yanked=("0.2.3",)))
        self.assertEqual(got, {"0.1.942", "0.2.3"})

    def test_refuses_a_line_that_is_not_json(self):
        with self.assertRaisesRegex(pc.CadenceError, "index"):
            pc.parse_index('{"vers": "0.1.1"}\nnot json\n')

    def test_refuses_an_entry_without_a_version(self):
        with self.assertRaisesRegex(pc.CadenceError, "vers"):
            pc.parse_index('{"name": "croft-software"}\n')

    def test_refuses_an_empty_index(self):
        with self.assertRaisesRegex(pc.CadenceError, "no versions"):
            pc.parse_index("")


class IndexPath(unittest.TestCase):
    def test_sparse_index_layout(self):
        self.assertEqual(pc.index_path("croft-software"), "cr/of/croft-software")
        self.assertEqual(pc.index_path("a"), "1/a")
        self.assertEqual(pc.index_path("ab"), "2/ab")
        self.assertEqual(pc.index_path("abc"), "3/a/abc")

    def test_refuses_a_name_that_is_not_a_crate_name(self):
        with self.assertRaisesRegex(pc.CadenceError, "crate name"):
            pc.index_path("../etc")


class BumpsSince(unittest.TestCase):
    HISTORY = ["0.2.3", "0.2.2", "0.2.1", "0.1.1251", "0.1.1249"]

    def test_head_already_published_counts_zero(self):
        self.assertEqual(
            pc.bumps_since(self.HISTORY, {"0.2.3"}), (0, "0.2.3")
        )

    def test_counts_versions_after_the_newest_published_one(self):
        # 0.2.1 is the newest published version in main's history; the
        # older 0.1.1249 does not shorten the count.
        self.assertEqual(
            pc.bumps_since(self.HISTORY, {"0.2.1", "0.1.1249"}), (2, "0.2.1")
        )

    def test_refuses_a_history_with_no_published_version(self):
        # A shallow clone, or a crate whose published versions never went
        # through main, has no anchor: guessing "publish" or "skip" here is
        # the failure the anchor exists to prevent.
        with self.assertRaisesRegex(pc.CadenceError, "no published version"):
            pc.bumps_since(self.HISTORY, {"0.1.700"})


class Decide(unittest.TestCase):
    def test_boundary_at_every(self):
        self.assertFalse(pc.due(49, 50))
        self.assertTrue(pc.due(50, 50))
        self.assertTrue(pc.due(51, 50))

    def test_refuses_a_cadence_below_one(self):
        with self.assertRaisesRegex(pc.CadenceError, "at least 1"):
            pc.due(3, 0)


def cli(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(CLI), *args],
        capture_output=True,
        text=True,
        check=False,
    )


class Cli(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.dir)]))
        self.history = self.dir / "history.txt"
        self.index = self.dir / "index.jsonl"
        self.history.write_text("0.2.5\n0.2.4\n0.2.3\n0.2.2\n")
        self.index.write_text(index("0.2.2", "0.1.942"))

    def run_cli(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return cli(
            "--history", str(self.history),
            "--published", str(self.index),
            "--head", "0.2.5",
            *extra,
        )

    def outputs(self, proc: subprocess.CompletedProcess[str]) -> dict[str, str]:
        return dict(line.split("=", 1) for line in proc.stdout.splitlines())

    def test_reports_the_count_and_publishes_at_the_cadence(self):
        proc = self.run_cli("--every", "3")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(
            self.outputs(proc),
            {
                "version": "0.2.5",
                "latest_published": "0.2.2",
                "bumps": "3",
                "already_published": "false",
                "publish": "true",
            },
        )

    def test_below_the_cadence_is_a_no_with_exit_zero(self):
        proc = self.run_cli("--every", "4")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self.outputs(proc)["publish"], "false")
        self.assertEqual(self.outputs(proc)["bumps"], "3")

    def test_force_publishes_below_the_cadence(self):
        proc = self.run_cli("--every", "50", "--force")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self.outputs(proc)["publish"], "true")

    def test_force_still_reports_an_already_published_head(self):
        self.index.write_text(index("0.2.5"))
        proc = self.run_cli("--every", "50", "--force")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        got = self.outputs(proc)
        self.assertEqual(got["already_published"], "true")
        self.assertEqual(got["bumps"], "0")

    def test_refuses_a_head_the_history_does_not_start_with(self):
        # A history whose first entry is not Cargo.toml's version was read
        # from the wrong commit or a truncated log; its count means nothing.
        proc = cli(
            "--history", str(self.history),
            "--published", str(self.index),
            "--head", "0.2.6",
            "--every", "3",
        )
        self.assertEqual(proc.returncode, 2)
        self.assertIn("0.2.6", proc.stderr)
        self.assertEqual(proc.stdout, "")

    def test_refuses_when_no_published_version_is_in_history(self):
        self.index.write_text(index("0.1.942"))
        proc = self.run_cli("--every", "3")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("no published version", proc.stderr)
        self.assertEqual(proc.stdout, "")


if __name__ == "__main__":
    unittest.main()
