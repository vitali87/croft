"""The tag workflow check must FAIL on each weakening it claims to catch.

tag.yml runs on every merge to main with a token that can write tags and start
workflows, so what it executes is pinned: every action by full commit SHA, and
every REST call to a named API version. Each case below breaks one of those
and asserts the checker rejects it; the controls prove the shipped workflow
and an unmutated fixture pass, so no rejection is vacuous.

The checker reads .github/workflows/tag.yml from the repo root, so each test
writes a workflow into a temp tree beside a copy of the checker.
"""

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CHECKER = ROOT / "scripts" / "check_tag_workflow.py"

SHA = "3d3c42e5aac5ba805825da76410c181273ba90b1"

# Pinned and versioned: the fixture every mutation below starts from.
CLEAN = f"""\
on:
  push:
    branches: [main]
jobs:
  tag:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@{SHA} # v7.0.1
      - name: Tag
        env:
          GH_TOKEN: ${{{{ github.token }}}}
        run: |
          set -euo pipefail
          api() {{ gh api -H "X-GitHub-Api-Version: 2026-03-10" "$@"; }}
          api "repos/$GITHUB_REPOSITORY/git/refs" \\
            -f ref="refs/tags/v1" -f sha="$GITHUB_SHA"
"""

TAIL = '            -f ref="refs/tags/v1" -f sha="$GITHUB_SHA"\n'


class TagWorkflow(unittest.TestCase):
    def run_checker(self, workflow=None):
        """Run the checker against `workflow`, or with no tag.yml if None."""
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        (tmp / "scripts").mkdir()
        (tmp / ".github" / "workflows").mkdir(parents=True)
        shutil.copy(CHECKER, tmp / "scripts" / CHECKER.name)
        if workflow is not None:
            (tmp / ".github" / "workflows" / "tag.yml").write_text(workflow)
        return subprocess.run(
            [sys.executable, str(tmp / "scripts" / CHECKER.name)],
            capture_output=True,
            text=True,
        )

    def with_line(self, line):
        """CLEAN with one more script line after its last command."""
        self.assertIn(TAIL, CLEAN, "fixture anchor is stale")
        return CLEAN.replace(TAIL, TAIL + "          " + line + "\n", 1)

    def assert_rejected(self, r, *needles):
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        for needle in needles:
            self.assertIn(needle, r.stderr)

    def test_the_shipped_workflow_passes(self):
        """The control on the real file: CI runs this, so it is the gate."""
        r = self.run_checker((ROOT / ".github" / "workflows" / "tag.yml").read_text())
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("pinned", r.stdout)

    def test_the_clean_fixture_passes(self):
        """Without this, every rejection below could come from the fixture
        rather than from the one line each test changes."""
        r = self.run_checker(CLEAN)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_an_action_not_pinned_to_a_full_sha_is_rejected(self):
        """A tag or branch can be moved to new code after review; an
        abbreviated or over-long SHA is not a commit pin either."""
        for ref in ("v7", "main", SHA[:7], SHA + "0", SHA.upper()):
            with self.subTest(ref=ref):
                r = self.run_checker(CLEAN.replace(SHA, ref, 1))
                self.assert_rejected(r, f"actions/checkout@{ref}")

    def test_a_gh_api_call_without_a_version_is_rejected(self):
        """Unversioned, GitHub answers with its oldest supported version, so
        the response shape changes under the job when that version retires."""
        r = self.run_checker(self.with_line('gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/v1"'))
        self.assert_rejected(r, "X-GitHub-Api-Version", "git/ref/tags/v1")

    def test_a_gh_subcommand_other_than_api_is_rejected(self):
        """`gh workflow run` makes REST calls this workflow cannot version."""
        r = self.run_checker(self.with_line("gh workflow run release.yml --ref v1"))
        self.assert_rejected(r, "`gh workflow` cannot pin an API version")

    def test_one_versioned_call_does_not_cover_another_on_its_line(self):
        """The header belongs to the command it is passed to, not the line,
        whichever of the two carries it."""
        versioned = 'gh api -H "X-GitHub-Api-Version: 2026-03-10" "repos/$GITHUB_REPOSITORY/a"'
        bare = 'gh api "repos/$GITHUB_REPOSITORY/b"'
        for line in (f"{versioned}; {bare}", f"{bare}; {versioned}"):
            with self.subTest(line=line):
                r = self.run_checker(self.with_line(line))
                self.assert_rejected(r, "without a dated", "repos/$GITHUB_REPOSITORY/b")
                self.assertNotIn("repos/$GITHUB_REPOSITORY/a", r.stderr)

    def test_a_malformed_version_is_rejected(self):
        """Only a dated version pins anything; `latest` names no version."""
        r = self.run_checker(
            CLEAN.replace("X-GitHub-Api-Version: 2026-03-10", "X-GitHub-Api-Version: latest", 1)
        )
        self.assert_rejected(r, "X-GitHub-Api-Version")

    def test_a_version_on_a_continuation_line_counts(self):
        """A backslash-continued command is one command, header included."""
        r = self.run_checker(
            self.with_line(
                'gh api "repos/$GITHUB_REPOSITORY/c" \\\n'
                '            -H "X-GitHub-Api-Version: 2026-03-10"'
            )
        )
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_quoted_unpinned_action_is_rejected(self):
        """YAML allows the value quoted; the quote must not hide the ref."""
        r = self.run_checker(CLEAN.replace(f"actions/checkout@{SHA}", '"actions/checkout@v7"', 1))
        self.assert_rejected(r, "actions/checkout@v7")

    def test_a_single_line_run_is_checked(self):
        """`run: <command>` is a script as much as `run: |` is."""
        r = self.run_checker(CLEAN + "      - run: gh workflow run release.yml --ref v1\n")
        self.assert_rejected(r, "`gh workflow` cannot pin an API version")

    def test_text_outside_run_scripts_is_not_a_call(self):
        """A step name is not a command; the script ends where its indent does."""
        r = self.run_checker(CLEAN + "      - name: notify gh release watchers\n        run: echo done\n")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_commented_out_call_is_not_a_call(self):
        r = self.run_checker(self.with_line("# gh workflow run release.yml --ref v1"))
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_word_ending_in_gh_is_not_a_call(self):
        """`enough of` must not read as `gh of`."""
        r = self.run_checker(self.with_line('echo "enough of that"'))
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_missing_workflow_is_rejected(self):
        """A renamed or deleted tag.yml must not read as a clean one. The
        message is asserted, not just the status: a crash on the missing file
        also exits 1, and would pass a status-only test."""
        r = self.run_checker(None)
        self.assert_rejected(r, "tag.yml is missing")


if __name__ == "__main__":
    unittest.main()
