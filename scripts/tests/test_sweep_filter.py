"""The scheduled sweep's jq filter, run against fixtures.

The filter decides which open issues get a vetting run dispatched. It has
been wrong once already (a null author compared unequal to the owner and
slipped through), so the exact program in the workflow file is extracted
and exercised here rather than trusted.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import unittest
from pathlib import Path

WORKFLOW = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "issue-vetting.yml"


def sweep_filter() -> str:
    text = WORKFLOW.read_text()
    m = re.search(r"jq -r --arg owner \"\$GITHUB_REPOSITORY_OWNER\" '(.*?)'", text, re.S)
    assert m, "the sweep's jq program was not found in the workflow"
    return m.group(1)


def issue(number: int, author, labels=()) -> dict:
    return {
        "number": number,
        "author": author,
        "labels": [{"name": name} for name in labels],
    }


@unittest.skipUnless(shutil.which("jq"), "jq is not installed")
class SweepFilter(unittest.TestCase):
    def run_filter(self, issues) -> list[str]:
        out = subprocess.run(
            ["jq", "-r", "--arg", "owner", "vitali87", sweep_filter()],
            input=json.dumps(issues),
            capture_output=True,
            text=True,
            check=True,
        )
        return out.stdout.split()

    def test_only_clean_external_issues_are_dispatched(self):
        issues = [
            issue(1, None),  # deleted account: no login to attribute
            issue(2, {"login": ""}),
            issue(3, {"login": "vitali87"}),  # the owner needs no vetting
            issue(4, {"login": "someone"}, ["ready"]),
            issue(5, {"login": "someone"}, ["needs-human"]),
            issue(6, {"login": "someone"}, ["rejected"]),
            issue(7, {"login": "someone"}, ["bug"]),
            issue(8, {"login": "other"}),
        ]
        self.assertEqual(self.run_filter(issues), ["7", "8"])

    def test_an_empty_list_dispatches_nothing(self):
        self.assertEqual(self.run_filter([]), [])
