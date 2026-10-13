"""publish-crate.yml must wire scripts/publish_cadence.py up the way the
cadence promises.

A cadence nothing enforces is a suggestion, so this reads the workflow
itself: it runs on every push to main, publishes every 50 bumps, gates its
publish job on the script's verdict, hands the one write-capable token only
to that job, and dispatches release.yml on the tag, which release.yml must
therefore accept. It is the only workflow that starts a release, so crates.io
and the GitHub releases hold the same versions (#1579).
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "publish-crate.yml"
RELEASE = ROOT / ".github" / "workflows" / "release.yml"
TAG = ROOT / ".github" / "workflows" / "tag.yml"


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

    def test_decide_refuses_a_ref_other_than_main(self):
        # A manual dispatch can name any branch; a forced run there would
        # publish that branch's code under the crate's identity.
        self.assertIn("if: github.ref != 'refs/heads/main'", self.text)

    def test_decide_tells_the_script_whether_the_release_exists(self):
        # Without it, a head that was uploaded but never released counts 0
        # bumps and the publish job never runs again to finish the handoff.
        # The tag is no evidence: tag.yml tags every version.
        self.assertIn('flags+=(--released)', self.text)
        self.assertIn('/releases/tags/v$head', self.text)
        self.assertIn('"${flags[@]}"', self.text)
        self.assertNotIn("--tagged", self.text)

    def test_upload_is_skipped_on_a_fresh_index_check(self):
        # decide's already_published goes stale when only the publish job is
        # re-run after an upload that landed, so the upload asks again.
        _, publish = re.split(r"(?m)^  publish:$", self.text)
        self.assertIn("pc.fetch_index(pc.CRATE)", publish)
        self.assertEqual(
            publish.count("if: steps.index.outputs.uploaded != 'true'"), 2
        )
        self.assertNotIn("needs.decide.outputs.already_published", publish)

    def test_an_existing_tag_must_name_the_published_commit(self):
        # release.yml builds what the tag points at; a stale tag at another
        # commit would ship binaries that are not the crate just published.
        self.assertIn('"refs/tags/$tag^{}"', self.text)
        self.assertIn('if [ "$target" != "$commit" ]; then', self.text)

    def test_the_commit_that_set_the_version_is_published_and_tagged(self):
        # The run's own commit can be a merge that landed after the version
        # was cut, whose code is not that version yet. tag.yml tags the
        # release commit, so publishing anything else would put code on
        # crates.io that the tag, and the binaries built from it, do not
        # have, and the tag step would then refuse the mismatch after the
        # upload.
        _, publish = re.split(r"(?m)^  publish:$", self.text)
        self.assertIn("fetch-depth: 0", publish)
        lookup = publish.index("git log --first-parent -1 --format=%H -G")
        checkout = publish.index('git checkout -q --detach "$commit"')
        upload = publish.index("run: cargo publish --locked")
        self.assertLess(lookup, checkout)
        self.assertLess(checkout, upload)
        self.assertIn("COMMIT: ${{ steps.source.outputs.commit }}", publish)
        self.assertIn('git tag -a "$tag" -m "croft $VERSION" "$commit"', publish)
        self.assertNotIn("GITHUB_SHA", publish)

    def test_tag_yml_tags_but_starts_no_release(self):
        # When tag.yml also dispatched release.yml, every version got a
        # GitHub release and only every fiftieth reached crates.io (#1579).
        tag = TAG.read_text()
        self.assertIn("refs/tags/$tag", tag)
        self.assertNotIn("release.yml/dispatches", tag)
        self.assertNotIn("workflow run", tag)
        self.assertNotIn("actions: write", tag)

    def test_queued_tag_runs_are_kept(self):
        # A group keeps one pending run by default and a newer one replaces
        # it, so a release commit's tag run could be cancelled and its
        # version left untagged.
        self.assertRegex(TAG.read_text(), r"(?m)^concurrency:\n(  .*\n)*  queue: max$")

    def test_the_release_body_covers_every_version_since_the_last_release(self):
        # One release per 50 versions: its body must carry all of their
        # notes, not only the released version's own.
        release = RELEASE.read_text()
        self.assertIn("gh release list", release)
        self.assertIn('python3 scripts/release.py notes "$version"', release)

    def test_release_workflow_can_be_dispatched_on_the_tag(self):
        # The tag is pushed with the workflow token, which never triggers
        # another workflow, so release.yml is dispatched by name on the tag
        # ref instead and must accept a dispatch.
        self.assertIn("gh workflow run release.yml --ref", self.text)
        self.assertRegex(RELEASE.read_text(), r"(?m)^\s*workflow_dispatch:")


if __name__ == "__main__":
    unittest.main()
