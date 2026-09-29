"""publish-crate.yml must wire scripts/publish_cadence.py up the way the
cadence promises.

A cadence nothing enforces is a suggestion, so this reads the workflow
itself: it runs on every push to main, publishes every 50 bumps, gates its
publish job on the script's verdict, hands the one write-capable token only
to that job, and dispatches release.yml on the tag, which release.yml must
therefore accept.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "publish-crate.yml"
RELEASE = ROOT / ".github" / "workflows" / "release.yml"


class WorkflowContract(unittest.TestCase):
    def setUp(self):
        self.text = WORKFLOW.read_text()

    def test_cadence_is_fifty(self):
        self.assertRegex(self.text, r"(?m)^\s*PUBLISH_EVERY:\s*50\s*$")

    def test_runs_on_every_push_to_main(self):
        self.assertRegex(self.text, r"(?s)on:\s*\n\s*push:\s*\n\s*branches:\s*\n\s*- main")

    def test_publish_job_is_gated_on_the_script_verdict(self):
        self.assertIn("if: needs.decide.outputs.publish == 'true'", self.text)
        self.assertIn("scripts/publish_cadence.py", self.text)

    def test_only_the_publish_job_can_mint_a_token_or_write(self):
        # Job headers sit at two-space indent on their own line; a bare
        # substring split would also match the `publish:` output name
        # inside the decide job.
        top, rest = re.split(r"(?m)^jobs:$", self.text)
        decide, publish = re.split(r"(?m)^  publish:$", rest)
        self.assertRegex(decide, r"(?m)^  decide:$")
        self.assertIn("contents: read", top)
        self.assertNotIn("id-token", top)
        self.assertNotIn("id-token", decide)
        self.assertNotIn("contents: write", decide)
        self.assertIn("id-token: write", publish)

    def test_release_workflow_can_be_dispatched_on_the_tag(self):
        # The tag is pushed with the workflow token, which never triggers
        # another workflow, so release.yml is dispatched by name on the tag
        # ref instead and must accept a dispatch.
        self.assertIn("gh workflow run release.yml --ref", self.text)
        self.assertRegex(RELEASE.read_text(), r"(?m)^\s*workflow_dispatch:")


if __name__ == "__main__":
    unittest.main()
