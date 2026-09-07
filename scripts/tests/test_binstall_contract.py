"""The binstall contract check must FAIL on each drift it claims to catch.

A gate that only ever passes is indistinguishable from no gate, and this one
guards a failure that reaches users at install time rather than CI. So every
case below breaks one half of the contract and asserts the checker rejects it,
plus one control proving the unmodified pair passes.

The checker reads Cargo.toml and .github/workflows/release.yml from the repo
root, so each test copies both into a temp tree, mutates one, and runs the
checker against that copy.
"""

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CHECKER = ROOT / "scripts" / "check_binstall_contract.py"


class BinstallContract(unittest.TestCase):
    def build_tree(self, manifest_sub=None, workflow_sub=None, drop_workflow=False):
        """A temp repo with both files, optionally mutated."""
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        (tmp / "scripts").mkdir()
        (tmp / ".github" / "workflows").mkdir(parents=True)
        shutil.copy(CHECKER, tmp / "scripts" / CHECKER.name)

        manifest = (ROOT / "Cargo.toml").read_text()
        if manifest_sub:
            old, new = manifest_sub
            self.assertIn(old, manifest, "manifest fixture anchor is stale")
            manifest = manifest.replace(old, new, 1)
        (tmp / "Cargo.toml").write_text(manifest)

        if not drop_workflow:
            workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text()
            if workflow_sub:
                old, new = workflow_sub
                self.assertIn(old, workflow, "workflow fixture anchor is stale")
                workflow = workflow.replace(old, new, 1)
            (tmp / ".github" / "workflows" / "release.yml").write_text(workflow)
        return tmp

    def run_checker(self, tree):
        return subprocess.run(
            [sys.executable, str(tree / "scripts" / CHECKER.name)],
            capture_output=True,
            text=True,
        )

    def test_the_shipped_pair_agrees(self):
        """The control: without this, every failure below could be vacuous."""
        r = self.run_checker(self.build_tree())
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("agree", r.stdout)

    def test_renaming_the_archive_in_the_workflow_is_caught(self):
        """The drift that 404s every install while CI stays green."""
        tree = self.build_tree(
            workflow_sub=('name="croft-${{ matrix.target }}"',
                          'name="croftbin-${{ matrix.target }}"')
        )
        r = self.run_checker(tree)
        self.assertEqual(r.returncode, 1)
        self.assertIn("binstall would 404", r.stderr)

    def test_a_bin_dir_pointing_at_the_wrong_folder_is_caught(self):
        """binstall would unpack the archive and look in a folder that is not
        in it, reporting a corrupt download rather than a config error."""
        tree = self.build_tree(
            manifest_sub=('bin-dir = "croft-{ target }/{ bin }{ binary-ext }"',
                          'bin-dir = "bin/{ bin }{ binary-ext }"')
        )
        r = self.run_checker(tree)
        self.assertEqual(r.returncode, 1)
        self.assertIn("does not match the archive name", r.stderr)

    def test_a_pkg_fmt_that_contradicts_the_extension_is_caught(self):
        tree = self.build_tree(
            manifest_sub=('pkg-fmt = "tgz"', 'pkg-fmt = "zip"')
        )
        r = self.run_checker(tree)
        self.assertEqual(r.returncode, 1)
        self.assertIn("pkg-fmt", r.stderr)

    def test_dropping_a_target_from_the_workflow_is_caught(self):
        """A release that carries three archives while the manifest promises
        four resolves for some users and 404s for others."""
        tree = self.build_tree(
            workflow_sub=("          - target: aarch64-apple-darwin\n", "")
        )
        r = self.run_checker(tree)
        self.assertEqual(r.returncode, 1)
        self.assertIn("aarch64-apple-darwin", r.stderr)

    def test_losing_the_tag_trigger_is_caught(self):
        """pkg-url hard-codes the v-prefixed tag; without the trigger no
        release is ever produced at that URL."""
        tree = self.build_tree(workflow_sub=('      - "v*"\n', ""))
        r = self.run_checker(tree)
        self.assertEqual(r.returncode, 1)
        self.assertIn("v* tags", r.stderr)

    def test_a_missing_workflow_is_caught(self):
        """Deleting release.yml while Cargo.toml still promises binstall."""
        r = self.run_checker(self.build_tree(drop_workflow=True))
        self.assertEqual(r.returncode, 1)
        self.assertIn("missing", r.stderr)


if __name__ == "__main__":
    unittest.main()
